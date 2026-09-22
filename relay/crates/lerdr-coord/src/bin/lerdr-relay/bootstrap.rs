//! Pairing bootstrap — the oracle's `armBootstrap`/`ArmBootstrapInvitation`
//! (`internal/app/server.go`) plus the setup-link rendering the shell scripts
//! own (`relay/common.sh`, `internal/setuphelper`).
//!
//! Invitation shapes (docs/specs/pairing-store.md §A.2):
//!
//! - **Token bootstrap** — when a relay key is configured the invitation is
//!   `{id: "bootstrap", secret: b64url(utf8(key))}` and the printed link is
//!   `SetupFragment` form (`setup=<key>`): the phone wraps the raw 32-byte
//!   key as `{id:"bootstrap", version:1}` itself (`store.ts:761-772`).
//! - **Random invitation** — a tokenless relay mints a
//!   `create_invitation`-shaped record (random 24-char id, random 32-byte
//!   secret) and prints the `createDeviceInvitation` link form
//!   (`setup=<b64secret>&invite=<id>&invite_version=<v>&invite_expires=<ms>`).
//!
//! SIGUSR1 re-arms (`arm_setup_link` sends it before the script prints): the
//! invitation slot is replaced and every enrolled credential is kept.

use std::io;
use std::path::Path;
use std::sync::Arc;

use base64::Engine;
use lerdr_e2ee::handshake::{AuthKind, AuthSelector, SECRET_BYTES};
use tracing::{debug, info, warn};

use lerdr_relay::auth::{
    AuthError, AuthOutcome, BootstrapRearm, BoxFuture, Credential, DeviceAuthStore,
    IssuedInvitation, Role,
};
use lerdr_relay::store::{
    FileAuthStore, Invitation, BOOTSTRAP_INVITATION_ID, INVITATION_LIFETIME_MS, STORE_FILENAME,
};

/// `identifierBytes` — minted ids are 18 random bytes → 24-char b64url.
const IDENTIFIER_BYTES: usize = 18;

fn b64() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
}

