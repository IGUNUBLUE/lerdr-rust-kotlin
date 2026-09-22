//! Device credential + invitation store — the `deviceauth.Store` port.
//!
//! One JSON document holds at most one live invitation plus the enrolled
//! credentials. The semantics mirror `internal/deviceauth`:
//!
//! - **Invitation resolve** checks id + version, expiry (expired invitations
//!   are dropped on read), the 5-attempt burn limit, and the exponential
//!   backoff (`next_attempt_at`) between failed proofs.
//! - **Failed proofs** (`complete(.., false)`) count only for non-bootstrap
//!   invitations — bootstrap secrets are full-entropy, and counting remote
//!   guesses would only deny service to the legitimate first device.
//! - **Invitation redemption** (`complete(.., true)`) is idempotent through
//!   `pending_credential_id`: a handshake that dies after minting re-issues
//!   the same credential on retry, and a second device cannot redeem the
//!   same invitation into a *different* credential. The invitation clears
//!   when the issued credential completes its first credential handshake.
//!
//! Wire-compat with the Go `devices.json` is deliberately NOT maintained —
//! this is a fresh store (`schema_version` guards the format).

use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use base64::Engine;
use lerdr_e2ee::handshake::{AuthKind, AuthSelector, SECRET_BYTES};
use serde::{Deserialize, Serialize};

use crate::auth::{
    AuthError, AuthOutcome, AuthenticatedIdentity, BootstrapRearm, BoxFuture, Credential,
    DeviceAuthStore, IssuedInvitation, Role,
};

/// `storeFilename`.
pub const STORE_FILENAME: &str = "devices.json";
const STORE_SCHEMA_VERSION: u32 = 1;
/// `invitationLifetime` — 10 minutes.
pub const INVITATION_LIFETIME_MS: i64 = 10 * 60 * 1000;
/// `maxInviteAttempts`.
pub const MAX_INVITE_ATTEMPTS: u32 = 5;
/// `bootstrapInvitationID` — the operator-printed first-pairing invitation.
pub const BOOTSTRAP_INVITATION_ID: &str = "bootstrap";
/// `maxNameBytes` — `validateMetadata`'s device/invitation name cap.
const MAX_NAME_BYTES: usize = 80;
/// `maxLocaleBytes` — `validateMetadata`'s locale cap.
const MAX_LOCALE_BYTES: usize = 32;

const IDENTIFIER_BYTES: usize = 18;
/// `io.LimitReader` cap on the stored document.
const MAX_STORE_BYTES: u64 = 4 << 20;

fn b64() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
}

/// `localize.NormalizeLocale` — unsupported/malformed tags fall back to en.
pub fn normalize_locale(value: &str) -> String {
    let tag = value.trim().to_lowercase().replace('_', "-");
    if tag == "zh-cn" || tag.starts_with("zh-cn-") {
        "zh-CN".to_owned()
    } else {
        "en".to_owned()
    }
}

/// `validText` — non-empty, within the byte cap, no control characters
/// (the `utf8.ValidString` arm is free: `&str` is always valid UTF-8).
fn valid_text(value: &str, max_bytes: usize) -> bool {
    !value.is_empty() && value.len() <= max_bytes && !value.chars().any(|c| c.is_control())
}

/// `validateMetadata` — the shared device/invitation metadata check; the
/// trimmed name and locale come back for storage. Error order is the
/// oracle's: name, then role, then locale.
fn validate_metadata(
    name: &str,
    role: &str,
    locale: &str,
) -> Result<(String, Role, String), AuthError> {
    let name = name.trim();
    let locale = locale.trim();
    if !valid_text(name, MAX_NAME_BYTES) {
        return Err(AuthError::InvalidName);
    }
    let role = match role {
        "controller" => Role::Controller,
        "reader" => Role::Reader,
        _ => return Err(AuthError::InvalidRole),
    };
    if !valid_text(locale, MAX_LOCALE_BYTES) || locale.contains([' ', '/', '\\']) {
        return Err(AuthError::InvalidLocale);
    }
    Ok((name.to_owned(), role, locale.to_owned()))
}

/// An invitation record — `invitationRecord`. `secret` is base64
/// RawURLEncoding of the 32-byte pairing secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Invitation {
    pub invitation_id: String,
    pub version: u64,
    pub secret: String,
    /// Unix milliseconds.
    pub expires_at_ms: i64,
    #[serde(default)]
    pub name: String,
    pub role: Role,
    #[serde(default)]
    pub locale: String,
    #[serde(default)]
    pub failed_attempts: u32,
    #[serde(default)]
    pub next_attempt_at_ms: i64,
    #[serde(default)]
    pub pending_credential_id: String,
}

