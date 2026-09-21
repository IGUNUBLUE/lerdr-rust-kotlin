//! Async driver around [`ServerHandshake`] — the `performServerE2EEHandshake`
//! port (`internal/transport/e2ee.go`). The sync core owns crypto; this driver
//! owns I/O, the 10-second deadline, and auth-store dispatch:
//!
//! ```text
//! read client_hello (plaintext)  ──► parse ──► resolve secret
//!      │                                    ──► verify proof (begin)
//!      │  proof bad ──► complete(false) ──► REJECTED
//!      ▼
//! server_hello (plaintext)  ──► read + open client_finish ──► parse
//!      ──► complete(true)  [commit: redeem invitation / refresh credential]
//!      ──► seal + write server_finish  ──► Session
//! ```
//!
//! Error → close mapping (`Hub.Serve`): [`HandshakeError::is_rejected`] →
//! graceful `CloseStatus::Unauthorized` (4401, "stop retrying"); every other
//! failure → `close_now`, no close frame.

use std::time::Duration;

use base64::Engine;
use lerdr_e2ee::handshake::{
    parse_client_finish, AuthKind, AuthSelector, ServerFinish, ServerHandshake, NONCE_BYTES,
    SECRET_BYTES,
};
use lerdr_e2ee::{Codec, E2eeError, Session};
use p256::SecretKey;

use crate::auth::{AuthError, AuthenticatedIdentity, DeviceAuthStore};
use crate::frame::{FrameRead, FrameWrite, ReadError, WriteError};

/// `e2eeHandshakeTimeout` — covers the whole four-frame exchange.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Fresh handshake key material — the CSPRNG seam. Production uses
/// [`OsKeySource`]; tests pin key+nonce to replay fixtures.
pub trait KeySource: Send {
    /// A fresh P-256 ephemeral key for one handshake.
    fn secret_key(&mut self) -> SecretKey;
    /// A fresh 32-byte server nonce.
    fn nonce(&mut self) -> [u8; NONCE_BYTES];
}

/// Production key source — `getrandom`-backed via `p256`'s `Generate` trait.
#[derive(Debug, Default)]
pub struct OsKeySource;

impl KeySource for OsKeySource {
    fn secret_key(&mut self) -> SecretKey {
        use p256::elliptic_curve::Generate;
        SecretKey::generate()
    }

    fn nonce(&mut self) -> [u8; NONCE_BYTES] {
        use p256::elliptic_curve::Generate;
        <[u8; NONCE_BYTES]>::generate()
    }
}

/// A completed handshake: the sealed session plus the committed identity.
pub struct HandshakeSuccess {
    pub session: Session,
    pub identity: AuthenticatedIdentity,
    /// The selector the client authenticated under — the session needs it
    /// for per-action authorization (credential id/version fence).
    pub selector: AuthSelector,
}

