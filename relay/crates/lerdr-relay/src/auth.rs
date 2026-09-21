//! Device authentication boundary — the `E2EEAuthResolver` port.
//!
//! The handshake driver consults a [`DeviceAuthStore`] twice: [`resolve`] to
//! fetch the 32-byte pairing secret for a selector, and [`complete`] after
//! authentication — with `authenticated = false` for a structurally valid
//! hello whose proof failed (invitation attempt limiting), and
//! `authenticated = true` after the client finish opened correctly, to
//! atomically consume the invitation / refresh the credential.
//!
//! [`AuthError::is_rejected`] is the retry semantics oracle: a rejected auth
//! means the same selector can never succeed (unknown, revoked, superseded,
//! expired, or burned), so the connection closes with
//! [`UNAUTHORIZED_CLOSE_CODE`] and the phone stops retrying. Transient
//! failures (rate limiting, store I/O) drop the connection without it.
//!
//! [`resolve`]: DeviceAuthStore::resolve
//! [`complete`]: DeviceAuthStore::complete

use std::future::Future;
use std::pin::Pin;

use lerdr_e2ee::handshake::{AuthSelector, SECRET_BYTES};

/// Boxed future — keeps [`DeviceAuthStore`] dyn-compatible so the server can
/// hold `Arc<dyn DeviceAuthStore>` and pick the impl at wiring time.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// `Role` — device privilege carried by credentials and invitations
/// (`deviceauth.Role`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Controller,
    Reader,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Controller => "controller",
            Role::Reader => "reader",
        }
    }
}

impl std::fmt::Display for Role {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// `AuthenticatedIdentity` — the runtime identity a completed auth yields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthenticatedIdentity {
    pub device_id: String,
    pub credential_id: String,
    pub role: Role,
    pub locale: String,
    pub credential_version: u64,
}

impl AuthenticatedIdentity {
    /// `validAuthenticatedIdentity`: all fields populated, a known role.
    pub fn is_valid(&self) -> bool {
        !self.device_id.is_empty()
            && !self.credential_id.is_empty()
            && self.credential_version != 0
            && !self.locale.is_empty()
    }
}

/// The result of [`DeviceAuthStore::complete`] — `E2EEAuthResult`.
#[derive(Debug, Clone)]
pub struct AuthOutcome {
    pub identity: AuthenticatedIdentity,
    /// Present only when an invitation redemption issued a fresh credential;
    /// the relay forwards it to the client inside `e2ee_server_finish`.
    pub credential_secret: Option<[u8; SECRET_BYTES]>,
}

/// A device credential record — the public face of a store row.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Credential {
    pub device_id: String,
    pub credential_id: String,
    #[serde(default)]
    pub name: String,
    pub role: Role,
    #[serde(default)]
    pub locale: String,
    /// Unix milliseconds; i64 timestamps keep the store serde-trivial.
    #[serde(default)]
    pub paired_at_ms: i64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub last_seen_at_ms: i64,
    pub version: u64,
    #[serde(default)]
    pub revoked: bool,
}

fn is_zero(v: &i64) -> bool {
    *v == 0
}

/// Device-auth failures. `is_rejected` decides the close code: rejected
/// means "stop retrying" (4401), everything else is transient.
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    /// Selector unknown, version stale, or proof bookkeeping on a dead
    /// selector (`ErrAuthentication`).
    #[error("device authentication failed")]
    Failed,
    /// The credential exists but was revoked (`ErrRevoked`).
    #[error("device credential revoked")]
    Revoked,
    /// The invitation is past its expiry (`ErrInvitationExpired`).
    #[error("invitation expired")]
    InvitationExpired,
    /// The invitation exhausted its attempt budget (`ErrInvitationBurned`).
    #[error("invitation burned")]
    InvitationBurned,
    /// Backoff after failed attempts is still in effect — **transient**, a
    /// legitimate device keeps its invitation (`ErrRateLimited`).
    #[error("invitation attempt rate limited")]
    RateLimited,
    /// Store I/O / corruption — transient at the auth boundary.
    #[error("device store failure: {0}")]
    Store(#[source] Box<dyn std::error::Error + Send + Sync>),
}

impl AuthError {
    /// `IsE2EEAuthRejected`: can retrying the same selector ever succeed?
    pub fn is_rejected(&self) -> bool {
        matches!(
            self,
            AuthError::Failed
                | AuthError::Revoked
                | AuthError::InvitationExpired
                | AuthError::InvitationBurned
        )
    }
}

/// The only authority the transport consults (`E2EEAuthResolver`).
///
/// Implementations serialize their own state internally (a store mutex); the
/// driver calls them sequentially per connection but many handshakes may run
/// concurrently. Dyn-compatible (`Arc<dyn DeviceAuthStore>`) — implementors
/// box their futures with [`BoxFuture`].
pub trait DeviceAuthStore: Send + Sync {
    /// `ResolveE2EESecret` — the 32-byte pairing secret for `selector`, or a
    /// classified refusal.
    fn resolve<'a>(
        &'a self,
        selector: &'a AuthSelector,
    ) -> BoxFuture<'a, Result<[u8; SECRET_BYTES], AuthError>>;

    /// `CompleteE2EEAuth` — `authenticated = false` records a failed proof
    /// (invitation attempt limiting); `authenticated = true` commits the
    /// auth after the client finish opened: redeem an invitation (issue the
    /// device its credential) or refresh a credential's liveness.
    fn complete<'a>(
        &'a self,
        selector: &'a AuthSelector,
        authenticated: bool,
    ) -> BoxFuture<'a, Result<AuthOutcome, AuthError>>;

    /// `AuthorizeCredential` — the per-action currency re-check: the
    /// credential must still exist, be unrevoked, and match `version`
    /// (rotated/revoked credentials stop mid-session actions).
    fn authorize(&self, credential_id: &str, version: u64) -> Option<Credential>;
}