/// `credentialRecord` — a credential plus its pairing secret (b64).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CredentialRecord {
    #[serde(flatten)]
    pub credential: Credential,
    /// Empty once revoked — a revoked credential must not retain a secret.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub secret: String,
}

// Secrets are scrubbed when records drop — a removed invitation or revoked
// credential must not leave its b64 secret in heap memory.
impl Drop for Invitation {
    fn drop(&mut self) {
        zeroize::Zeroize::zeroize(&mut self.secret);
    }
}

impl Invitation {
    /// `invitationFromRecord` — the public shape `CreateInvitation`
    /// returns; the attempt-limit and redemption bookkeeping stays
    /// internal to the store.
    pub fn issued(&self) -> IssuedInvitation {
        IssuedInvitation {
            invitation_id: self.invitation_id.clone(),
            version: self.version,
            secret: self.secret.clone(),
            expires_at_ms: self.expires_at_ms,
            name: self.name.clone(),
            role: self.role,
            locale: self.locale.clone(),
        }
    }
}

impl Drop for CredentialRecord {
    fn drop(&mut self) {
        zeroize::Zeroize::zeroize(&mut self.secret);
    }
}

/// `diskState` — the persisted document. `rearm_bootstrap` is a runtime
/// option, never persisted.
#[derive(Debug, Serialize, Deserialize)]
struct AuthState {
    schema_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    invitation: Option<Invitation>,
    #[serde(default)]
    credentials: Vec<CredentialRecord>,
    #[serde(skip)]
    rearm_bootstrap: bool,
}

impl Default for AuthState {
    fn default() -> Self {
        AuthState {
            schema_version: STORE_SCHEMA_VERSION,
            invitation: None,
            credentials: Vec::new(),
            rearm_bootstrap: false,
        }
    }
}

impl AuthState {
    fn credential_index(&self, credential_id: &str) -> Option<usize> {
        self.credentials
            .iter()
            .position(|r| r.credential.credential_id == credential_id)
    }

    fn decode_secret(encoded: &str) -> Result<[u8; SECRET_BYTES], AuthError> {
        b64()
            .decode(encoded)
            .ok()
            .and_then(|v| <[u8; SECRET_BYTES]>::try_from(v.as_slice()).ok())
            .ok_or(AuthError::Failed)
    }

    /// `ResolveE2EESecret`. The `bool` marks state mutations — the caller
    /// persists (expired invitations self-heal on read).
    fn resolve(
        &mut self,
        selector: &AuthSelector,
        now_ms: i64,
    ) -> (Result<[u8; SECRET_BYTES], AuthError>, bool) {
        match selector.kind {
            AuthKind::Invitation => self.resolve_invitation(selector, now_ms),
            AuthKind::Credential => (self.resolve_credential(selector), false),
        }
    }

    fn resolve_invitation(
        &mut self,
        selector: &AuthSelector,
        now_ms: i64,
    ) -> (Result<[u8; SECRET_BYTES], AuthError>, bool) {
        let Some(record) = &self.invitation else {
            return (Err(AuthError::Failed), false);
        };
        if record.invitation_id != selector.id || record.version != selector.version {
            return (Err(AuthError::Failed), false);
        }
        let mut mutated = false;
        if now_ms >= record.expires_at_ms {
            if record.invitation_id == BOOTSTRAP_INVITATION_ID
                && (self.credentials.is_empty() || self.rearm_bootstrap)
            {
                // The bootstrap invitation re-arms instead of expiring so
                // first pairing survives a quiet relay.
                self.invitation.as_mut().unwrap().expires_at_ms = now_ms + INVITATION_LIFETIME_MS;
                mutated = true;
            } else {
                self.invitation = None;
                return (Err(AuthError::InvitationExpired), true);
            }
        }
        let record = self.invitation.as_ref().unwrap();
        if record.failed_attempts >= MAX_INVITE_ATTEMPTS {
            return (Err(AuthError::InvitationBurned), mutated);
        }
        if now_ms < record.next_attempt_at_ms {
            return (Err(AuthError::RateLimited), mutated);
        }
        (Self::decode_secret(&record.secret), mutated)
    }

    fn resolve_credential(&self, selector: &AuthSelector) -> Result<[u8; SECRET_BYTES], AuthError> {
        let Some(index) = self.credential_index(&selector.id) else {
            return Err(AuthError::Failed);
        };
        let record = &self.credentials[index];
        if record.credential.revoked {
            return Err(AuthError::Revoked);
        }
        if record.credential.version != selector.version {
            return Err(AuthError::Failed);
        }
        Self::decode_secret(&record.secret)
    }