/// The OS randomness source — `rand.Reader` (`OsRng` under rand 0.9).
pub fn os_fill(buf: &mut [u8]) {
    use rand::TryRngCore;
    rand::rngs::OsRng
        .try_fill_bytes(buf)
        .expect("OS RNG failure is unrecoverable");
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// A live invitation plus the link fragment that redeems it.
#[derive(Debug, Clone)]
pub struct PairingOffer {
    pub invitation: Invitation,
    /// The URL fragment — token form (`setup=<key>`) for the bootstrap
    /// record, `invite=` params for a minted invitation.
    pub fragment: String,
}

impl PairingOffer {
    /// The deep link the phone consumes (`lerdr://pair#<fragment>`).
    pub fn deep_link(&self) -> String {
        format!("{}#{}", crate::config::DEEP_LINK_BASE, self.fragment)
    }
}

/// `EnsureBootstrapInvitation` on `serve`: with credentials enrolled the
/// store is paired and stays untouched (`None`); otherwise a live unexpired
/// invitation is kept — re-printing its link needs no re-mint — and anything
/// else mints a fresh one-use invitation.
///
/// The offer's fragment follows the armed record: a `bootstrap` invitation
/// redeems via the relay key (`setup=<key>`); any other id rides the
/// `invite=` params (`createDeviceInvitation`, `store.ts:1830-1840`).
pub fn ensure_pairing(
    store: &FileAuthStore,
    label: &str,
    relay_url: &str,
    token: Option<&str>,
    fill: &mut dyn FnMut(&mut [u8]),
) -> Result<Option<PairingOffer>, AuthError> {
    if !store.credentials().is_empty() {
        return Ok(None);
    }
    let now = now_ms();
    if let Some(offer) = store
        .invitation()
        .filter(|i| now < i.expires_at_ms)
        .and_then(|i| offer_for(i, label, relay_url))
    {
        return Ok(Some(offer));
    }
    let invitation = arm_invitation(store, label, token, fill)?;
    Ok(offer_for(invitation, label, relay_url))
}

/// `ArmBootstrapInvitation` — SIGUSR1: replace the invitation slot, keep
/// every enrolled credential. Token-configured relays re-arm the bootstrap
/// record (the printed link is stable — the key doesn't change); tokenless
/// relays mint a fresh random invitation per re-arm.
pub fn arm_invitation(
    store: &FileAuthStore,
    label: &str,
    token: Option<&str>,
    fill: &mut dyn FnMut(&mut [u8]),
) -> Result<Invitation, AuthError> {
    let invitation = mint_invitation(label, token, fill);
    store.set_invitation(invitation.clone())?;
    info!(
        invitation_id = %invitation.invitation_id,
        expires_at_ms = invitation.expires_at_ms,
        "pairing invitation issued"
    );
    Ok(invitation)
}

/// The offer an armed invitation produces — re-arms hand this the fresh
/// record for printing.
pub fn offer_for_invitation(
    invitation: &Invitation,
    label: &str,
    relay_url: &str,
) -> Option<PairingOffer> {
    offer_for(invitation.clone(), label, relay_url)
}

/// `armBootstrapLocked` — the fresh one-use record. `Role::Controller` +
/// `locale: "en"` mirror the oracle's hardcoded bootstrap metadata; `name`
/// carries the host label.
fn mint_invitation(
    label: &str,
    token: Option<&str>,
    fill: &mut dyn FnMut(&mut [u8]),
) -> Invitation {
    let (id, secret) = match token {
        // `bootstrapInvitationID` + the relay key as the pairing secret — the
        // id is exempt from attempt burning and re-arms on expiry while no
        // credentials exist (`resolve_invitation` in store.rs).
        Some(token) => (
            BOOTSTRAP_INVITATION_ID.to_owned(),
            b64().encode(token.as_bytes()),
        ),
        // `CreateInvitation` shape: random 18-byte id + random 32-byte secret.
        None => {
            let mut id = [0u8; IDENTIFIER_BYTES];
            let mut secret = [0u8; SECRET_BYTES];
            fill(&mut id);
            fill(&mut secret);
            (b64().encode(id), b64().encode(secret))
        }
    };
    Invitation {
        invitation_id: id,
        version: 1,
        secret,
        expires_at_ms: now_ms() + INVITATION_LIFETIME_MS,
        name: label.to_owned(),
        role: Role::Controller,
        locale: "en".to_owned(),
        failed_attempts: 0,
        next_attempt_at_ms: 0,
        pending_credential_id: String::new(),
    }
}

/// Build the printed fragment for an armed invitation. The bootstrap record
/// stores `b64url(utf8(key))` — the link needs the raw key back.
fn offer_for(invitation: Invitation, label: &str, relay_url: &str) -> Option<PairingOffer> {
    let fragment = if invitation.invitation_id == BOOTSTRAP_INVITATION_ID {
        let secret = b64().decode(&invitation.secret).ok()?;
        let token = String::from_utf8(secret).ok()?;
        setup_fragment(&token, label, Some(relay_url))
    } else {
        invitation_fragment(&invitation, label, relay_url)
    };
    Some(PairingOffer {
        invitation,
        fragment,
    })
}

/// `SetupFragment` — `url.Values.Encode()` emits keys sorted: label, relay,
/// setup. `relay` is omitted when empty.
pub fn setup_fragment(token: &str, label: &str, relay: Option<&str>) -> String {
    let mut fragment = format!("label={}", form_encode(label));
    if let Some(relay) = relay.filter(|r| !r.is_empty()) {
        fragment.push_str(&format!("&relay={}", form_encode(relay)));
    }
    fragment.push_str(&format!("&setup={}", form_encode(token)));
    fragment
}

/// The `createDeviceInvitation` fragment — `URLSearchParams` preserves the
/// literal insertion order (`store.ts:1830-1840`): setup, invite,
/// invite_version, invite_expires, label, relay.
pub fn invitation_fragment(invitation: &Invitation, label: &str, relay_url: &str) -> String {
    format!(
        "setup={}&invite={}&invite_version={}&invite_expires={}&label={}&relay={}",
        form_encode(&invitation.secret),
        form_encode(&invitation.invitation_id),
        invitation.version,
        invitation.expires_at_ms,
        form_encode(label),
        form_encode(relay_url),
    )
}

/// `url.QueryEscape` — `[A-Za-z0-9-_.~]` pass through, space becomes `+`,
/// every other byte is `%XX` uppercase. Both oracle encoders (Go
/// `url.Values` and `URLSearchParams`) parse this shape back losslessly.
fn form_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// `print_phone_setup` — the terminal block the setup scripts emit: QR first
/// (half-block rows like `TerminalQR`), then the link, always on stderr.
pub fn print_setup_link(link: &str) {
    eprintln!();
    match terminal_qr(link) {
        Some(qr) => {
            eprintln!("  Scan this QR code with your phone camera:");
            eprintln!();
            for line in qr.lines() {
                eprintln!("  {line}");
            }
            eprintln!();
            eprintln!(
                "  This code pairs one phone with this relay; do not share screenshots of it."
            );
            eprintln!();
            eprintln!("  Or open this private setup link on your phone:");
        }
        None => {
            eprintln!("  Open this private setup link on your phone:");
        }
    }
    eprintln!("  {link}");
    eprintln!();
    eprintln!("  This link pairs one phone within 10 minutes. Print it again for another.");
    eprintln!();
}

/// `TerminalQR` — EC level M, half-block rows, quiet zone on. Encoding
/// failure (oversized value) yields `None` — the link line prints regardless,
/// matching `render_setup_qr`'s allowed-to-fail contract.
fn terminal_qr(value: &str) -> Option<String> {
    let code = qrcode::QrCode::with_error_correction_level(value, qrcode::EcLevel::M).ok()?;
    Some(
        code.render::<qrcode::render::unicode::Dense1x2>()
            .quiet_zone(true)
            .build(),
    )
}

/// `writePIDFile` — `<RuntimeDir>/relay.pid` so `arm_setup_link` in
/// `relay/common.sh` finds the process to SIGUSR1. Best-effort at the call
/// site: a read-only runtime dir must not stop the serve.
pub fn write_pid_file(runtime_dir: &Path) -> io::Result<()> {
    use io::Write;
    std::fs::create_dir_all(runtime_dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(runtime_dir, std::fs::Permissions::from_mode(0o700));
    }
    let path = runtime_dir.join("relay.pid");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&path)?;
    writeln!(file, "{}", std::process::id())
}