/// Handshake failures. `is_rejected` is the `ErrDeviceAuthRejected` port —
/// the only failure class that earns a graceful 4401 close.
#[derive(Debug, thiserror::Error)]
pub enum HandshakeError {
    /// `e2eeHandshakeTimeout` elapsed mid-exchange.
    #[error("e2ee handshake timed out")]
    Timeout,
    /// Refused permanently — unknown/revoked/burned/expired selector, a
    /// failed proof, or an auth record invalidated mid-handshake. Closes
    /// 4401 so the phone stops retrying dead material.
    #[error("device authentication rejected")]
    Rejected,
    /// Transport read failed (or the peer closed mid-handshake).
    #[error("frame read during handshake: {0}")]
    Read(#[source] ReadError),
    /// Transport write failed.
    #[error("frame write during handshake: {0}")]
    Write(#[source] WriteError),
    /// Malformed client hello or client finish.
    #[error("invalid handshake message: {0}")]
    Malformed(#[source] E2eeError),
    /// `ResolveE2EESecret` failed transiently — worth a retry.
    #[error("resolve device authentication: {0}")]
    Resolve(#[source] AuthError),
    /// Recording a failed proof failed transiently.
    #[error("record rejected device authentication: {0}")]
    RecordFailure(#[source] AuthError),
    /// `CompleteE2EEAuth(true)` failed transiently after the finish opened.
    #[error("complete device authentication: {0}")]
    Complete(#[source] AuthError),
    /// The store returned a credential secret on a credential handshake, or
    /// an identity that fails `validAuthenticatedIdentity`.
    #[error("device authentication returned an invalid result")]
    InvalidAuthResult,
    /// `ServerHandshake::respond` failed (key material / ECDH).
    #[error("server hello derivation: {0}")]
    KeyMaterial(#[source] E2eeError),
}

impl HandshakeError {
    /// `errors.Is(err, ErrDeviceAuthRejected)`.
    pub fn is_rejected(&self) -> bool {
        matches!(self, HandshakeError::Rejected)
    }

    /// Whether the peer went away on its own (not a failure worth a span).
    pub fn peer_closed(&self) -> bool {
        matches!(self, HandshakeError::Read(e) if e.is_closed())
    }
}

/// `performServerE2EEHandshake` — one full exchange, deadline-bounded.
///
/// `codec` is the transport's frame codec (`io.codec()`); `keys` draws the
/// ephemeral keypair and nonce. No secret, key, or proof bytes are ever
/// logged or embedded in errors.
pub async fn run<R, W, A, K>(
    reader: &mut R,
    writer: &mut W,
    auth: &A,
    keys: &mut K,
    codec: Codec,
) -> Result<HandshakeSuccess, HandshakeError>
where
    R: FrameRead + ?Sized,
    W: FrameWrite + ?Sized,
    A: DeviceAuthStore + ?Sized,
    K: KeySource + ?Sized,
{
    match tokio::time::timeout(
        HANDSHAKE_TIMEOUT,
        exchange(reader, writer, auth, keys, codec),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => Err(HandshakeError::Timeout),
    }
}

async fn exchange<R, W, A, K>(
    reader: &mut R,
    writer: &mut W,
    auth: &A,
    keys: &mut K,
    codec: Codec,
) -> Result<HandshakeSuccess, HandshakeError>
where
    R: FrameRead + ?Sized,
    W: FrameWrite + ?Sized,
    A: DeviceAuthStore + ?Sized,
    K: KeySource + ?Sized,
{
    // 1. Client hello — plaintext frame, structural parse first so a mangled
    //    hello never touches the store.
    let raw_hello = reader.read_frame().await.map_err(HandshakeError::Read)?;
    let client_hello =
        lerdr_e2ee::handshake::parse_client_hello(&raw_hello).map_err(HandshakeError::Malformed)?;
    let selector = client_hello.selector.clone();

    // 2. Resolve the pairing secret. A rejected selector is permanent.
    let secret = match auth.resolve(&selector).await {
        Ok(secret) => secret,
        Err(err) if err.is_rejected() => return Err(HandshakeError::Rejected),
        Err(err) => return Err(HandshakeError::Resolve(err)),
    };

    // 3. Verify the client proof. On failure the store records the attempt
    //    (invitation burn bookkeeping) before the connection is refused.
    let handshake = match ServerHandshake::begin(&raw_hello, &secret) {
        Ok(handshake) => handshake,
        Err(E2eeError::ClientProofFailed) => {
            if let Err(err) = auth.complete(&selector, false).await {
                if !err.is_rejected() {
                    return Err(HandshakeError::RecordFailure(err));
                }
            }
            return Err(HandshakeError::Rejected);
        }
        Err(err) => return Err(HandshakeError::Malformed(err)),
    };

    // 4. Server hello — fresh ephemeral key + nonce, transcript proofs and
    //    session keys derived inside the sync core.
    let (hello_json, established) = handshake
        .respond(&secret, &keys.secret_key(), keys.nonce())
        .map_err(HandshakeError::KeyMaterial)?;
    let mut session = established.server_session(codec);
    writer
        .write_frame(&hello_json)
        .await
        .map_err(HandshakeError::Write)?;

    // 5. Client finish — first sealed frame; must open and parse.
    let raw_finish = reader.read_frame().await.map_err(HandshakeError::Read)?;
    let plaintext_finish = session
        .open(&raw_finish)
        .map_err(|_| HandshakeError::Malformed(E2eeError::InvalidClientFinish))?;
    parse_client_finish(&plaintext_finish).map_err(HandshakeError::Malformed)?;

    // 6. Commit — the auth record flips only now that the client proved the
    //    session keys work (invitation redemption / credential liveness).
    let outcome = match auth.complete(&selector, true).await {
        Ok(outcome) => outcome,
        Err(err) if err.is_rejected() => return Err(HandshakeError::Rejected),
        Err(err) => return Err(HandshakeError::Complete(err)),
    };
    let identity = outcome.identity;
    if !identity.is_valid() {
        return Err(HandshakeError::InvalidAuthResult);
    }
    let credential_secret = match outcome.credential_secret {
        Some(secret) if selector.kind == AuthKind::Invitation => {
            Some(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(secret))
        }
        Some(_) => return Err(HandshakeError::InvalidAuthResult),
        None => None,
    };

    // 7. Server finish — sealed, carries the committed identity.
    let finish = encode_finish(&identity, credential_secret.as_deref());
    let frame = session.seal(&finish).map_err(HandshakeError::KeyMaterial)?;
    writer
        .write_frame(&frame)
        .await
        .map_err(HandshakeError::Write)?;

    Ok(HandshakeSuccess {
        session,
        identity,
        selector,
    })
}

/// Marshal `e2ee_server_finish` (`encode_server_finish` wrapper).
fn encode_finish(identity: &AuthenticatedIdentity, credential_secret: Option<&str>) -> Vec<u8> {
    lerdr_e2ee::handshake::encode_server_finish(&ServerFinish {
        device_id: identity.device_id.clone(),
        credential_id: identity.credential_id.clone(),
        role: identity.role.as_str().to_owned(),
        locale: identity.locale.clone(),
        credential_version: identity.credential_version,
        credential_secret: credential_secret.map(str::to_owned),
    })
}

/// Compile-time guarantee: the secret type is exactly the wire's 32 bytes.
const _: [u8; SECRET_BYTES] = [0u8; SECRET_BYTES];