    /// `CompleteE2EEAuth`.
    fn complete(
        &mut self,
        selector: &AuthSelector,
        authenticated: bool,
        now_ms: i64,
        fill: &mut dyn FnMut(&mut [u8]),
    ) -> (Result<AuthOutcome, AuthError>, bool) {
        if !authenticated {
            return match selector.kind {
                AuthKind::Invitation => self.record_failed_invitation(selector, now_ms),
                AuthKind::Credential => (Err(AuthError::Failed), false),
            };
        }
        match selector.kind {
            AuthKind::Invitation => self.redeem_invitation(selector, now_ms, fill),
            AuthKind::Credential => {
                let result = self.complete_credential(selector, now_ms);
                let mutated = result.is_ok();
                (result, mutated)
            }
        }
    }

    /// `recordFailedInvitationLocked`.
    fn record_failed_invitation(
        &mut self,
        selector: &AuthSelector,
        now_ms: i64,
    ) -> (Result<AuthOutcome, AuthError>, bool) {
        let Some(record) = &self.invitation else {
            return (Err(AuthError::Failed), false);
        };
        if record.invitation_id != selector.id || record.version != selector.version {
            return (Err(AuthError::Failed), false);
        }
        if record.invitation_id == BOOTSTRAP_INVITATION_ID {
            return (Err(AuthError::Failed), false);
        }
        if now_ms >= record.expires_at_ms {
            self.invitation = None;
            return (Err(AuthError::InvitationExpired), true);
        }
        let record = self.invitation.as_mut().unwrap();
        record.failed_attempts += 1;
        if record.failed_attempts >= MAX_INVITE_ATTEMPTS {
            self.invitation = None;
            return (Err(AuthError::InvitationBurned), true);
        }
        // Backoff between attempts: 1s, 2s, 4s, 8s.
        record.next_attempt_at_ms = now_ms + (1000i64 << (record.failed_attempts - 1));
        (Err(AuthError::Failed), true)
    }

    /// `redeemInvitationLocked` — mint or re-issue the pending credential.
    fn redeem_invitation(
        &mut self,
        selector: &AuthSelector,
        now_ms: i64,
        fill: &mut dyn FnMut(&mut [u8]),
    ) -> (Result<AuthOutcome, AuthError>, bool) {
        let Some(record) = &self.invitation else {
            return (Err(AuthError::Failed), false);
        };
        if record.invitation_id != selector.id || record.version != selector.version {
            return (Err(AuthError::Failed), false);
        }
        if now_ms >= record.expires_at_ms {
            self.invitation = None;
            return (Err(AuthError::InvitationExpired), true);
        }
        let pending = record.pending_credential_id.clone();
        if !pending.is_empty() {
            // Re-issue the credential minted by an earlier redemption — the
            // invitation cannot yield two distinct credentials.
            let result = self
                .credential_index(&pending)
                .map(|i| self.issued_outcome(&self.credentials[i]))
                .unwrap_or(Err(AuthError::Failed));
            return (result, false);
        }
        let device_id = self.unique_id(true, fill);
        let credential_id = self.unique_id(false, fill);
        let mut secret = [0u8; SECRET_BYTES];
        fill(&mut secret);
        let (Some(device_id), Some(credential_id)) = (device_id, credential_id) else {
            return (Err(AuthError::Failed), false);
        };
        let record = self.invitation.as_ref().unwrap();
        let credential = Credential {
            device_id,
            credential_id: credential_id.clone(),
            name: record.name.clone(),
            role: record.role,
            locale: normalize_locale(&selector.locale),
            paired_at_ms: now_ms,
            last_seen_at_ms: now_ms,
            version: 1,
            revoked: false,
        };
        self.credentials.push(CredentialRecord {
            credential: credential.clone(),
            secret: b64().encode(secret),
        });
        self.invitation.as_mut().unwrap().pending_credential_id = credential_id;
        (
            Ok(AuthOutcome {
                identity: AuthenticatedIdentity {
                    device_id: credential.device_id,
                    credential_id: credential.credential_id,
                    role: credential.role,
                    locale: credential.locale,
                    credential_version: credential.version,
                },
                credential_secret: Some(secret),
            }),
            true,
        )
    }

    /// `credentialAuthResult(record, issueSecret=true)`.
    fn issued_outcome(&self, record: &CredentialRecord) -> Result<AuthOutcome, AuthError> {
        let credential = &record.credential;
        Ok(AuthOutcome {
            identity: AuthenticatedIdentity {
                device_id: credential.device_id.clone(),
                credential_id: credential.credential_id.clone(),
                role: credential.role,
                locale: credential.locale.clone(),
                credential_version: credential.version,
            },
            credential_secret: Some(Self::decode_secret(&record.secret)?),
        })
    }