/// `ResetWithBootstrap` at startup: `RELAY_REARM_BOOTSTRAP` serves each
/// launch from a new identity, so the enrolled device list is wiped before
/// the store opens. File-level reset — `FileAuthStore` has no
/// credential-clearing API.
pub fn reset_device_store(device_auth_dir: &Path) -> io::Result<()> {
    let path = device_auth_dir.join(STORE_FILENAME);
    match std::fs::remove_file(&path) {
        Ok(()) => {
            info!(path = %path.display(), "device store reset for re-armed bootstrap");
            Ok(())
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// [`DeviceAuthStore`] decorator that traces the pairing lifecycle — the
/// oracle's issued/redeemed journal lines without leaking secrets (only ids,
/// roles, and outcomes are logged).
pub struct EventedAuthStore {
    inner: Arc<FileAuthStore>,
}

impl EventedAuthStore {
    pub fn new(inner: Arc<FileAuthStore>) -> Self {
        Self { inner }
    }
}

impl DeviceAuthStore for EventedAuthStore {
    fn resolve<'a>(
        &'a self,
        selector: &'a AuthSelector,
    ) -> BoxFuture<'a, Result<[u8; SECRET_BYTES], AuthError>> {
        Box::pin(async move { self.inner.resolve(selector).await })
    }

    fn complete<'a>(
        &'a self,
        selector: &'a AuthSelector,
        authenticated: bool,
    ) -> BoxFuture<'a, Result<AuthOutcome, AuthError>> {
        Box::pin(async move {
            let result = self.inner.complete(selector, authenticated).await;
            match (selector.kind, authenticated, &result) {
                (AuthKind::Invitation, true, Ok(outcome)) => {
                    let identity = &outcome.identity;
                    info!(
                        invitation_id = %selector.id,
                        device_id = %identity.device_id,
                        credential_id = %identity.credential_id,
                        role = %identity.role,
                        "pairing invitation redeemed"
                    );
                }
                (AuthKind::Invitation, false, Err(error)) => {
                    warn!(invitation_id = %selector.id, %error, "pairing invitation proof rejected");
                }
                (AuthKind::Credential, true, Ok(_)) => {
                    debug!(credential_id = %selector.id, "device credential authenticated");
                }
                _ => {}
            }
            result
        })
    }

    fn authorize(&self, credential_id: &str, version: u64) -> Option<Credential> {
        self.inner.authorize(credential_id, version)
    }

    // Device administration delegates straight through — without these the
    // trait defaults answer `Unsupported` ("Device management is
    // unavailable") even though `FileAuthStore` implements them.

    fn list_devices(&self) -> Result<Vec<Credential>, AuthError> {
        self.inner.list_devices()
    }

    fn rename_device(&self, credential_id: &str, name: &str) -> Result<Credential, AuthError> {
        self.inner.rename_device(credential_id, name)
    }

    fn revoke_device(&self, credential_id: &str) -> Result<Credential, AuthError> {
        self.inner.revoke_device(credential_id)
    }

    fn create_invitation(
        &self,
        name: &str,
        role: &str,
        locale: &str,
    ) -> Result<IssuedInvitation, AuthError> {
        self.inner.create_invitation(name, role, locale)
    }

    fn reset_devices(&self, rearm: Option<&BootstrapRearm>, locale: &str) -> Result<(), AuthError> {
        self.inner.reset_devices(rearm, locale)
    }
}