    /// `completeCredentialLocked` — liveness refresh + burn the invitation
    /// whose pending credential just proved itself under credential auth.
    fn complete_credential(
        &mut self,
        selector: &AuthSelector,
        now_ms: i64,
    ) -> Result<AuthOutcome, AuthError> {
        let Some(index) = self.credential_index(&selector.id) else {
            return Err(AuthError::Failed);
        };
        let record = &mut self.credentials[index];
        if record.credential.revoked || record.credential.version != selector.version {
            return Err(AuthError::Failed);
        }
        record.credential.last_seen_at_ms = now_ms;
        record.credential.locale = normalize_locale(&selector.locale);
        if self
            .invitation
            .as_ref()
            .is_some_and(|i| i.pending_credential_id == record.credential.credential_id)
        {
            self.invitation = None;
        }
        let credential = &self.credentials[index].credential;
        Ok(AuthOutcome {
            identity: AuthenticatedIdentity {
                device_id: credential.device_id.clone(),
                credential_id: credential.credential_id.clone(),
                role: credential.role,
                locale: credential.locale.clone(),
                credential_version: credential.version,
            },
            credential_secret: None,
        })
    }

    /// `AuthorizeCredential` — current, unrevoked, version-matched.
    fn authorize(&self, credential_id: &str, version: u64) -> Option<Credential> {
        let record = &self.credentials[self.credential_index(credential_id)?];
        let credential = &record.credential;
        if credential.revoked || credential.version != version {
            return None;
        }
        Some(credential.clone())
    }

    /// `ListCredentials` — every credential record, tombstones included.
    fn list(&self) -> Vec<Credential> {
        self.credentials
            .iter()
            .map(|r| r.credential.clone())
            .collect()
    }

    /// `RenameCredential` — trim + `validateMetadata` the name, then update
    /// the stored record and return it.
    fn rename(&mut self, credential_id: &str, name: &str) -> (Result<Credential, AuthError>, bool) {
        let name = name.trim();
        if !valid_text(name, MAX_NAME_BYTES) {
            return (Err(AuthError::InvalidName), false);
        }
        let Some(index) = self.credential_index(credential_id) else {
            return (Err(AuthError::NotFound), false);
        };
        self.credentials[index].credential.name = name.to_owned();
        (Ok(self.credentials[index].credential.clone()), true)
    }

    /// `RevokeCredential` — tombstone the credential (`revoked`,
    /// `version++`, secret scrubbed) and drop an invitation still pending
    /// on it. Refusing the last active controller keeps the hub
    /// manageable; an already-revoked record is returned unchanged.
    fn revoke(&mut self, credential_id: &str) -> (Result<Credential, AuthError>, bool) {
        let Some(index) = self.credential_index(credential_id) else {
            return (Err(AuthError::NotFound), false);
        };
        let credential = &self.credentials[index].credential;
        if !credential.revoked
            && credential.role == Role::Controller
            && self.active_controllers() == 1
        {
            return (Err(AuthError::LastController), false);
        }
        let record = &mut self.credentials[index];
        if !record.credential.revoked {
            record.credential.revoked = true;
            record.credential.version += 1;
            record.secret.clear();
        }
        if self
            .invitation
            .as_ref()
            .is_some_and(|i| i.pending_credential_id == credential_id)
        {
            self.invitation = None;
        }
        (Ok(self.credentials[index].credential.clone()), true)
    }

    /// `activeControllerCountLocked` — unrevoked controllers enrolled.
    fn active_controllers(&self) -> usize {
        self.credentials
            .iter()
            .filter(|r| !r.credential.revoked && r.credential.role == Role::Controller)
            .count()
    }

    /// `CreateInvitation` — `validateMetadata` then mint id + 32-byte
    /// secret + 10-minute expiry, replacing the invitation slot.
    fn create(
        &mut self,
        name: &str,
        role: &str,
        locale: &str,
        now_ms: i64,
        fill: &mut dyn FnMut(&mut [u8]),
    ) -> (Result<Invitation, AuthError>, bool) {
        let (name, role, locale) = match validate_metadata(name, role, locale) {
            Ok(valid) => valid,
            Err(error) => return (Err(error), false),
        };
        let mut id = [0u8; IDENTIFIER_BYTES];
        let mut secret = [0u8; SECRET_BYTES];
        fill(&mut id);
        fill(&mut secret);
        let invitation = Invitation {
            invitation_id: b64().encode(id),
            version: 1,
            secret: b64().encode(secret),
            expires_at_ms: now_ms + INVITATION_LIFETIME_MS,
            name,
            role,
            locale,
            failed_attempts: 0,
            next_attempt_at_ms: 0,
            pending_credential_id: String::new(),
        };
        self.invitation = Some(invitation.clone());
        (Ok(invitation), true)
    }

    /// `ResetWithBootstrap` — the document is replaced wholesale: every
    /// credential gone, and with `rearm` configured a fresh `bootstrap`
    /// record (the operator's setup token) keeps the printed link pairing.
    /// `rearm_bootstrap` is a runtime flag, not document state — it
    /// survives the reset like the Go `Store` field does.
    fn reset(
        &mut self,
        rearm: Option<&BootstrapRearm>,
        locale: &str,
        now_ms: i64,
    ) -> (Result<(), AuthError>, bool) {
        let invitation = match rearm {
            Some(rearm) => {
                let (name, _, locale) = match validate_metadata(&rearm.name, "controller", locale) {
                    Ok(valid) => valid,
                    Err(error) => return (Err(error), false),
                };
                Some(Invitation {
                    invitation_id: BOOTSTRAP_INVITATION_ID.to_owned(),
                    version: 1,
                    secret: b64().encode(rearm.secret),
                    expires_at_ms: now_ms + INVITATION_LIFETIME_MS,
                    name,
                    role: Role::Controller,
                    locale,
                    failed_attempts: 0,
                    next_attempt_at_ms: 0,
                    pending_credential_id: String::new(),
                })
            }
            None => None,
        };
        let rearm_bootstrap = self.rearm_bootstrap;
        *self = AuthState {
            schema_version: STORE_SCHEMA_VERSION,
            invitation,
            credentials: Vec::new(),
            rearm_bootstrap,
        };
        (Ok(()), true)
    }

    /// `randomValue` + uniqueness — b64 of `IDENTIFIER_BYTES` random bytes,
    /// retried 8 times like the Go loop.
    fn unique_id(&mut self, device: bool, fill: &mut dyn FnMut(&mut [u8])) -> Option<String> {
        for _ in 0..8 {
            let mut bytes = [0u8; IDENTIFIER_BYTES];
            fill(&mut bytes);
            let candidate = b64().encode(bytes);
            let taken = if device {
                self.credentials
                    .iter()
                    .any(|r| r.credential.device_id == candidate)
            } else {
                self.credential_index(&candidate).is_some()
            };
            if !taken {
                return Some(candidate);
            }
        }
        None
    }

    /// Persisted-schema sanity (a light `validateState`): unique ids,
    /// non-zero versions, decodable secrets, revoked rows carry no secret.
    fn validate(&self) -> Result<(), StoreError> {
        let bad = |msg: &str| StoreError::Invalid(msg.to_owned());
        let mut devices = std::collections::HashSet::new();
        let mut credentials = std::collections::HashSet::new();
        for record in &self.credentials {
            let c = &record.credential;
            if c.device_id.is_empty() || c.credential_id.is_empty() || c.version == 0 {
                return Err(bad("credential identity is incomplete"));
            }
            if !devices.insert(&c.device_id) || !credentials.insert(&c.credential_id) {
                return Err(bad("duplicate device or credential identifier"));
            }
            if c.revoked {
                if !record.secret.is_empty() {
                    return Err(bad("revoked credential retains a secret"));
                }
            } else if Self::decode_secret(&record.secret).is_err() {
                return Err(bad("credential secret is invalid"));
            }
        }
        if let Some(inv) = &self.invitation {
            if inv.invitation_id.is_empty() || inv.version == 0 {
                return Err(bad("invitation identity is invalid"));
            }
            if Self::decode_secret(&inv.secret).is_err() {
                return Err(bad("invitation secret is invalid"));
            }
            if inv.failed_attempts >= MAX_INVITE_ATTEMPTS {
                return Err(bad("invitation attempt count is invalid"));
            }
        }
        Ok(())
    }
}

type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

/// The minted-bytes source — `rand.Reader` in Go, injectable for tests.
type Fill = Mutex<Box<dyn FnMut(&mut [u8]) + Send>>;

fn system_now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn os_fill(buf: &mut [u8]) {
    // rand 0.9: OsRng implements `TryRngCore`, not `RngCore`.
    use rand::TryRngCore;
    rand::rngs::OsRng
        .try_fill_bytes(buf)
        .expect("OS RNG failure is unrecoverable");
}

/// Store persistence / load failure.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("device store I/O on {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("decode device store: {0}")]
    Json(#[source] serde_json::Error),
    #[error("unsupported device store schema {0}")]
    Schema(u32),
    #[error("invalid device store: {0}")]
    Invalid(String),
}