/// `host_label` — `hostname -s`, "relay" when the machine can't say.
pub fn host_label() -> String {
    let name = gethostname::gethostname().to_string_lossy().into_owned();
    name.split('.')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or("relay")
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use lerdr_relay::auth::Role;

    /// Deterministic byte source — sequential bytes make ids/secrets
    /// predictable for assertions.
    fn seq_fill(buf: &mut [u8]) {
        static NEXT: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
        for byte in buf.iter_mut() {
            *byte = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }

    fn open_store(dir: &Path) -> FileAuthStore {
        FileAuthStore::open(dir).expect("open store")
    }

    fn credential(id: &str) -> Credential {
        Credential {
            device_id: format!("dev-{id}"),
            credential_id: id.to_owned(),
            name: "phone".into(),
            role: Role::Controller,
            locale: "en".into(),
            paired_at_ms: 1,
            last_seen_at_ms: 1,
            version: 1,
            revoked: false,
        }
    }

    #[test]
    fn setup_fragment_matches_go_encoding() {
        // setuphelper_test.go TestSetupFragment — sorted keys, query escapes.
        let fragment = setup_fragment("a+b&c", "My Host", Some("wss://example.test/ws?a=1"));
        assert_eq!(
            fragment,
            "label=My+Host&relay=wss%3A%2F%2Fexample.test%2Fws%3Fa%3D1&setup=a%2Bb%26c"
        );
        // Empty relay is omitted.
        assert_eq!(setup_fragment("tok", "h", None), "label=h&setup=tok");
        assert_eq!(setup_fragment("tok", "h", Some("")), "label=h&setup=tok");
    }

    #[test]
    fn invitation_fragment_matches_store_ts_order() {
        let invitation = Invitation {
            invitation_id: "inv-abc_123".into(),
            version: 1,
            secret: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".into(),
            expires_at_ms: 1_750_000_000_000,
            name: "workstation".into(),
            role: Role::Controller,
            locale: "en".into(),
            failed_attempts: 0,
            next_attempt_at_ms: 0,
            pending_credential_id: String::new(),
        };
        assert_eq!(
            invitation_fragment(&invitation, "work station", "wss://host.ts.net"),
            "setup=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA&invite=inv-abc_123&invite_version=1&invite_expires=1750000000000&label=work+station&relay=wss%3A%2F%2Fhost.ts.net"
        );
    }

    #[test]
    fn deep_link_uses_lerdr_pair_base() {
        let offer = PairingOffer {
            invitation: Invitation {
                invitation_id: "x".repeat(24),
                version: 1,
                secret: "s".repeat(43),
                expires_at_ms: 1,
                name: "h".into(),
                role: Role::Controller,
                locale: "en".into(),
                failed_attempts: 0,
                next_attempt_at_ms: 0,
                pending_credential_id: String::new(),
            },
            fragment: "setup=s".into(),
        };
        assert_eq!(offer.deep_link(), "lerdr://pair#setup=s");
    }

    #[test]
    fn token_arms_bootstrap_record() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_store(dir.path());
        let token = "k".repeat(32);
        let offer = ensure_pairing(
            &store,
            "workstation",
            "ws://127.0.0.1:8375",
            Some(&token),
            &mut seq_fill,
        )
        .unwrap()
        .expect("empty store yields an offer");
        assert_eq!(offer.invitation.invitation_id, "bootstrap");
        assert_eq!(offer.invitation.secret, b64().encode(token.as_bytes()));
        // The link is the SetupFragment token form — no invite params.
        assert_eq!(
            offer.fragment,
            format!("label=workstation&relay=ws%3A%2F%2F127.0.0.1%3A8375&setup={token}")
        );
    }

    #[test]
    fn tokenless_mints_random_invitation() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_store(dir.path());
        let offer = ensure_pairing(&store, "h", "ws://127.0.0.1:8375", None, &mut seq_fill)
            .unwrap()
            .unwrap();
        let invitation = &offer.invitation;
        assert_eq!(invitation.invitation_id.len(), 24);
        assert_eq!(invitation.secret.len(), 43);
        assert_eq!(invitation.version, 1);
        assert_eq!(invitation.role, Role::Controller);
        assert!(offer.fragment.contains("&invite="));
        assert!(offer.fragment.starts_with("setup="));
    }

    #[test]
    fn paired_store_arms_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_store(dir.path());
        store
            .add_credential(credential("cred-1"), b64().encode([7u8; 32]))
            .unwrap();
        assert!(ensure_pairing(&store, "h", "ws://x", None, &mut seq_fill)
            .unwrap()
            .is_none());
    }

    #[test]
    fn live_invitation_is_reused_not_reminted() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_store(dir.path());
        let first = ensure_pairing(&store, "h", "ws://x", None, &mut seq_fill)
            .unwrap()
            .unwrap();
        let second = ensure_pairing(&store, "h", "ws://x", None, &mut seq_fill)
            .unwrap()
            .unwrap();
        assert_eq!(
            first.invitation.invitation_id,
            second.invitation.invitation_id
        );
        assert_eq!(first.fragment, second.fragment);
    }

    #[test]
    fn sigusr1_rearm_replaces_invitation_and_keeps_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_store(dir.path());
        store
            .add_credential(credential("cred-1"), b64().encode([9u8; 32]))
            .unwrap();
        let first = arm_invitation(&store, "h", None, &mut seq_fill).unwrap();
        let second = arm_invitation(&store, "h", None, &mut seq_fill).unwrap();
        assert_ne!(first.invitation_id, second.invitation_id);
        assert_eq!(store.credentials().len(), 1);
        assert_eq!(
            store.invitation().unwrap().invitation_id,
            second.invitation_id
        );
    }

    #[test]
    fn reset_device_store_wipes_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_store(dir.path());
        store
            .add_credential(credential("cred-1"), b64().encode([3u8; 32]))
            .unwrap();
        drop(store);
        reset_device_store(dir.path()).unwrap();
        let reopened = open_store(dir.path());
        assert!(reopened.credentials().is_empty());
    }

    #[test]
    fn qr_renders_half_blocks() {
        let qr = terminal_qr("lerdr://pair#setup=secret").unwrap();
        assert!(qr.contains('█') || qr.contains('▀') || qr.contains('▄'));
    }
}