/// Shared guts of both stores: state under a mutex, an injectable clock and
/// CSPRNG, and an optional persistence path.
struct StoreCore {
    state: Mutex<AuthState>,
    now: Clock,
    fill: Fill,
    path: Option<PathBuf>,
}

impl StoreCore {
    fn new(path: Option<PathBuf>) -> Self {
        Self {
            state: Mutex::new(AuthState::default()),
            now: Arc::new(system_now_ms),
            fill: Mutex::new(Box::new(os_fill)),
            path,
        }
    }

    fn now_ms(&self) -> i64 {
        (self.now)()
    }

    fn fill_bytes(&self, out: &mut [u8]) {
        (self.fill.lock().expect("fill poisoned"))(out)
    }

    /// Run `f` against the locked state; persist when it reports a mutation —
    /// a persist failure replaces the result exactly like the Go store.
    fn transact<T>(
        &self,
        f: impl FnOnce(&mut AuthState, i64, &mut dyn FnMut(&mut [u8])) -> (Result<T, AuthError>, bool),
    ) -> Result<T, AuthError> {
        let mut fill_guard = self.fill.lock().expect("fill poisoned");
        let mut guard = self.state.lock().expect("store poisoned");
        let (result, mutated) = f(&mut guard, self.now_ms(), &mut **fill_guard);
        drop(fill_guard);
        if mutated && self.path.is_some() {
            self.persist(&guard)
                .map_err(|e| AuthError::Store(Box::new(e)))?;
        }
        result
    }

    fn persist(&self, state: &AuthState) -> Result<(), StoreError> {
        let path = self.path.as_ref().expect("persist without path");
        persist_atomic(path, state)
    }

    fn credentials(&self) -> Vec<Credential> {
        self.state.lock().expect("store poisoned").list()
    }

    fn invitation(&self) -> Option<Invitation> {
        self.state
            .lock()
            .expect("store poisoned")
            .invitation
            .clone()
    }

    /// `RenameCredential` — validated and persisted through [`transact`].
    ///
    /// [`transact`]: StoreCore::transact
    fn rename_device(&self, credential_id: &str, name: &str) -> Result<Credential, AuthError> {
        self.transact(|state, _, _| state.rename(credential_id, name))
    }

    /// `RevokeCredential` — validated and persisted through [`transact`].
    ///
    /// [`transact`]: StoreCore::transact
    fn revoke_device(&self, credential_id: &str) -> Result<Credential, AuthError> {
        self.transact(|state, _, _| state.revoke(credential_id))
    }

    /// `CreateInvitation` — validated, minted, and persisted through
    /// [`transact`].
    ///
    /// [`transact`]: StoreCore::transact
    fn create_invitation(
        &self,
        name: &str,
        role: &str,
        locale: &str,
    ) -> Result<IssuedInvitation, AuthError> {
        self.transact(|state, now, fill| {
            let (result, mutated) = state.create(name, role, locale, now, fill);
            (result.map(|i| i.issued()), mutated)
        })
    }

    /// `ResetWithBootstrap` — the wholesale swap persists through
    /// [`transact`].
    ///
    /// [`transact`]: StoreCore::transact
    fn reset_devices(&self, rearm: Option<&BootstrapRearm>, locale: &str) -> Result<(), AuthError> {
        self.transact(|state, now, _| state.reset(rearm, locale, now))
    }
}

/// In-memory [`DeviceAuthStore`] — the reference impl and the test fixture.
/// Semantics are identical to [`FileAuthStore`]; nothing survives the process.
pub struct MemoryAuthStore {
    core: StoreCore,
}

impl Default for MemoryAuthStore {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryAuthStore {
    /// An empty store — no credentials, no invitation.
    pub fn new() -> Self {
        Self {
            core: StoreCore::new(None),
        }
    }

    /// Inject a clock (unix millis) — tests pin expiry/backoff.
    pub fn with_clock(mut self, now: impl Fn() -> i64 + Send + Sync + 'static) -> Self {
        self.core.now = Arc::new(now);
        self
    }

    /// Inject a deterministic byte source for minted ids/secrets.
    pub fn with_fill(mut self, fill: impl FnMut(&mut [u8]) + Send + 'static) -> Self {
        self.core.fill = Mutex::new(Box::new(fill));
        self
    }

    /// `WithBootstrapReenrollment` — re-arm the bootstrap invitation even with
    /// enrolled devices (quick-tunnel installs).
    pub fn with_bootstrap_reenrollment(self, rearm: bool) -> Self {
        self.core
            .state
            .lock()
            .expect("store poisoned")
            .rearm_bootstrap = rearm;
        self
    }

    /// Insert a credential verbatim (test seeding, config import).
    pub fn add_credential(&self, credential: Credential, secret_b64: impl Into<String>) {
        self.core
            .state
            .lock()
            .expect("store poisoned")
            .credentials
            .push(CredentialRecord {
                credential,
                secret: secret_b64.into(),
            });
    }

    /// Install an invitation verbatim (test seeding).
    pub fn set_invitation(&self, invitation: Invitation) {
        self.core.state.lock().expect("store poisoned").invitation = Some(invitation);
    }

    /// `CreateInvitation` — mint id + 32-byte secret + 10-minute expiry.
    pub fn create_invitation(
        &self,
        name: impl Into<String>,
        role: Role,
        locale: impl Into<String>,
    ) -> Invitation {
        let mut id = [0u8; IDENTIFIER_BYTES];
        let mut secret = [0u8; SECRET_BYTES];
        self.core.fill_bytes(&mut id);
        self.core.fill_bytes(&mut secret);
        let invitation = Invitation {
            invitation_id: b64().encode(id),
            version: 1,
            secret: b64().encode(secret),
            expires_at_ms: self.core.now_ms() + INVITATION_LIFETIME_MS,
            name: name.into(),
            role,
            locale: locale.into(),
            failed_attempts: 0,
            next_attempt_at_ms: 0,
            pending_credential_id: String::new(),
        };
        self.core.state.lock().expect("store poisoned").invitation = Some(invitation.clone());
        invitation
    }

    /// Snapshot of enrolled credentials (`ListCredentials`, minus `current`).
    pub fn credentials(&self) -> Vec<Credential> {
        self.core.credentials()
    }

    /// The live invitation, if any.
    pub fn invitation(&self) -> Option<Invitation> {
        self.core.invitation()
    }
}

impl DeviceAuthStore for MemoryAuthStore {
    fn resolve<'a>(
        &'a self,
        selector: &'a AuthSelector,
    ) -> BoxFuture<'a, Result<[u8; SECRET_BYTES], AuthError>> {
        Box::pin(async move {
            self.core
                .transact(|state, now, _| state.resolve(selector, now))
        })
    }

    fn complete<'a>(
        &'a self,
        selector: &'a AuthSelector,
        authenticated: bool,
    ) -> BoxFuture<'a, Result<AuthOutcome, AuthError>> {
        Box::pin(async move {
            self.core
                .transact(|state, now, fill| state.complete(selector, authenticated, now, fill))
        })
    }

    fn authorize(&self, credential_id: &str, version: u64) -> Option<Credential> {
        self.core
            .state
            .lock()
            .expect("store poisoned")
            .authorize(credential_id, version)
    }

    fn list_devices(&self) -> Result<Vec<Credential>, AuthError> {
        Ok(self.core.credentials())
    }

    fn rename_device(&self, credential_id: &str, name: &str) -> Result<Credential, AuthError> {
        self.core.rename_device(credential_id, name)
    }

    fn revoke_device(&self, credential_id: &str) -> Result<Credential, AuthError> {
        self.core.revoke_device(credential_id)
    }

    fn create_invitation(
        &self,
        name: &str,
        role: &str,
        locale: &str,
    ) -> Result<IssuedInvitation, AuthError> {
        self.core.create_invitation(name, role, locale)
    }

    fn reset_devices(&self, rearm: Option<&BootstrapRearm>, locale: &str) -> Result<(), AuthError> {
        self.core.reset_devices(rearm, locale)
    }
}

/// JSON-file [`DeviceAuthStore`] — the production impl. Loads `devices.json`
/// (created when absent, mirroring `Store.Open`), persists every mutation
/// atomically (temp file + `sync` + `rename`, `0600` on a `0700` dir).
pub struct FileAuthStore {
    core: StoreCore,
}

impl FileAuthStore {
    /// `Open(dir)` — create the directory (0700) and file (0600) as needed,
    /// load and validate an existing store.
    pub fn open(dir: impl AsRef<Path>) -> Result<Self, StoreError> {
        let dir = dir.as_ref();
        protect_directory(dir)?;
        let path = dir.join(STORE_FILENAME);
        let state = match std::fs::File::open(&path) {
            Ok(file) => {
                let mut raw = Vec::new();
                io::Read::take(io::BufReader::new(file), MAX_STORE_BYTES + 1)
                    .read_to_end(&mut raw)
                    .map_err(|source| StoreError::Io {
                        path: path.clone(),
                        source,
                    })?;
                if raw.len() as u64 > MAX_STORE_BYTES {
                    return Err(StoreError::Invalid("device store too large".to_owned()));
                }
                let state: AuthState = serde_json::from_slice(&raw).map_err(StoreError::Json)?;
                if state.schema_version != STORE_SCHEMA_VERSION {
                    return Err(StoreError::Schema(state.schema_version));
                }
                state.validate()?;
                state
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => AuthState::default(),
            Err(source) => {
                return Err(StoreError::Io {
                    path: path.clone(),
                    source,
                })
            }
        };
        let store = Self {
            core: StoreCore::new(Some(path)),
        };
        *store.core.state.lock().expect("store poisoned") = state;
        // Mirror Open(): persist once so a fresh dir gains a well-formed file.
        store
            .core
            .persist(&store.core.state.lock().expect("store poisoned"))?;
        Ok(store)
    }

    /// Snapshot of enrolled credentials.
    pub fn credentials(&self) -> Vec<Credential> {
        self.core.credentials()
    }

    /// The live invitation, if any.
    pub fn invitation(&self) -> Option<Invitation> {
        self.core.invitation()
    }

    /// Insert a credential verbatim and persist.
    pub fn add_credential(
        &self,
        credential: Credential,
        secret_b64: impl Into<String>,
    ) -> Result<(), AuthError> {
        self.core.transact(|state, _, _| {
            state.credentials.push(CredentialRecord {
                credential,
                secret: secret_b64.into(),
            });
            (Ok(()), true)
        })
    }

    /// Install an invitation verbatim and persist.
    pub fn set_invitation(&self, invitation: Invitation) -> Result<(), AuthError> {
        self.core.transact(|state, _, _| {
            state.invitation = Some(invitation);
            (Ok(()), true)
        })
    }
}

impl DeviceAuthStore for FileAuthStore {
    fn resolve<'a>(
        &'a self,
        selector: &'a AuthSelector,
    ) -> BoxFuture<'a, Result<[u8; SECRET_BYTES], AuthError>> {
        Box::pin(async move {
            self.core
                .transact(|state, now, _| state.resolve(selector, now))
        })
    }

    fn complete<'a>(
        &'a self,
        selector: &'a AuthSelector,
        authenticated: bool,
    ) -> BoxFuture<'a, Result<AuthOutcome, AuthError>> {
        Box::pin(async move {
            self.core
                .transact(|state, now, fill| state.complete(selector, authenticated, now, fill))
        })
    }

    fn authorize(&self, credential_id: &str, version: u64) -> Option<Credential> {
        self.core
            .state
            .lock()
            .expect("store poisoned")
            .authorize(credential_id, version)
    }

    fn list_devices(&self) -> Result<Vec<Credential>, AuthError> {
        Ok(self.core.credentials())
    }

    fn rename_device(&self, credential_id: &str, name: &str) -> Result<Credential, AuthError> {
        self.core.rename_device(credential_id, name)
    }

    fn revoke_device(&self, credential_id: &str) -> Result<Credential, AuthError> {
        self.core.revoke_device(credential_id)
    }

    fn create_invitation(
        &self,
        name: &str,
        role: &str,
        locale: &str,
    ) -> Result<IssuedInvitation, AuthError> {
        self.core.create_invitation(name, role, locale)
    }

    fn reset_devices(&self, rearm: Option<&BootstrapRearm>, locale: &str) -> Result<(), AuthError> {
        self.core.reset_devices(rearm, locale)
    }
}

/// `protectDirectory` — `0700`, must be a real directory, not a symlink.
fn protect_directory(dir: &Path) -> Result<(), StoreError> {
    let io_err = |source: io::Error| StoreError::Io {
        path: dir.to_path_buf(),
        source,
    };
    std::fs::create_dir_all(dir).map_err(io_err)?;
    let meta = std::fs::symlink_metadata(dir).map_err(io_err)?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err(StoreError::Invalid(
            "device store path is not a directory".to_owned(),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).map_err(io_err)?;
    }
    Ok(())
}

/// `persistLocked` — marshal, `0600` temp file in the same dir, `sync`,
/// rename, dir `sync` (best-effort).
fn persist_atomic(path: &Path, state: &AuthState) -> Result<(), StoreError> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let mut data = serde_json::to_vec(state).map_err(StoreError::Json)?;
    data.push(b'\n');
    let tmp = dir.join(format!(
        ".devices-{}-{}.tmp",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let result = (|| -> io::Result<()> {
        use io::Write;
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(&data)?;
        file.sync_all()?;
        Ok(())
    })();
    if let Err(source) = result {
        let _ = std::fs::remove_file(&tmp);
        return Err(StoreError::Io { path: tmp, source });
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    if let Err(source) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(StoreError::Io {
            path: path.to_path_buf(),
            source,
        });
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    let _ = std::fs::File::open(dir).and_then(|f| f.sync_all());
    Ok(())
}
