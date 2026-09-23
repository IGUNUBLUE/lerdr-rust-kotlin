//! Web Push delivery worker — the `internal/push` delivery path port:
//! `Manager.Run`/`RunOnce` + `sendOne` over the queue bookkeeping in
//! [`super::push`], plus the two crypto halves the oracle gets from
//! `webpush-go` (v1.4.0):
//!
//! - VAPID key persistence (`loadOrGenerateVAPIDKeys`):
//!   `vapid_private.pem`/`vapid_public.pem` under the push dir;
//!   PKCS#8 *or* SEC1 PEM *or* raw base64url scalar/point; a missing
//!   public key is derived + written, a mismatch is fatal, private is
//!   `0600`.
//! - RFC 8291 aes128gcm encryption: P-256 ECDH against the
//!   subscription `p256dh`, auth-secret salted HKDF
//!   (`"WebPush: info\x00"`), then RFC 8188 record coding —
//!   `salt(16) | rs=4096(4 BE) | idlen(1) | ephemeral-pub(65)` +
//!   `plaintext | 0x02 | zero-pad` under AES-128-GCM.
//! - RFC 8292 VAPID JWT: hand-rolled ES256 (`{"alg":"ES256","typ":"JWT"}`
//!   / `{"aud": <endpoint origin>, "exp": now+12h, "sub": <subscriber>}`),
//!   sent as `Authorization: vapid t=<jwt>, k=<b64url-pub>`.
//! - The HTTP POST (`sendOne`): `Content-Encoding: aes128gcm`,
//!   `Content-Type: application/octet-stream`, `TTL: 300`, redirects
//!   never followed (`CheckRedirect → ErrUseLastResponse`), 10s
//!   connect+request timeout.
//!
//! Security: logs carry a 60-char endpoint truncation
//! (`truncateEndpoint`) and never key material or payload bytes. The
//! endpoint allowlist ran at `push_subscribe`; the sender still
//! re-checks `https` before dialing.

use std::collections::HashMap;
use std::io;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use aes_gcm::aead::Aead;
use aes_gcm::{Aes128Gcm, KeyInit};
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
use base64::Engine;
use hkdf::Hkdf;
use p256::ecdsa::signature::Signer;
use p256::ecdsa::{Signature, SigningKey};
use p256::elliptic_curve::sec1::ToSec1Point;
use p256::pkcs8::{
    DecodePrivateKey, DecodePublicKey, EncodePrivateKey, EncodePublicKey, LineEnding,
};
use p256::{PublicKey, SecretKey};
use sha2::Sha256;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use super::push::{
    atomic_write, retry_delay_ns, set_private_permissions, DeliveryResult, Disposition, Push,
    QueueEntry, Subscription, Timestamp, MAX_DELIVERY_ATTEMPTS,
};

/// `Run`'s ticker cadence — 250ms between queue passes.
const WORKER_TICK: Duration = Duration::from_millis(250);
/// In-flight HTTP sends cap — the oracle drains serially under
/// `queue.process`; we bound concurrency per *subscription*
/// (same-endpoint entries stay serial so `terminalSubscriptions`
/// semantics hold) and across subscriptions.
const MAX_IN_FLIGHT_GROUPS: usize = 4;
/// `http.Client{Timeout: 10s}` — whole-request deadline.
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);
/// `Options{TTL: 300}`.
const PUSH_TTL_SECONDS: u32 = 300;
/// `Options{Subscriber}` — the oracle's `sub` claim (already an
/// `https:` URL so webpush-go uses it verbatim, no `mailto:`).
const VAPID_SUBJECT: &str = "https://github.com/IGUNUBLUE/lerdr";
/// webpush-go's JWT lifetime — `time.Now().Add(12 * time.Hour)`
/// (note: the `VapidExpiration` option is ignored upstream).
const VAPID_EXPIRY: Duration = Duration::from_secs(12 * 60 * 60);
/// `MaxRecordSize` — the single aes128gcm record covers the whole body.
const RECORD_SIZE: usize = 4096;
/// `salt(16) | rs(4) | idlen(1) | keyid(65)` — the aes128gcm preamble.
const RECORD_HEADER_LEN: usize = 16 + 4 + 1 + 65;
/// `pad()` budget: `recordSize - 16 (tag) - header` — the record
/// plaintext (`message | 0x02 | zeros`) always pads out to this.
const RECORD_PLAINTEXT_LEN: usize = RECORD_SIZE - 16 - RECORD_HEADER_LEN;
const SALT_LEN: usize = 16;
const P256_UNCOMPRESSED_LEN: usize = 65;

/// Go `base64.RawURLEncoding` — URL-safe, no padding, non-strict
/// trailing bits (same engine push.rs pins).
const RAW_URL: GeneralPurpose = GeneralPurpose::new(
    &base64::alphabet::URL_SAFE,
    GeneralPurposeConfig::new()
        .with_encode_padding(false)
        .with_decode_padding_mode(DecodePaddingMode::RequireNone)
        .with_decode_allow_trailing_bits(true),
);

// ---------------------------------------------------------------------------
// VAPID key persistence — `loadOrGenerateVAPIDKeys` + the parse/
// encode helpers. `VapidKeypair` is the loaded pair: the public half
// is always the derived point (matching `m.vapidPublic`).
// ---------------------------------------------------------------------------

/// The relay's VAPID identity — `m.vapidPrivate`/`m.vapidPublic`.
/// `Debug` is safe: `SecretKey`'s `fmt` is non-exhaustive (no scalar).
#[derive(Debug)]
pub(crate) struct VapidKeypair {
    secret: SecretKey,
}

/// `ecdsa.GenerateKey(elliptic.P256())` — rejection-sample the scalar
/// from the OS RNG. (`elliptic-curve` 0.14's `Generate` trait speaks
/// `rand_core` 0.10, which `rand` 0.9's `OsRng` doesn't implement —
/// byte-filling is all we need.)
fn generate_secret() -> SecretKey {
    use rand::TryRngCore;
    loop {
        let mut scalar = [0u8; 32];
        rand::rngs::OsRng
            .try_fill_bytes(&mut scalar)
            .expect("OS RNG failure is unrecoverable");
        // `from_bytes` rejects zero / ≥N scalars — the oracle's
        // `params.N` check; P(reject) ≈ 2⁻³².
        if let Ok(secret) = SecretKey::from_bytes((&scalar).into()) {
            return secret;
        }
    }
}

impl VapidKeypair {
    /// `loadOrGenerateVAPIDKeys(pushDir)`:
    /// - both files missing → generate, write both;
    /// - private only → derive + write public (`0644`);
    /// - both → public must equal the derived point;
    /// - public only, unreadable, or unparsable → error.
    ///
    /// An existing private file is tightened to `0600`.
    pub(crate) fn load_or_generate(push_dir: &Path) -> io::Result<Self> {
        std::fs::create_dir_all(push_dir)?;
        let priv_path = push_dir.join("vapid_private.pem");
        let pub_path = push_dir.join("vapid_public.pem");

        let priv_data = read_optional(&priv_path, "read VAPID private key")?;
        let pub_data = read_optional(&pub_path, "read VAPID public key")?;

        let Some(priv_data) = priv_data else {
            if pub_data.is_some() {
                return Err(invalid(
                    "VAPID private key is missing while public key exists",
                ));
            }
            // Both missing → generate + persist.
            let pair = VapidKeypair {
                secret: generate_secret(),
            };
            let private_pem = pair.private_pem()?;
            let public_pem = pair.public_pem()?;
            atomic_write(&priv_path, private_pem.as_bytes(), 0o600)?;
            atomic_write(&pub_path, public_pem.as_bytes(), 0o644)?;
            tracing::info!("generated new VAPID key pair");
            return Ok(pair);
        };

        let text = String::from_utf8_lossy(&priv_data);
        let secret = parse_vapid_private(&text)
            .map_err(|e| invalid(format!("parse VAPID private key: {e}")))?;
        let pair = VapidKeypair { secret };
        let derived = pair.secret.public_key();

        match pub_data {
            None => {
                let public_pem = pair.public_pem()?;
                atomic_write(&pub_path, public_pem.as_bytes(), 0o644)?;
                tracing::info!("derived missing VAPID public key from existing private key");
            }
            Some(pub_data) => {
                let text = String::from_utf8_lossy(&pub_data);
                let stored = parse_vapid_public(&text)
                    .map_err(|e| invalid(format!("parse VAPID public key: {e}")))?;
                if stored.to_sec1_point(false) != derived.to_sec1_point(false) {
                    return Err(invalid("VAPID public key does not match private key"));
                }
            }
        }
        set_private_permissions(&priv_path)?;
        Ok(pair)
    }

    /// Ephemeral pair — `Push::default()` has no push dir; the oracle
    /// only ever runs persisted, so this stays a degraded-mode seam.
    fn ephemeral() -> Self {
        VapidKeypair {
            secret: generate_secret(),
        }
    }

    /// `encodeVAPIDPublic` — uncompressed point, base64url.
    pub(crate) fn public_b64(&self) -> String {
        RAW_URL.encode(self.secret.public_key().to_sec1_point(false).as_bytes())
    }

    /// `encodeVAPIDPrivate` — 32-byte scalar, base64url.
    #[allow(dead_code)] // exercised by the persistence tests
    fn private_b64(&self) -> String {
        RAW_URL.encode(self.secret.to_bytes())
    }

    /// `x509.MarshalPKCS8PrivateKey` + `pem "PRIVATE KEY"`.
    fn private_pem(&self) -> io::Result<String> {
        self.secret
            .to_pkcs8_pem(LineEnding::LF)
            .map(|z| z.to_string())
            .map_err(|e| invalid(format!("encode VAPID private key: {e}")))
    }

    /// `x509.MarshalPKIXPublicKey` + `pem "PUBLIC KEY"`.
    fn public_pem(&self) -> io::Result<String> {
        self.secret
            .public_key()
            .to_public_key_pem(LineEnding::LF)
            .map_err(|e| invalid(format!("encode VAPID public key: {e}")))
    }
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

/// `os.ReadFile` split into `None` on IsNotExist, error otherwise.
fn read_optional(path: &Path, context: &'static str) -> io::Result<Option<Vec<u8>>> {
    match std::fs::read(path) {
        Ok(data) => Ok(Some(data)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io::Error::new(e.kind(), format!("{context}: {e}"))),
    }
}

/// `parseVAPIDPrivateKey` — raw base64url scalar (webpush-go format)
/// or PEM. `SecretKey::from_pem` dispatches on the label the way Go
/// tries `ParseECPrivateKey` (SEC1 `"EC PRIVATE KEY"`) then
/// `ParsePKCS8PrivateKey` (`"PRIVATE KEY"`).
fn parse_vapid_private(data: &str) -> Result<SecretKey, String> {
    let data = data.trim();
    if !data.starts_with("-----BEGIN") {
        let scalar = RAW_URL
            .decode(data)
            .map_err(|e| format!("decode private scalar: {e}"))?;
        // Go `big.Int.SetBytes` + `0 < d < N`: any length decodes,
        // >32 always exceeds N. Left-pad to the field size.
        if scalar.len() > 32 {
            return Err("private scalar is outside the P-256 range".to_owned());
        }
        let mut padded = [0u8; 32];
        padded[32 - scalar.len()..].copy_from_slice(&scalar);
        return SecretKey::from_bytes((&padded).into())
            .map_err(|_| "private scalar is outside the P-256 range".to_owned());
    }
    // PEM — Go tries `x509.ParseECPrivateKey` (SEC1 `"EC PRIVATE
    // KEY"`) then `x509.ParsePKCS8PrivateKey` (`"PRIVATE KEY"`), so do
    // we; `p256` validates the scalar either way.
    if let Ok(secret) = SecretKey::from_sec1_pem(data) {
        return Ok(secret);
    }
    SecretKey::from_pkcs8_pem(data).map_err(|e| format!("parse EC key: {e}"))
}

/// `parseVAPIDPublicKey` — raw base64url SEC1 point (webpush-go
/// format) or PEM `SubjectPublicKeyInfo`.
fn parse_vapid_public(data: &str) -> Result<PublicKey, String> {
    let data = data.trim();
    if !data.starts_with("-----BEGIN") {
        let point = RAW_URL
            .decode(data)
            .map_err(|e| format!("decode public point: {e}"))?;
        return PublicKey::from_sec1_bytes(&point)
            .map_err(|_| "invalid P-256 public point".to_owned());
    }
    PublicKey::from_public_key_pem(data).map_err(|e| format!("parse public key: {e}"))
}

// ---------------------------------------------------------------------------
// RFC 8291 encryption — webpush-go's `encrypt`: single-record
// aes128gcm (RFC 8188 body) sized to `MaxRecordSize`.
// ---------------------------------------------------------------------------

/// `decodeSubscriptionKey` — pad to a multiple of 4, then standard
/// base64, then URL-safe base64 (webpush-go tries both alphabets).
fn decode_subscription_key(key: &str) -> Result<Vec<u8>, SendError> {
    let mut padded = key.to_owned();
    let rem = key.len() % 4;
    if rem != 0 {
        padded.extend(std::iter::repeat_n('=', 4 - rem));
    }
    base64::engine::general_purpose::STANDARD
        .decode(&padded)
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(&padded))
        .map_err(|_| SendError::Local("subscription key is not valid base64"))
}

/// The aes128gcm body for one subscription — random salt + ephemeral
/// key, then [`encrypt_record_with`].
fn encrypt_record(subscription: &Subscription, payload: &[u8]) -> Result<Vec<u8>, SendError> {
    let mut salt = [0u8; SALT_LEN];
    use rand::TryRngCore;
    rand::rngs::OsRng
        .try_fill_bytes(&mut salt)
        .map_err(|_| SendError::Local("salt generation failed"))?;
    let ephemeral = generate_secret();
    encrypt_record_with(subscription, payload, salt, &ephemeral)
}

/// `SendNotificationWithContext`'s crypto half, byte-for-byte:
///
/// ```text
/// ecdh    = ECDH(ephemeral, subscription p256dh)         // x-coord, 32B
/// ikm     = HKDF(salt=auth,   ikm=ecdh, "WebPush: info\x00"+p256dh+eph_pub, 32)
/// cek     = HKDF(salt=salt16, ikm=ikm,  "Content-Encoding: aes128gcm\x00", 16)
/// nonce   = HKDF(salt=salt16, ikm=ikm,  "Content-Encoding: nonce\x00", 12)
/// body    = salt16 | rs=4096 | 0x41 | eph_pub | AES-128-GCM(cek,nonce, msg|0x02|0*)
/// ```
fn encrypt_record_with(
    subscription: &Subscription,
    payload: &[u8],
    salt: [u8; SALT_LEN],
    ephemeral: &SecretKey,
) -> Result<Vec<u8>, SendError> {
    let auth = decode_subscription_key(&subscription.keys.auth)?;
    let p256dh = decode_subscription_key(&subscription.keys.p256dh)?;
    // webpush-go `pad`: `message | 0x02` must fit the padded record.
    if payload.len() + 1 > RECORD_PLAINTEXT_LEN {
        return Err(SendError::Local("payload has exceeded the maximum length"));
    }
    let peer = PublicKey::from_sec1_bytes(&p256dh).map_err(|_| {
        SendError::Local("Unmarshal Error: Public key is not a valid point on the curve")
    })?;
    let ephemeral_public = ephemeral.public_key().to_sec1_point(false);
    let ephemeral_public = ephemeral_public.as_bytes();
    debug_assert_eq!(ephemeral_public.len(), P256_UNCOMPRESSED_LEN);

    // ECDH shared secret — the 32-byte x coordinate.
    let shared = ephemeral.diffie_hellman(&peer);
    let ecdh = shared.raw_secret_bytes();

    // IKM — `hkdf.New(sha256, ecdh, auth, "WebPush: info\x00"+dh+ephPub)`.
    let mut info = Vec::with_capacity(15 + p256dh.len() + ephemeral_public.len());
    info.extend_from_slice(b"WebPush: info\x00");
    info.extend_from_slice(&p256dh);
    info.extend_from_slice(ephemeral_public);
    let mut ikm = [0u8; 32];
    Hkdf::<Sha256>::new(Some(&auth), ecdh)
        .expand(&info, &mut ikm)
        .map_err(|_| SendError::Local("hkdf ikm"))?;

    let mut cek = [0u8; 16];
    Hkdf::<Sha256>::new(Some(&salt), &ikm)
        .expand(b"Content-Encoding: aes128gcm\x00", &mut cek)
        .map_err(|_| SendError::Local("hkdf cek"))?;
    let mut nonce = [0u8; 12];
    Hkdf::<Sha256>::new(Some(&salt), &ikm)
        .expand(b"Content-Encoding: nonce\x00", &mut nonce)
        .map_err(|_| SendError::Local("hkdf nonce"))?;

    let cipher = Aes128Gcm::new_from_slice(&cek).map_err(|_| SendError::Local("aes128gcm key"))?;
    // `message | 0x02` then zero padding to the record budget.
    let mut record = Vec::with_capacity(RECORD_PLAINTEXT_LEN);
    record.extend_from_slice(payload);
    record.push(0x02);
    record.resize(RECORD_PLAINTEXT_LEN, 0);
    let ciphertext = cipher
        .encrypt((&nonce).into(), record.as_slice())
        .map_err(|_| SendError::Local("aes128gcm seal"))?;

    let mut body = Vec::with_capacity(RECORD_SIZE);
    body.extend_from_slice(&salt);
    body.extend_from_slice(&(RECORD_SIZE as u32).to_be_bytes());
    body.push(ephemeral_public.len() as u8);
    body.extend_from_slice(ephemeral_public);
    body.extend_from_slice(&ciphertext);
    debug_assert_eq!(body.len(), RECORD_SIZE);
    Ok(body)
}

// ---------------------------------------------------------------------------
// RFC 8292 VAPID — `getVAPIDAuthorizationHeader`: ES256 JWT
// `{"aud": origin, "exp": now+12h, "sub": subject}` sent as
// `vapid t=<jwt>, k=<b64url-pub>`.
// ---------------------------------------------------------------------------

/// `subURL.Scheme + "://" + subURL.Host` — the `aud` claim. Authority
/// is taken verbatim (Go keeps an explicit `:443` in `Host`).
fn endpoint_origin(endpoint: &str) -> Result<String, SendError> {
    let (scheme, rest) = endpoint
        .split_once("://")
        .ok_or(SendError::Local("push endpoint has no scheme"))?;
    let authority = rest.split('/').next().unwrap_or_default();
    if scheme.is_empty() || authority.is_empty() {
        return Err(SendError::Local("push endpoint has no authority"));
    }
    Ok(format!("{scheme}://{authority}"))
}

/// The signed JWT — `jwt.MapClaims` marshals keys sorted, so the
/// payload is `{"aud":…,"exp":…,"sub":…}` byte-for-byte. ES256 is the
/// 64-byte `r||s` signature over `b64(header).b64(payload)`.
fn vapid_jwt(endpoint: &str, pair: &VapidKeypair, now: Timestamp) -> Result<String, SendError> {
    let aud = endpoint_origin(endpoint)?;
    let exp = now.unix_seconds() + VAPID_EXPIRY.as_secs() as i64;
    let header = RAW_URL.encode(r#"{"alg":"ES256","typ":"JWT"}"#);
    let claims = RAW_URL.encode(format!(
        r#"{{"aud":"{aud}","exp":{exp},"sub":"{VAPID_SUBJECT}"}}"#
    ));
    let signing_input = format!("{header}.{claims}");
    let signer = SigningKey::from(&pair.secret);
    let signature: Signature = signer.sign(signing_input.as_bytes());
    Ok(format!(
        "{signing_input}.{}",
        RAW_URL.encode(signature.to_bytes())
    ))
}

/// `getVAPIDAuthorizationHeader` — `vapid t=<jwt>, k=<b64url-pub>`.
fn vapid_authorization(
    endpoint: &str,
    pair: &VapidKeypair,
    now: Timestamp,
) -> Result<String, SendError> {
    let jwt = vapid_jwt(endpoint, pair, now)?;
    Ok(format!("vapid t={jwt}, k={}", pair.public_b64()))
}

// ---------------------------------------------------------------------------
// The send half — `sendOne`: encrypt → POST → classify the result.
// ---------------------------------------------------------------------------

/// `pushError` + transport failure — the oracle's error taxonomy:
/// status codes drive terminal/retryable classification; transport
/// failures are Go `net.Error` (retryable); local failures (bad keys,
/// oversized payload, disallowed scheme) are neither → dropped.
#[derive(Debug)]
pub(crate) enum SendError {
    /// Non-2xx HTTP status — `pushError{statusCode}`.
    Status(u16),
    /// Timeout/connect/transport failure — Go `net.Error` → retryable.
    Transport(&'static str),
    /// Client-side refusal — never reaches the wire → not retryable.
    Local(&'static str),
}

impl SendError {
    /// `isTerminalError` — only 404/410 mean the subscription is gone.
    /// Auth failures (401/403) can be relay-side VAPID config and must
    /// not destroy the subscription.
    fn is_terminal(&self) -> bool {
        matches!(self, SendError::Status(404 | 410))
    }

    /// `retryablePushError` — 408/429/5xx statuses or transport
    /// failures reschedule; everything else drops.
    fn is_retryable(&self) -> bool {
        match self {
            SendError::Status(status) => *status == 408 || *status == 429 || *status >= 500,
            SendError::Transport(_) => true,
            SendError::Local(_) => false,
        }
    }
}

impl std::fmt::Display for SendError {
    /// Never carries endpoints, key material, or payload bytes.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SendError::Status(status) => write!(f, "push service returned {status}"),
            SendError::Transport(what) => write!(f, "push request failed ({what})"),
            SendError::Local(what) => write!(f, "push send refused ({what})"),
        }
    }
}

/// The sender seam — `m.sendPush`. Production impl is
/// [`WebPushSender`]; tests inject a stub. One call = one POST.
///
/// Spelled as `impl Future + Send` rather than `async fn` because the
/// worker `JoinSet::spawn`s across the call — the `Send` bound must be
/// part of the signature.
pub(crate) trait PushSender: Send + Sync + 'static {
    #[allow(clippy::manual_async_fn)]
    fn send<'a>(
        &'a self,
        subscription: &'a Subscription,
        payload: &'a [u8],
    ) -> impl std::future::Future<Output = Result<(), SendError>> + Send + 'a;
}

/// `sendOne` over reqwest — rustls-only client, redirects off
/// (`CheckRedirect → ErrUseLastResponse`), 10s timeouts.
struct WebPushSender {
    client: reqwest::Client,
    vapid: VapidKeypair,
}

impl WebPushSender {
    fn new(vapid: VapidKeypair) -> io::Result<Self> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(HTTP_TIMEOUT)
            .connect_timeout(HTTP_TIMEOUT)
            .build()
            .map_err(io::Error::other)?;
        Ok(WebPushSender { client, vapid })
    }
}

impl PushSender for WebPushSender {
    #[allow(clippy::manual_async_fn)]
    fn send<'a>(
        &'a self,
        subscription: &'a Subscription,
        payload: &'a [u8],
    ) -> impl std::future::Future<Output = Result<(), SendError>> + Send + 'a {
        async move {
            // Re-check the subscribe-time allowlist's core: HTTPS only.
            if !subscription.endpoint.starts_with("https://") {
                return Err(SendError::Local("push endpoint is not https"));
            }
            let body = encrypt_record(subscription, payload)?;
            let authorization =
                vapid_authorization(&subscription.endpoint, &self.vapid, Timestamp::now())?;
            let response = self
                .client
                .post(subscription.endpoint.as_str())
                .header("Content-Encoding", "aes128gcm")
                .header("Content-Type", "application/octet-stream")
                .header("TTL", PUSH_TTL_SECONDS.to_string())
                .header("Authorization", authorization)
                .body(body)
                .send()
                .await
                .map_err(|e| {
                    // `net.Error` → retryable. reqwest send errors are
                    // all transport-side here (timeout/connect/body/
                    // IO); URL parsing can't fail post-allowlist.
                    if e.is_timeout() {
                        SendError::Transport("timeout")
                    } else if e.is_connect() {
                        SendError::Transport("connect")
                    } else {
                        SendError::Transport("request")
                    }
                })?;
            let status = response.status().as_u16();
            if (200..300).contains(&status) {
                return Ok(());
            }
            Err(SendError::Status(status))
        }
    }
}

// ---------------------------------------------------------------------------
// The worker — `Manager.Run` (250ms tick + wake) driving `RunOnce`'s
// `processDue` pass. Sends run with bounded concurrency across
// subscription keys; same-endpoint entries stay serial so a terminal
// response still prunes the rest of the pass's queue for that
// endpoint (`terminalSubscriptions`).
// ---------------------------------------------------------------------------

/// Spawn the delivery worker — `go m.Run(ctx)` plus the VAPID half of
/// `NewManager`. The key load-or-generate runs synchronously so a bad
/// key file fails startup like the oracle; the returned handle is the
/// task. `cancel` mirrors `ctx`.
pub(crate) fn spawn_push_worker(
    push: Push,
    cancel: CancellationToken,
) -> io::Result<tokio::task::JoinHandle<()>> {
    let vapid = match push.push_dir() {
        Some(dir) => VapidKeypair::load_or_generate(&dir)?,
        None => VapidKeypair::ephemeral(),
    };
    let sender = Arc::new(WebPushSender::new(vapid)?);
    Ok(spawn_worker(push, sender, cancel))
}

/// The injectable half of [`spawn_push_worker`] — tests drive the
/// loop/drain with a stub [`PushSender`].
fn spawn_worker<S: PushSender>(
    push: Push,
    sender: Arc<S>,
    cancel: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(worker_loop(push, sender, cancel))
}

/// `Manager.Run` — select on cancel / 250ms tick / wake, drain after
/// each. A drain error logs once and the loop keeps going (the oracle
/// warns and continues; entries survive in the queue).
async fn worker_loop<S: PushSender>(push: Push, sender: Arc<S>, cancel: CancellationToken) {
    let wake = push.delivery_wake();
    let mut ticker =
        tokio::time::interval_at(tokio::time::Instant::now() + WORKER_TICK, WORKER_TICK);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            () = cancel.cancelled() => break,
            _ = ticker.tick() => {}
            () = wake.notified() => {}
        }
        match drain_once(&push, &sender, Timestamp::now(), &cancel).await {
            Ok(results) => {
                for result in &results {
                    debug!(
                        disposition = ?result.disposition,
                        attempts = result.attempts,
                        endpoint = %truncate_endpoint(&result.endpoint),
                        "push delivery",
                    );
                }
            }
            // `Run` warns and keeps ticking; nothing is dropped by an
            // error here (queue mutations already applied are kept).
            Err(code) => warn!("push queue processing failed: {code}"),
        }
    }
}

/// `truncateEndpoint` — 60 bytes + `...` (endpoint strings carry no
/// keys; the truncation bounds log noise). Backed off to a char
/// boundary so a multibyte tail can't panic the logger.
fn truncate_endpoint(endpoint: &str) -> String {
    if endpoint.len() > 60 {
        let mut cut = 60;
        while !endpoint.is_char_boundary(cut) {
            cut -= 1;
        }
        return format!("{}...", &endpoint[..cut]);
    }
    endpoint.to_owned()
}

/// `subscriptionKey` — `deviceID + "\x00" + endpoint` groups entries
/// that share the terminal-fate rule.
fn subscription_key(subscription: &Subscription) -> String {
    format!("{}\x00{}", subscription.device_id, subscription.endpoint)
}

/// `RunOnce`'s `processDue` pass: snapshot the due entries, send with
/// bounded concurrency (≤ [`MAX_IN_FLIGHT_GROUPS`] subscription groups
/// in flight), apply results as they land, then the `finish` half —
/// pruned-subscription recovery + the `retracting`/`active` sweep.
/// Returns the pass's results; `Err` mirrors `finish(err)` after the
/// queue mutations have been applied.
pub(crate) async fn drain_once<S: PushSender>(
    push: &Push,
    sender: &Arc<S>,
    now: Timestamp,
    cancel: &CancellationToken,
) -> Result<Vec<DeliveryResult>, &'static str> {
    if !push.is_reconciled() {
        return Ok(Vec::new());
    }
    // `dueEntries` snapshot → per-subscription groups (stable order).
    let mut groups: Vec<Vec<QueueEntry>> = Vec::new();
    let mut group_of: HashMap<String, usize> = HashMap::new();
    for entry in push.due_entries(now) {
        let key = subscription_key(&entry.subscription);
        match group_of.get(&key) {
            Some(&index) => groups[index].push(entry),
            None => {
                group_of.insert(key, groups.len());
                groups.push(vec![entry]);
            }
        }
    }
    let mut pending = groups.into_iter();
    let mut tasks: JoinSet<Result<Vec<DeliveryResult>, &'static str>> = JoinSet::new();
    let mut results: Vec<DeliveryResult> = Vec::new();
    let mut pass_error: Option<&'static str> = None;
    let mut cancelled = false;
    loop {
        while tasks.len() < MAX_IN_FLIGHT_GROUPS {
            let Some(group) = pending.next() else {
                break;
            };
            let push = push.clone();
            let sender = sender.clone();
            tasks.spawn(async move { process_group(&push, &sender, group, now).await });
        }
        if tasks.is_empty() {
            break;
        }
        tokio::select! {
            () = cancel.cancelled() => {
                // `ctx.Err()` mid-pass: abort in-flight sends and stop
                // dispatching — queued entries stay queued.
                cancelled = true;
                tasks.abort_all();
                while tasks.join_next().await.is_some() {}
                break;
            }
            joined = tasks.join_next() => {
                match joined {
                    Some(Ok(Ok(mut group_results))) => results.append(&mut group_results),
                    // `accepted`/finish error — the oracle's
                    // `finish(err)` aborts the pass outright; queued
                    // entries stay queued for the next pass.
                    Some(Ok(Err(code))) => {
                        pass_error = Some(code);
                        tasks.abort_all();
                        while tasks.join_next().await.is_some() {}
                        break;
                    }
                    Some(Err(e)) if !e.is_cancelled() => {
                        warn!(error = %e, "push delivery task failed");
                    }
                    _ => {}
                }
            }
        }
    }
    // `finish` — prune recovery runs even on a cancelled/failed pass;
    // `finish`'s `flush()` then persists the pass's queue mutations
    // once. A persist failure masks the recovery error like the
    // oracle's `persistErr` preference.
    let recover_result = push.recover_pruned(&results, now);
    push.flush_queue()?;
    recover_result?;
    // `RunOnce` skips the epilogue when `processDue` returned an
    // error (`finish(ctx.Err())`/`finish(err)` propagate first). A
    // cancelled pass reports like the oracle's `Run` filter
    // (`!errors.Is(err, context.Canceled)`) — nothing to warn on.
    if let Some(code) = pass_error {
        return Err(code);
    }
    if cancelled {
        return Ok(results);
    }
    push.sweep_keys(&results)?;
    Ok(results)
}

/// One subscription group's serial half of the pass — the oracle's
/// per-entry `processDue` body for a single `terminalSubscriptions`
/// key. `terminal` stands in for the map entry: after a 404/410 the
/// rest of this endpoint's entries prune without a send.
async fn process_group<S: PushSender>(
    push: &Push,
    sender: &Arc<S>,
    entries: Vec<QueueEntry>,
    now: Timestamp,
) -> Result<Vec<DeliveryResult>, &'static str> {
    let mut results = Vec::with_capacity(entries.len());
    let mut terminal = false;
    for entry in entries {
        let key = &entry.event.key;
        if terminal {
            push.finish_entry(&entry, Disposition::Pruned, now);
            results.push(DeliveryResult {
                key: key.clone(),
                endpoint: entry.subscription.endpoint.clone(),
                disposition: Disposition::Pruned,
                attempts: entry.attempts,
                next_attempt: None,
                event: entry.event.clone(),
                subscription: entry.subscription.clone(),
            });
            continue;
        }
        if now >= entry.event.expires_at {
            push.finish_entry(&entry, Disposition::Expired, now);
            results.push(DeliveryResult {
                key: key.clone(),
                endpoint: entry.subscription.endpoint.clone(),
                disposition: Disposition::Expired,
                attempts: entry.attempts,
                next_attempt: None,
                event: entry.event.clone(),
                subscription: entry.subscription.clone(),
            });
            continue;
        }
        if !push.entry_current(key, now) {
            push.finish_entry(&entry, Disposition::Stale, now);
            results.push(DeliveryResult {
                key: key.clone(),
                endpoint: entry.subscription.endpoint.clone(),
                disposition: Disposition::Stale,
                attempts: entry.attempts,
                next_attempt: None,
                event: entry.event.clone(),
                subscription: entry.subscription.clone(),
            });
            continue;
        }
        let attempts = entry.attempts + 1;
        match sender.send(&entry.subscription, &entry.event.payload).await {
            Ok(()) => {
                push.finish_entry(&entry, Disposition::Accepted, now);
                // `accepted` callback — MarkAccepted starts the
                // cooldown slot; an error aborts the pass.
                push.note_accepted(key, now)?;
                results.push(DeliveryResult {
                    key: key.clone(),
                    endpoint: entry.subscription.endpoint.clone(),
                    disposition: Disposition::Accepted,
                    attempts,
                    next_attempt: None,
                    event: entry.event.clone(),
                    subscription: entry.subscription.clone(),
                });
            }
            Err(error) if error.is_terminal() => {
                terminal = true;
                warn!(
                    endpoint = %truncate_endpoint(&entry.subscription.endpoint),
                    error = %error,
                    "push subscription pruned",
                );
                push.finish_entry(&entry, Disposition::Pruned, now);
                results.push(DeliveryResult {
                    key: key.clone(),
                    endpoint: entry.subscription.endpoint.clone(),
                    disposition: Disposition::Pruned,
                    attempts,
                    next_attempt: None,
                    event: entry.event.clone(),
                    subscription: entry.subscription.clone(),
                });
            }
            Err(error) => {
                let next = now.add_ns(retry_delay_ns(attempts));
                if error.is_retryable()
                    && attempts < MAX_DELIVERY_ATTEMPTS
                    && next < entry.event.expires_at
                {
                    push.reschedule_entry(&entry, attempts, next);
                    results.push(DeliveryResult {
                        key: key.clone(),
                        endpoint: entry.subscription.endpoint.clone(),
                        disposition: Disposition::Retrying,
                        attempts,
                        next_attempt: Some(next),
                        event: entry.event.clone(),
                        subscription: entry.subscription.clone(),
                    });
                    continue;
                }
                warn!(
                    endpoint = %truncate_endpoint(&entry.subscription.endpoint),
                    error = %error,
                    "push delivery dropped",
                );
                push.finish_entry(&entry, Disposition::Dropped, now);
                results.push(DeliveryResult {
                    key: key.clone(),
                    endpoint: entry.subscription.endpoint.clone(),
                    disposition: Disposition::Dropped,
                    attempts,
                    next_attempt: None,
                    event: entry.event.clone(),
                    subscription: entry.subscription.clone(),
                });
            }
        }
    }
    Ok(results)
}

// ---------------------------------------------------------------------------
// Tests — VAPID persistence, the aes128gcm golden vector, JWT shape +
// signature, and the drain pass against a stub sender (no network).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::push::{PublishRequest, PushEventKey, SubscriptionKeys};
    use p256::ecdsa::signature::Verifier;
    use p256::ecdsa::VerifyingKey;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    // -- helpers ----------------------------------------------------------

    fn sub(device: &str, endpoint: &str) -> Subscription {
        Subscription {
            endpoint: endpoint.to_owned(),
            keys: SubscriptionKeys {
                p256dh: "key".to_owned(),
                auth: "auth".to_owned(),
            },
            device_id: device.to_owned(),
            locale: "en".to_owned(),
            platform: "other".to_owned(),
            user_agent: String::new(),
            notify_finished: false,
            client_id: "client".to_owned(),
        }
    }

    fn question_key(device: &str, event_id: &str) -> PushEventKey {
        PushEventKey {
            device_id: device.to_owned(),
            server_session_id: "primary".to_owned(),
            pane_id: "pane-1".to_owned(),
            terminal_id: "term-1".to_owned(),
            agent_session_id: "sess-1".to_owned(),
            generation: 0,
            event_id: event_id.to_owned(),
            interaction_revision: 1,
            category: "question".to_owned(),
        }
    }

    fn publish(push: &Push, key: PushEventKey, now: Timestamp) {
        let result = push
            .publish(PublishRequest {
                key,
                preview: "question",
                created_at: Some(now),
                expires_at: Some(now.add_ns(60 * 1_000_000_000)),
            })
            .expect("publish");
        assert!(result.queued > 0, "publish queued nothing");
    }

    /// Publish + hop `now` past the 2s default settle.
    fn publish_due(push: &Push, key: PushEventKey, now: Timestamp) -> Timestamp {
        publish(push, key, now);
        now.add_ns(3 * 1_000_000_000)
    }

    /// Stub `PushSender` — recorded calls, canned outcomes, optional
    /// per-call gate for in-flight tests. Never touches the network.
    #[derive(Default)]
    struct StubSender {
        calls: Mutex<Vec<String>>,
        outcomes: Mutex<VecDeque<Result<(), SendError>>>,
    }

    impl StubSender {
        fn with_outcomes(outcomes: Vec<Result<(), SendError>>) -> Self {
            StubSender {
                calls: Mutex::new(Vec::new()),
                outcomes: Mutex::new(outcomes.into()),
            }
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl PushSender for StubSender {
        #[allow(clippy::manual_async_fn)]
        fn send<'a>(
            &'a self,
            subscription: &'a Subscription,
            _payload: &'a [u8],
        ) -> impl std::future::Future<Output = Result<(), SendError>> + Send + 'a {
            let outcome = self.outcomes.lock().unwrap().pop_front().unwrap_or(Ok(()));
            self.calls
                .lock()
                .unwrap()
                .push(subscription.endpoint.clone());
            async move { outcome }
        }
    }

    fn cancel() -> CancellationToken {
        CancellationToken::new()
    }

    // -- VAPID persistence -------------------------------------------------

    #[test]
    fn vapid_generates_persists_and_reloads() {
        let dir = tempfile::tempdir().unwrap();
        let first = VapidKeypair::load_or_generate(dir.path()).unwrap();
        assert!(dir.path().join("vapid_private.pem").exists());
        assert!(dir.path().join("vapid_public.pem").exists());
        // PKCS#8 + PKIX PEM labels, like x509.Marshal* + pem.Encode.
        let private_pem = std::fs::read_to_string(dir.path().join("vapid_private.pem")).unwrap();
        assert!(private_pem.starts_with("-----BEGIN PRIVATE KEY-----"));
        let public_pem = std::fs::read_to_string(dir.path().join("vapid_public.pem")).unwrap();
        assert!(public_pem.starts_with("-----BEGIN PUBLIC KEY-----"));
        let reloaded = VapidKeypair::load_or_generate(dir.path()).unwrap();
        assert_eq!(first.public_b64(), reloaded.public_b64());
    }

    #[cfg(unix)]
    #[test]
    fn vapid_derives_missing_public_and_tightens_perms() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let pair = VapidKeypair::ephemeral();
        let priv_path = dir.path().join("vapid_private.pem");
        std::fs::write(&priv_path, pair.private_pem().unwrap()).unwrap();
        std::fs::set_permissions(&priv_path, std::fs::Permissions::from_mode(0o644)).unwrap();

        let loaded = VapidKeypair::load_or_generate(dir.path()).unwrap();
        assert_eq!(loaded.public_b64(), pair.public_b64());
        // Public PEM written, private untouched content-wise, modes
        // are the oracle's 600/644.
        let public_pem = std::fs::read_to_string(dir.path().join("vapid_public.pem")).unwrap();
        assert!(public_pem.starts_with("-----BEGIN PUBLIC KEY-----"));
        assert_eq!(
            std::fs::metadata(&priv_path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(dir.path().join("vapid_public.pem"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o644
        );
        // Reload — now both files exist and agree.
        let again = VapidKeypair::load_or_generate(dir.path()).unwrap();
        assert_eq!(again.public_b64(), pair.public_b64());
    }

    #[test]
    fn vapid_rejects_public_only() {
        let dir = tempfile::tempdir().unwrap();
        let pair = VapidKeypair::ephemeral();
        std::fs::write(
            dir.path().join("vapid_public.pem"),
            pair.public_pem().unwrap(),
        )
        .unwrap();
        let err = VapidKeypair::load_or_generate(dir.path()).unwrap_err();
        assert!(
            err.to_string().contains("private key is missing"),
            "error = {err}"
        );
    }

    #[test]
    fn vapid_rejects_mismatched_pair() {
        let dir = tempfile::tempdir().unwrap();
        let first = VapidKeypair::ephemeral();
        let other = VapidKeypair::ephemeral();
        std::fs::write(
            dir.path().join("vapid_private.pem"),
            first.private_pem().unwrap(),
        )
        .unwrap();
        std::fs::write(
            dir.path().join("vapid_public.pem"),
            other.public_pem().unwrap(),
        )
        .unwrap();
        let err = VapidKeypair::load_or_generate(dir.path()).unwrap_err();
        assert!(err.to_string().contains("does not match"), "error = {err}");
    }

    #[test]
    fn vapid_rejects_unparsable_private_without_writing_public() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("vapid_private.pem"), "not-a-key\n").unwrap();
        let err = VapidKeypair::load_or_generate(dir.path()).unwrap_err();
        assert!(
            err.to_string().contains("parse VAPID private key"),
            "error = {err}"
        );
        assert!(!dir.path().join("vapid_public.pem").exists());
    }

    #[test]
    fn vapid_parses_raw_scalar_and_sec1_private() {
        let dir = tempfile::tempdir().unwrap();
        let pair = VapidKeypair::ephemeral();
        // webpush-go format: bare base64url 32-byte scalar.
        std::fs::write(dir.path().join("vapid_private.pem"), pair.private_b64()).unwrap();
        let loaded = VapidKeypair::load_or_generate(dir.path()).unwrap();
        assert_eq!(loaded.public_b64(), pair.public_b64());
        // Raw base64url public point too.
        let dir2 = tempfile::tempdir().unwrap();
        std::fs::write(dir2.path().join("vapid_private.pem"), pair.private_b64()).unwrap();
        std::fs::write(dir2.path().join("vapid_public.pem"), pair.public_b64()).unwrap();
        let loaded2 = VapidKeypair::load_or_generate(dir2.path()).unwrap();
        assert_eq!(loaded2.public_b64(), pair.public_b64());
        // SEC1 "EC PRIVATE KEY" PEM (x509.MarshalECPrivateKey format).
        let dir3 = tempfile::tempdir().unwrap();
        let sec1_pem = pair.secret.to_sec1_pem(LineEnding::LF).unwrap().to_string();
        std::fs::write(dir3.path().join("vapid_private.pem"), sec1_pem).unwrap();
        let loaded3 = VapidKeypair::load_or_generate(dir3.path()).unwrap();
        assert_eq!(loaded3.public_b64(), pair.public_b64());
    }

    #[test]
    fn vapid_rejects_out_of_range_scalar() {
        // Zero and ≥N scalars fail the oracle's `0 < d < N` check.
        for scalar in [[0u8; 32], [0xffu8; 32]] {
            let err = parse_vapid_private(&RAW_URL.encode(scalar)).unwrap_err();
            assert!(err.contains("outside the P-256 range"), "error = {err}");
        }
    }

    // -- aes128gcm ---------------------------------------------------------

    /// Golden vector produced independently of this implementation
    /// (Python `cryptography` — ECDH + HKDF-SHA256 + AES-128-GCM),
    /// pinning webpush-go's exact record layout: 16B salt, BE rs=4096,
    /// 65B ephemeral point, `message | 0x02 | zeros` to 3994B + 16B tag.
    #[test]
    fn aes128gcm_matches_golden_vector() {
        let auth_b64 = "AQIDBAUGBwgJCgsMDQ4PEA";
        let p256dh_b64 =
            "BMZVnUFt-1avcU8UbZF8JKv4GLL7EhYEEpZJhIIwotJYsqbYLcbGc0zwkv-qn8AS8Q9wCNOVKgjVeX6F_qul2Xc";
        let eph_scalar_b64 = "QEFCQ0RFRkdISUpLTE1OT1BRUlNUVVZXWFlaW1xdXl8";
        let salt_b64 = "gIGCg4SFhoeIiYqLjI2Ojw";
        let message = br#"{"title":"Hello","body":"web push vector"}"#;
        let expected_b64 = "gIGCg4SFhoeIiYqLjI2OjwAAEABBBGjsfPCM1BBuQ7FN6JVCZSK9CkUVDAJ-RceVNDTXR-e64685qI677oZ5u2HnhFw6icubWjI3w_2wsFh9uvQVEY0EzItf69gpLzEWSbaO_FqSkdYc3oMSgVLe_wvJ1_1Aa5O5GGlRFbtytrkE7VE16-GuLJQDhrR9izoFhrCDDBeiLZGjWiA3dL0aqskbI1tQThL-7Ofald3oY4ZbvDPfhveXM8kQnVX4ljEJ2eP7XOA7YFpRwQpqX854rbcBeO1DaxzEdUoXsyw9MBBXstUl57D3WnPBqcFf2iU9tZMwBDHIMtD9jeV6-oKrJ6T58fdxk3JmTNpeY698oqLDQK-dCQxdUKDFHXjYJ54ftuqXkX9JVrlT0c-pmvTzUKmTWikRTfH2DcRHDYroVuw-DYSfqJllje4ClAGCvHgef0TnbOj7bkFUHLFjmjsxm2klNGtTSS0u9ScYacOTrRv5KTfvCTEccoC3CHy0hvQkD4bGfOM7TIE8GDxKxS4YVn5xivFRHJfasO2ZutnVsfiSm2HA9FXi4rUTcoR9iXzSwZr3tnwl1V7W5sGyuIuobQh1PGoxD1cbeq6WxTHPQzkBQon_kcKNMVZ0BNPL0vEbsQigmyimyjizsblLCp-CacECZ-fXM4iU_tToAZzZlPuY4Xcd6otHUlPPV0hBvEb_XJo8VoHFAdhF6GLm4JyOe8uSsXNRu6Bkmj6llf_I8qIfAce9K7619ePot7Ec-egJ0TOkbwjLUx_1af7YVlYgw5FbGLRqUEGlLjJdi-k8ajMHq7oyCId79Nh5jq8RdmaSs3ouCI_KfzQQAKMG2jNfvAEadQRXUWRNcYthD_Qqgqs5yW6HizlV5QCuPXygrSlQpPeLn_nO0xbWbOWcZRM5vkplDsri0Q5659gniqavOdi4Xp8hnp4MG7EccM0Pq6M5nPxPCchJHGcwcKyjOZ8j28c-TGXzKgYdydsUALq20ZrBFZuantAgYwceIlsVgEwz6VM34Zt97UoL9FD6L-HV1qkDz29OuSaCDxGnKZ8f07UFNmF5MEtc8CzZFpz4RKleFX4fIhBDtQSlWgV6sIAmcNlFm040ZYA2sFzPE1umh24v_6csd3J__SKigGrp1yYnz2FOc1NZT7hIUCbQ0hw6onPzdlArJ2JtxZ5_XAqwUQwPqKMvtVJBh_WotiEpG2MOLBr31W6YKe2_DTg777j8cw08ucKx4MLxQE_W5QKEmJY-BEoxIimy_OkY9Odp0LHCmEY3zuxFp5RTvC_2Xx3vI-81DK68t-OzpxMd60bnx8IF0O-TRph756_DNZ8TTvCAeJoPyblG4OVg_5c2C0H9CCKMB7vFRxiYpjkbLN5cpmqvjJwCgqgjqPCvez7YKQujPS1Icik83s4Mhq_QQ4dDhDCMDvLmZQG8RVn3t7iSw_tsvF5koykt3YWKtgsgu5AxwiOUOhPE-PyosLphjnJdOVswjUWbtvL6k9USW2fQvtT_Zfknrbunua2zasklSfvrJGnwCUnn3MdjViPAGjOIW6j5Pbw6VKSkCJUJpaHwACBY8CsiaxGgekGCduf5YdWntvK7X_loUIRKcTf7L5I4Sw2Kt9wv4HNFssazOceYgyuF_DAA9hZPfWbPoO6qFrIXf5euug2gjv0iwhA1BQUEy2wxhZcgY9OBuM-EZYG-1MEhqoMrf2V0QaR7HJzEceQYBFk_iUeFV0Lovf97iQ7w8MH1moRCfBDFKsAXojeAGM7-wZ8t9ctVJ0xB-Bik8BzfCTXgrdCRfdIGX1f77L1xGuepYwnd8PuRNgnAAiz9bvE-5uwiSarqaICj5rcuiBTm2E4QoOlTQGTvyETd3GMZjYq17_7b54hv5fR-cCDk01Odna4cTF3fxaSQNleIsHz0eeInKsYLNf0ovIqYGvNzADwwcCqE_3APeeJVttdDaL7jnOuxgbkIwk2P01D7CIbdlDz2cKhE9V9cyc0FhVJtY2M1DP_zcMaLxES0VPn3Ud_otbumOGOFpkqzUK6ZY8D7WUTah-f5EQVz2Os2e6J_NDjTluYYV-Uib9q0ubOnPIpgy8EIURNbCCAr1uR4fnNapKtC0vTkL6BQ74XR2LfX76da6Gw265V7fyA5u37oOqFZfaLhrhwe-njVBEEEbGoeDa452UhEIUal59vMpWeb7FEojlDz2RZiCNQmm1MRx9Q5i3NuWfiJuSdZHkYleW-hxnz39Dw30E0h9VxdHWyVDtKnxEsUaD9u4iTXv15RwBDheNOGsyp_ViZqpfbhCcM0pdLLL_-a16IMOK6lpjSyYkN7EcdFFZrtZLtzzzF8vX6xhvOcxvcHzhKTpgXuIGO_eUrw5lsZG2x0Fv6gKjTweH4s5SInkM4RBoQPA53DpwhkSgQp8eMR80p2hPgEXMN1RAsbgHKhis1Chlq27X4F9zGppVBM7Q3p4q8vrTbg1HkP9vvM0LIgioGsy-ntOEahP51RZ2SdZIk9_F6AoxpD8jf3VrEBCNN61O2u-9GVtZX83T7Ws4mqg4KVuEiIl1R3AGksnEAFRI2kTEXIYa8QmV8t0E4ak2-1J_XvizzR0a4iydPU7DuaojYEe7SCxuSdPDbtbjjGTco0Zg7T891tiYMb6w5KFXCbPd0qK9NQfHElsIFZzKr7JUKBAx9aryQRGXn3jqUh1lcRG8OIJyKMdAEP7lNXGadYBf8yapNpfxSDiXc4nTBmlixB_MhVWJCUr5nb9qqxS4Wt0CG1XbDIHU9CQykHjIGa5oxKixkO9eXoUHG8zq_IwwPMn-yhGZseMqIBSDhJGgkyDXCRDwH2pe-fWJhAXCLtOlfIcgMdeLcShDtnVT5xRNhzr4kT-2yEK5KYaHN9gCp8gG84F9cUSlWd_dkJE5doqkz-soUJsztNKGRjKaQ6aI_cjj_RW8XAPnVsddMz7ri8YF29ij_n23DxJlU5eVd4mFeF2msVNGrGZEO3C7gfiLlhUAVP325EM_ICrtYMmrIxXoE3HMZPYvfZKsNNcF-CZASSceTtl5PYGVY2L8fx5NWt-5kgz1yXdl-hyGvinq4p2q9gT9i-4S7yQ-j5sXNuUPbE5tgVu6Fx8kka8R0zm7ME8CzHI7XrYKeELv4Mi_GPozDCrv5NZL6_Rn-dTCYGf3TG7R5rOh3iZZC3f3sEVohSTFdJvYAI8y77IXHglqVwSiIUN30LUNslHQxH1DefI9p5PSgu5GM-tnWVo27qOi0uv5GXNfCKFI7WXsVN3p7zn84HGzvTB6IG2jUaLsg8FDWQqH8iG4sLfX0N7vlQuXoLzr4shF7PrOdNWMtOaex_BXLPf86dKsTQRrnLsofIvyqXEJGFIABmVnDrUsamIlZXIqPCNSXmigncAAnchY8sFGSgDlP9P5jaBsQs8vSbZb_3oauzTJzZREZQAlC3ddaWK62mkCruLJQ6ojwEWIwZKWCTspyI1iNLj6pPpf0TWN4QTHKhpmhCFkPieng7n6MkUSvJfBOUch-koUo-JJofZhrRvnhNUuOilImUDpwygwc5e6DRKo6FAL0FOE43X4rj0EAliVHgwymx_EnoXlJcTFmNhHMDHceIWCT0ZDEpgfpSFbak17JA1mg-1O_PFte5ZOTwGIQbAPDWqmwehNNPGZCSIUmmd5hVRJ8mCYNWOz-gEmvM9j7lzYpot7zoId-uFbXePlmhdVl1qApTSqVpxPv8n6KUSz8ZCgEeSOgqOnssZFlQhA1tlzxGeMU_knb4p2KIMWLMq-dza3402UGgKrwQNIbiBgWTykdWmJ85fihbgSamQhCBFafGiZz40LaOzZSAJAG4blEjrzKmrZmlTErSf7BP2ZTtOiVaELtF3psyyLvtgkSfph2JZdIThuLTmm83OmRrLEiWOozzSXC5EB7avnuC0nmXT8je704mTDXpL3Bs5TtpZHnSU7Dh2TwT2Z04urChqCqY5nasGVRm5ANQkM5bV6bX2bw7PZBxmEQ5OtMx1DRKRG7VxS6yMMqdPbc6LXQNionhCw1Vk5hEbrR6aM1hKeAqUJ4WSKbEP_k4_qFu9XME5-bE1xWbnHoY52TLOn7ntOlTefUU97DXaxjrqKd__HwY05gsFmC9GGNE8HWP8rIea0ohzbk444YEZxa9bOyFiLuQxHDY9GyMKAFQNHO9ZqB6qxekSz0O_aRYc8OOkRlepm45hBHsPBPpxyxPxEAsKrULE9nV8cfpGYWImhjfYWeIWzocAr3lMAb54wHveaMLjPFrEXr-t5fweZWqqFsZK1stiYtU1nvh6uIrY1xDfH97wpxjOUyIs3bF96KZdq1gSlvm7tiw37u8vKpX5az-0RCokzHVSMXzr4N5n1vaH4kvDhR3XcsMkdtRcohKz3ReqDCsOsEeIeXI9WYTngZchFr7dWYsRZzh_08hPaApsuGZmvx7bvUZSv3-gFaoxHlNCmSwavXavr5isiGt1FcVZdUqLVeJjLB-4aaVHqpNtb13ZBhZRVGBpGcik6mfUPgd-vevET0yEjash7O2sbz-Vk7btAPYEZnAYSxkoBNkZyuEyZf_EJqyG1HKvaSRA9ww3_KmUYr_gtC442XHjXh5WuU7lAYkmO7EtjAWtQrQFG8WlYja-L7QWA1FTu2t9M1_olNJ2OX_l5xuDikh49xr44UFOiykhSH4ILunvoR2M6umxzUOcLAQZS98IYPSWIY6hcYSEWubLf1a70-MnVhp5fdo0szGSje9JzSKXEBUrz7cHsw40m_tJZ3Tqz9HM-V5JAgteCDCvIZf7qDbhA_ghOFH10G6w-k8gmMjKrkzMjaZpk8yiNutgx5OkWPl3AA-EKxNaObSp9Il0-LYZxrS3-gss1sGXUl3VHNRxzO5x1Bj4aHkYJ3Dflt5pIcEAHVlfATHhdeanJdGvYnkMTiBDfN-OU0vMptI0VKW-TCTmuxMogsKOlb_d4dVMNc7kipxM5Nz7vbs91zozGj3E-riOUcODePeoFUdqRk6Y0GZzBn88mSTu-QK6tIE3JL3zJ9MmMXFGT7mNkz-wPw3qICjGoPR6zXMfTFhYvQrX7RM84-w48xBE8GDfn1J3Opdw-zqXuDQqIyGvNacelvqY3o8xFwR4e5EbMPkswyDprbRB9ShQGxN6fSQ83gsbs5gmmrMxmJ00JT96JFF3FeQylphMUV8yyWNwn0_bfbp_LoQuxmEEtS_TW9IXzP65jzes64zGm2q5agMrBgwn5ZpvzsZ5wkrb6IFAHnrExJ8InpxFt7o3OOyYi6HHIOKpN1mT3RWgcdTbKvFhaVgWM4pyEFtWeubXsI5euw5WX3tAXZkkeiV9EeKZB5tZUXz4IXq7nvezV0iRb_ZDLtY-gJ-RR6S7eJEwkE86v_FsggtoApt7DvlCZHRSkGSsiGutbVEXJdCpNyxNnojEl5UjhDa_xFoc9355G9U52pCQ2FXQdYfwQ";

        let mut subscription = sub("device-1", "https://fcm.googleapis.com/send/x");
        subscription.keys.auth = auth_b64.to_owned();
        subscription.keys.p256dh = p256dh_b64.to_owned();
        let salt: [u8; 16] = RAW_URL.decode(salt_b64).unwrap().try_into().unwrap();
        let scalar: [u8; 32] = RAW_URL.decode(eph_scalar_b64).unwrap().try_into().unwrap();
        let ephemeral = SecretKey::from_bytes((&scalar).into()).unwrap();

        let body = encrypt_record_with(&subscription, message, salt, &ephemeral).unwrap();
        assert_eq!(body, RAW_URL.decode(expected_b64).unwrap());
        assert_eq!(body.len(), RECORD_SIZE);
    }

    /// Round-trip through the receiver's side of RFC 8291 — derives
    /// ikm/cek/nonce with the *subscription* private key and decrypts,
    /// proving the ECDH+HKDF wiring end-to-end.
    #[test]
    fn aes128gcm_roundtrip_decrypts() {
        let auth = [7u8; 16];
        let sub_secret = generate_secret();
        let p256dh = sub_secret.public_key().to_sec1_point(false);
        let mut subscription = sub("d", "https://fcm.googleapis.com/send/x");
        subscription.keys.auth = RAW_URL.encode(auth);
        subscription.keys.p256dh = RAW_URL.encode(p256dh.as_bytes());
        let message = b"{\"v\":1,\"category\":\"question\"}";

        let body = encrypt_record(&subscription, message).unwrap();
        assert_eq!(body.len(), RECORD_SIZE);
        // Parse the record header.
        let salt = &body[..16];
        let rs = u32::from_be_bytes(body[16..20].try_into().unwrap()) as usize;
        assert_eq!(rs, RECORD_SIZE);
        let idlen = body[20] as usize;
        assert_eq!(idlen, P256_UNCOMPRESSED_LEN);
        let eph_pub = &body[21..21 + idlen];
        let ciphertext = &body[21 + idlen..];
        // Receiver-side derivation.
        let eph_public = PublicKey::from_sec1_bytes(eph_pub).unwrap();
        let shared = sub_secret.diffie_hellman(&eph_public);
        let mut info = b"WebPush: info\x00".to_vec();
        info.extend_from_slice(p256dh.as_bytes());
        info.extend_from_slice(eph_pub);
        let mut ikm = [0u8; 32];
        Hkdf::<Sha256>::new(Some(&auth), shared.raw_secret_bytes().as_slice())
            .expand(&info, &mut ikm)
            .unwrap();
        let mut cek = [0u8; 16];
        Hkdf::<Sha256>::new(Some(salt), &ikm)
            .expand(b"Content-Encoding: aes128gcm\x00", &mut cek)
            .unwrap();
        let mut nonce = [0u8; 12];
        Hkdf::<Sha256>::new(Some(salt), &ikm)
            .expand(b"Content-Encoding: nonce\x00", &mut nonce)
            .unwrap();
        let plain = Aes128Gcm::new_from_slice(&cek)
            .unwrap()
            .decrypt((&nonce).into(), ciphertext)
            .unwrap();
        // `message | 0x02 | zeros`.
        assert_eq!(&plain[..message.len()], message);
        assert_eq!(plain[message.len()], 0x02);
        assert!(plain[message.len() + 1..].iter().all(|b| *b == 0));
    }

    #[test]
    fn aes128gcm_rejects_oversized_and_bad_keys() {
        let mut subscription = sub("d", "https://fcm.googleapis.com/send/x");
        let big = vec![0u8; RECORD_PLAINTEXT_LEN];
        assert!(matches!(
            encrypt_record(&subscription, &big),
            Err(SendError::Local(_))
        ));
        // Not a curve point → local refusal (dropped, not retried).
        subscription.keys.p256dh = RAW_URL.encode([0xffu8; 65]);
        assert!(matches!(
            encrypt_record(&subscription, b"x"),
            Err(SendError::Local(_))
        ));
    }

    // -- VAPID JWT ----------------------------------------------------------

    #[test]
    fn vapid_jwt_claims_sign_and_header_format() {
        let pair = VapidKeypair::ephemeral();
        let endpoint = "https://fcm.googleapis.com/send/abc";
        let now = Timestamp::now();
        let authorization = vapid_authorization(endpoint, &pair, now).unwrap();
        // `vapid t=<jwt>, k=<b64url-pub>` — oracle/webpush-go casing.
        let Some(rest) = authorization.strip_prefix("vapid t=") else {
            panic!("authorization = {authorization}");
        };
        let (jwt, key) = rest.split_once(", k=").unwrap();
        assert_eq!(key, pair.public_b64());
        let segments: Vec<&str> = jwt.split('.').collect();
        assert_eq!(segments.len(), 3);
        // Header is exactly jwt.NewWithClaims(ES256)'s marshal.
        let header: serde_json::Value =
            serde_json::from_slice(&RAW_URL.decode(segments[0]).unwrap()).unwrap();
        assert_eq!(header, serde_json::json!({"alg": "ES256", "typ": "JWT"}));
        let claims: serde_json::Value =
            serde_json::from_slice(&RAW_URL.decode(segments[1]).unwrap()).unwrap();
        assert_eq!(claims["aud"], "https://fcm.googleapis.com");
        assert_eq!(claims["sub"], VAPID_SUBJECT);
        assert_eq!(
            claims["exp"].as_i64().unwrap(),
            now.unix_seconds() + VAPID_EXPIRY.as_secs() as i64
        );
        // ES256 signature verifies against the VAPID public key.
        let signature = Signature::from_slice(&RAW_URL.decode(segments[2]).unwrap()).unwrap();
        let verifier = VerifyingKey::from(&SigningKey::from(&pair.secret));
        verifier
            .verify(
                format!("{}.{}", segments[0], segments[1]).as_bytes(),
                &signature,
            )
            .expect("ES256 signature verifies");
    }

    // -- drain pass (stub sender, no network) --------------------------------

    #[tokio::test]
    async fn drain_accepts_and_marks_delivered() {
        let push = Push::default();
        push.subscribe(sub("device-1", "https://fcm.googleapis.com/one"), &[])
            .unwrap();
        let now = Timestamp::now();
        let key = question_key("device-1", "evt-1");
        let due = publish_due(&push, key.clone(), now);
        let sender = Arc::new(StubSender::default());
        let results = drain_once(&push, &sender, due, &cancel()).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].disposition, Disposition::Accepted);
        assert_eq!(results[0].attempts, 1);
        assert_eq!(sender.calls(), vec!["https://fcm.googleapis.com/one"]);
        // Entry finished → queue empty; key stays active with a
        // delivered record (retraction ledger) for a real category.
        assert!(push.due_entries(due.add_ns(60 * 1_000_000_000)).is_empty());
        assert!(!push.has_entries_for(&key));
    }

    #[tokio::test]
    async fn drain_retries_retryable_then_accepts() {
        let push = Push::default();
        push.subscribe(sub("device-1", "https://fcm.googleapis.com/one"), &[])
            .unwrap();
        let now = Timestamp::now();
        let key = question_key("device-1", "evt-1");
        let due = publish_due(&push, key.clone(), now);
        let sender = Arc::new(StubSender::with_outcomes(vec![
            Err(SendError::Status(503)),
            Ok(()),
        ]));
        let results = drain_once(&push, &sender, due, &cancel()).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].disposition, Disposition::Retrying);
        assert_eq!(results[0].attempts, 1);
        // backoff = 1s<<(1-1) = 1s
        let next = results[0].next_attempt.unwrap();
        assert_eq!(next, due.add_ns(1_000_000_000));
        // Not due yet at the same instant…
        let second = drain_once(&push, &sender, due, &cancel()).await.unwrap();
        assert!(second.is_empty());
        assert_eq!(sender.calls().len(), 1);
        // …due at next_attempt.
        let third = drain_once(&push, &sender, next, &cancel()).await.unwrap();
        assert_eq!(third.len(), 1);
        assert_eq!(third[0].disposition, Disposition::Accepted);
        assert_eq!(third[0].attempts, 2);
        assert!(push.due_entries(next).is_empty());
    }

    #[tokio::test]
    async fn drain_terminal_prunes_subscription() {
        let push = Push::default();
        push.subscribe(sub("device-1", "https://fcm.googleapis.com/gone"), &[])
            .unwrap();
        let now = Timestamp::now();
        let due = publish_due(&push, question_key("device-1", "evt-1"), now);
        let sender = Arc::new(StubSender::with_outcomes(vec![Err(SendError::Status(404))]));
        let results = drain_once(&push, &sender, due, &cancel()).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].disposition, Disposition::Pruned);
        // 404 → subscription dropped from the registry.
        assert!(push.subscriptions().is_empty());
        assert!(push.recovered_keys().is_empty());
    }

    #[tokio::test]
    async fn drain_terminal_endpoint_sends_once_per_pass() {
        let push = Push::default();
        push.subscribe(sub("device-1", "https://fcm.googleapis.com/dead"), &[])
            .unwrap();
        let now = Timestamp::now();
        let due = publish_due(&push, question_key("device-1", "evt-1"), now);
        let due = publish_due(&push, question_key("device-1", "evt-2"), now).max(due);
        let sender = Arc::new(StubSender::with_outcomes(vec![Err(SendError::Status(410))]));
        let results = drain_once(&push, &sender, due, &cancel()).await.unwrap();
        // Two entries, one send — the second prunes without dialing
        // (`terminalSubscriptions`).
        assert_eq!(sender.calls().len(), 1);
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|r| r.disposition == Disposition::Pruned));
        assert!(push.subscriptions().is_empty());
        assert!(push.due_entries(due).is_empty());
    }

    #[tokio::test]
    async fn drain_pruned_falls_back_to_retained_endpoint() {
        let push = Push::default();
        push.subscribe(sub("device-1", "https://fcm.googleapis.com/old"), &[])
            .unwrap();
        push.subscribe(sub("device-1", "https://fcm.googleapis.com/new"), &[])
            .unwrap();
        let now = Timestamp::now();
        let due = publish_due(&push, question_key("device-1", "evt-1"), now);
        // The publish picks the latest subscription ("new").
        let sender = Arc::new(StubSender::with_outcomes(vec![
            Err(SendError::Status(410)),
            Ok(()),
        ]));
        let results = drain_once(&push, &sender, due, &cancel()).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].disposition, Disposition::Pruned);
        // Recovery: "new" removed, the event requeued onto "old".
        let subs = push.subscriptions();
        assert_eq!(subs.len(), 1);
        assert_eq!(subs[0].endpoint, "https://fcm.googleapis.com/old");
        let due2 = push.due_entries(due);
        assert_eq!(due2.len(), 1);
        assert_eq!(
            due2[0].subscription.endpoint,
            "https://fcm.googleapis.com/old"
        );
        // Next pass delivers to the fallback.
        let results = drain_once(&push, &sender, due, &cancel()).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].disposition, Disposition::Accepted);
        assert_eq!(
            sender.calls(),
            vec![
                "https://fcm.googleapis.com/new",
                "https://fcm.googleapis.com/old"
            ]
        );
    }

    #[tokio::test]
    async fn drain_expired_and_stale_entries_skip_the_wire() {
        let push = Push::default();
        push.subscribe(sub("device-1", "https://fcm.googleapis.com/one"), &[])
            .unwrap();
        let now = Timestamp::now();
        // Expired: 1s TTL, drained after.
        publish(&push, question_key("device-1", "evt-exp"), now);
        let sender = Arc::new(StubSender::default());
        let results = drain_once(&push, &sender, now.add_ns(61 * 1_000_000_000), &cancel())
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].disposition, Disposition::Expired);
        // Stale: published then the viewed-pane marker suppresses it.
        let key = question_key("device-1", "evt-stale");
        let due = publish_due(&push, key.clone(), now);
        push.set_viewed_pane(
            "device-1",
            Some(lerdr_core::protocol::TargetRef {
                server_session_id: key.server_session_id.clone(),
                pane_id: key.pane_id.clone(),
                terminal_id: key.terminal_id.clone(),
                generation: key.generation,
                agent_session_id: key.agent_session_id.clone(),
            }),
        );
        let results = drain_once(&push, &sender, due, &cancel()).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].disposition, Disposition::Stale);
        assert!(sender.calls().is_empty());
    }

    #[tokio::test]
    async fn drain_drops_after_max_attempts_and_on_nonretryable() {
        let push = Push::default();
        push.subscribe(sub("device-1", "https://fcm.googleapis.com/one"), &[])
            .unwrap();
        let now = Timestamp::now();
        let due = publish_due(&push, question_key("device-1", "evt-1"), now);
        // 400 is not terminal, not retryable → dropped.
        let sender = Arc::new(StubSender::with_outcomes(vec![Err(SendError::Status(400))]));
        let results = drain_once(&push, &sender, due, &cancel()).await.unwrap();
        assert_eq!(results[0].disposition, Disposition::Dropped);
        assert!(push.due_entries(due).is_empty());
        // Attempts cap: entry already at max-1 → last failure drops.
        let due = publish_due(&push, question_key("device-1", "evt-2"), now);
        let mut entry = push.due_entries(due).remove(0);
        // Simulate five prior failures.
        for attempt in 1..MAX_DELIVERY_ATTEMPTS {
            let next = due.add_ns(retry_delay_ns(attempt));
            assert!(push.reschedule_entry(&entry, attempt, next));
            entry.attempts = attempt;
            entry.due_at = next;
        }
        let sender = Arc::new(StubSender::with_outcomes(vec![Err(SendError::Status(500))]));
        // The reschedules pushed `due_at` forward — drain at it.
        let results = drain_once(&push, &sender, entry.due_at, &cancel())
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].disposition, Disposition::Dropped);
        assert_eq!(results[0].attempts, MAX_DELIVERY_ATTEMPTS);
    }

    /// A sender whose in-flight POST never resolves — cancellation
    /// must abort it and leave the entry queued.
    struct PendingSender;

    impl PushSender for PendingSender {
        #[allow(clippy::manual_async_fn)]
        fn send<'a>(
            &'a self,
            _subscription: &'a Subscription,
            _payload: &'a [u8],
        ) -> impl std::future::Future<Output = Result<(), SendError>> + Send + 'a {
            std::future::pending()
        }
    }

    #[tokio::test]
    async fn drain_cancellation_leaves_entries_queued() {
        let push = Push::default();
        push.subscribe(sub("device-1", "https://fcm.googleapis.com/one"), &[])
            .unwrap();
        let now = Timestamp::now();
        let key = question_key("device-1", "evt-1");
        let due = publish_due(&push, key.clone(), now);
        let token = cancel();
        token.cancel();
        let sender = Arc::new(PendingSender);
        // The cancelled pass aborts the in-flight send; the entry
        // stays queued for the next pass — nothing is dropped.
        let results = drain_once(&push, &sender, due, &token).await.unwrap();
        assert!(push.has_entries_for(&key));
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn worker_loop_wakes_on_publish() {
        let push = Push::default();
        push.subscribe(sub("device-1", "https://fcm.googleapis.com/one"), &[])
            .unwrap();
        let sender = Arc::new(StubSender::default());
        let token = cancel();
        let handle = spawn_worker(push.clone(), sender.clone(), token.clone());
        let now = Timestamp::now();
        publish(&push, question_key("device-1", "evt-1"), now);
        // The wake notify fires the drain — wait for the send.
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if !sender.calls().is_empty() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("worker delivered within timeout");
        token.cancel();
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn sender_rejects_non_https_endpoints() {
        let sender = WebPushSender::new(VapidKeypair::ephemeral()).unwrap();
        let mut subscription = sub("d", "http://fcm.googleapis.com/send/x");
        subscription.keys.p256dh = RAW_URL.encode(
            generate_secret()
                .public_key()
                .to_sec1_point(false)
                .as_bytes(),
        );
        subscription.keys.auth = RAW_URL.encode([1u8; 16]);
        // The scheme check fires before any dial — no network touched.
        let outcome = sender.send(&subscription, b"{}").await;
        assert!(matches!(outcome, Err(SendError::Local(_))));
    }

    // -- durable queue (`queue.json`) --------------------------------------

    /// Read the on-disk queue as raw JSON for shape assertions.
    fn queue_file(dir: &tempfile::TempDir) -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(dir.path().join("queue.json")).unwrap())
            .unwrap()
    }

    /// `TestRecoveredManagerWaitsForAuthoritativeReconcile` — a
    /// persisted entry survives reopen, stays gated until `reconcile`
    /// supplies the authoritative inventory, then delivers even though
    /// its `due_at` is long past.
    #[tokio::test]
    async fn restart_gates_recovered_entry_until_reconcile() {
        let dir = tempfile::tempdir().unwrap();
        let push = Push::new(dir.path()).unwrap();
        push.subscribe(sub("device-1", "https://fcm.googleapis.com/one"), &[])
            .unwrap();
        let now = Timestamp::now();
        let key = question_key("device-1", "evt-1");
        publish(&push, key.clone(), now); // due at now+2s settle
        assert!(dir.path().join("queue.json").exists());
        drop(push);

        // Reopen — the queue recovered; `reconciled` stays shut.
        let push = Push::new(dir.path()).unwrap();
        assert_eq!(push.recovered_keys(), vec![key.clone()]);
        assert!(!push.is_reconciled());
        let sender = Arc::new(StubSender::default());
        let past_due = now.add_ns(10 * 1_000_000_000); // well past due_at
        let results = drain_once(&push, &sender, past_due, &cancel())
            .await
            .unwrap();
        assert!(results.is_empty(), "unreconciled pass must no-op");
        assert!(sender.calls().is_empty());
        assert!(push.has_entries_for(&key));

        // The authoritative inventory keeps the key → gate opens and
        // the past-due entry delivers on the very next pass.
        push.reconcile(std::slice::from_ref(&key)).unwrap();
        let results = drain_once(&push, &sender, past_due, &cancel())
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].disposition, Disposition::Accepted);
        assert_eq!(sender.calls(), vec!["https://fcm.googleapis.com/one"]);

        // `finish` persisted the move into `delivered` — the
        // retraction ledger lives on disk now.
        let file = queue_file(&dir);
        assert_eq!(file["entries"].as_object().unwrap().len(), 0);
        let delivered = file["delivered"].as_object().unwrap();
        assert_eq!(delivered.len(), 1);
        let record = delivered.values().next().unwrap();
        assert_eq!(record["key"]["event_id"], "evt-1");
        assert_eq!(
            record["subscription"]["endpoint"],
            "https://fcm.googleapis.com/one"
        );
        assert!(record["accepted_at"].is_string());

        // A third open recovers the delivered marker — it re-gates
        // (`activeKeys` includes delivered rows) and `reconcile` with
        // the key still current resolves clean with nothing to send.
        let push = Push::new(dir.path()).unwrap();
        assert!(!push.is_reconciled());
        assert_eq!(push.recovered_keys(), vec![key.clone()]);
        push.reconcile(&[key]).unwrap();
        let results = drain_once(&push, &sender, past_due, &cancel())
            .await
            .unwrap();
        assert!(results.is_empty());
        assert_eq!(sender.calls().len(), 1);
    }

    /// A recovered key absent from the authoritative inventory is
    /// retracted, not delivered — `Reconcile` → `Resolve` → retract
    /// event queued + persisted.
    #[tokio::test]
    async fn restart_retracts_recovered_key_absent_from_inventory() {
        let dir = tempfile::tempdir().unwrap();
        let push = Push::new(dir.path()).unwrap();
        push.subscribe(sub("device-1", "https://fcm.googleapis.com/one"), &[])
            .unwrap();
        let now = Timestamp::now();
        let key = question_key("device-1", "evt-1");
        publish(&push, key.clone(), now);
        drop(push);

        let push = Push::new(dir.path()).unwrap();
        // The pane is gone in the inventory → the pending entry is
        // cancelled; nothing to retract (no delivered record).
        push.reconcile(&[]).unwrap();
        assert!(!push.has_entries_for(&key));
        let file = queue_file(&dir);
        assert_eq!(file["entries"].as_object().unwrap().len(), 0);
        let sender = Arc::new(StubSender::default());
        let results = drain_once(&push, &sender, now.add_ns(10 * 1_000_000_000), &cancel())
            .await
            .unwrap();
        assert!(results.is_empty());
        assert!(sender.calls().is_empty());
    }

    /// `delivered`-marker retraction across restart: accept a
    /// delivery, reopen, reconcile the key out — the retract send
    /// still happens because the delivered record persisted.
    #[tokio::test]
    async fn restart_retracts_delivered_record() {
        let dir = tempfile::tempdir().unwrap();
        let push = Push::new(dir.path()).unwrap();
        push.subscribe(sub("device-1", "https://fcm.googleapis.com/one"), &[])
            .unwrap();
        let now = Timestamp::now();
        let key = question_key("device-1", "evt-1");
        let due = publish_due(&push, key.clone(), now);
        let sender = Arc::new(StubSender::default());
        let results = drain_once(&push, &sender, due, &cancel()).await.unwrap();
        assert_eq!(results[0].disposition, Disposition::Accepted);
        drop(push);

        // Reopen: the delivered row recovers; reconcile drops the key
        // from the inventory → `resolve` queues a retract event.
        let push = Push::new(dir.path()).unwrap();
        push.reconcile(&[]).unwrap();
        let results = drain_once(&push, &sender, Timestamp::now(), &cancel())
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].disposition, Disposition::Accepted);
        assert!(
            results[0].event.retract,
            "the queued event is the retraction"
        );
        assert_eq!(sender.calls().len(), 2);
        // Retraction finished → delivered row swept on disk too.
        let file = queue_file(&dir);
        assert_eq!(file["delivered"].as_object().unwrap().len(), 0);
    }

    /// publish → drop/reopen → deliver in `due_at` order.
    #[tokio::test]
    async fn restart_resumes_due_order() {
        let dir = tempfile::tempdir().unwrap();
        let push = Push::new(dir.path()).unwrap();
        push.subscribe(sub("device-1", "https://fcm.googleapis.com/one"), &[])
            .unwrap();
        let now = Timestamp::now();
        let key_b = question_key("device-1", "evt-b");
        let key_a = question_key("device-1", "evt-a");
        // `evt-b` created first but due later (settle rides on
        // created_at); `evt-a` is due first.
        publish(&push, key_b.clone(), now);
        publish(&push, key_a.clone(), now.add_ns(-500_000_000));
        drop(push);

        let push = Push::new(dir.path()).unwrap();
        push.reconcile(&[key_a.clone(), key_b.clone()]).unwrap();
        let sender = Arc::new(StubSender::default());
        let results = drain_once(&push, &sender, now.add_ns(10 * 1_000_000_000), &cancel())
            .await
            .unwrap();
        assert_eq!(results.len(), 2);
        assert!(results
            .iter()
            .all(|r| r.disposition == Disposition::Accepted));
        // `(due_at, id)` order — `evt-a` first.
        assert_eq!(results[0].key.event_id, "evt-a");
        assert_eq!(results[1].key.event_id, "evt-b");
    }

    /// Backoff state is durable: a retrying entry's `attempts` +
    /// `due_at` persist, so a restarted queue honors the same backoff
    /// window instead of redelivering early.
    #[tokio::test]
    async fn restart_preserves_retry_backoff() {
        let dir = tempfile::tempdir().unwrap();
        let push = Push::new(dir.path()).unwrap();
        push.subscribe(sub("device-1", "https://fcm.googleapis.com/one"), &[])
            .unwrap();
        let now = Timestamp::now();
        let key = question_key("device-1", "evt-1");
        let due = publish_due(&push, key.clone(), now);
        let sender = Arc::new(StubSender::with_outcomes(vec![
            Err(SendError::Status(503)),
            Ok(()),
        ]));
        let results = drain_once(&push, &sender, due, &cancel()).await.unwrap();
        assert_eq!(results[0].disposition, Disposition::Retrying);
        assert_eq!(results[0].attempts, 1);
        let next = results[0].next_attempt.unwrap();
        assert_eq!(next, due.add_ns(1_000_000_000));
        // The reschedule landed on disk through `flush_queue`.
        let file = queue_file(&dir);
        let entry = file["entries"]
            .as_object()
            .unwrap()
            .values()
            .next()
            .unwrap();
        assert_eq!(entry["attempts"], 1);
        drop(push);

        // Reopen — backoff survives: pre-`next` pass is a no-op even
        // though the entry exists.
        let push = Push::new(dir.path()).unwrap();
        push.reconcile(std::slice::from_ref(&key)).unwrap();
        let results = drain_once(&push, &sender, due.add_ns(500_000_000), &cancel())
            .await
            .unwrap();
        assert!(results.is_empty());
        assert_eq!(sender.calls().len(), 1, "no send before next_attempt");
        let results = drain_once(&push, &sender, next, &cancel()).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].disposition, Disposition::Accepted);
        assert_eq!(results[0].attempts, 2);
    }

    /// The 410 prune persists: subscription removal and the fallback
    /// requeue both land on disk, so a restarted queue delivers to the
    /// retained endpoint only.
    #[tokio::test]
    async fn restart_preserves_prune_and_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let push = Push::new(dir.path()).unwrap();
        push.subscribe(sub("device-1", "https://fcm.googleapis.com/old"), &[])
            .unwrap();
        push.subscribe(sub("device-1", "https://fcm.googleapis.com/new"), &[])
            .unwrap();
        let now = Timestamp::now();
        let key = question_key("device-1", "evt-1");
        let due = publish_due(&push, key.clone(), now); // lands on "new"
        let sender = Arc::new(StubSender::with_outcomes(vec![
            Err(SendError::Status(410)),
            Ok(()),
        ]));
        let results = drain_once(&push, &sender, due, &cancel()).await.unwrap();
        assert_eq!(results[0].disposition, Disposition::Pruned);
        // `subscriptions.json` lost "new"; `queue.json` holds the
        // requeued entry on "old".
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(
                &std::fs::read_to_string(dir.path().join("subscriptions.json")).unwrap()
            )
            .unwrap()["subscriptions"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        let entry = queue_file(&dir)["entries"]
            .as_object()
            .unwrap()
            .values()
            .next()
            .unwrap()
            .clone();
        assert_eq!(
            entry["subscription"]["endpoint"],
            "https://fcm.googleapis.com/old"
        );
        drop(push);

        let push = Push::new(dir.path()).unwrap();
        let subs = push.subscriptions();
        assert_eq!(subs.len(), 1);
        assert_eq!(subs[0].endpoint, "https://fcm.googleapis.com/old");
        push.reconcile(&[key]).unwrap();
        let results = drain_once(&push, &sender, due, &cancel()).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].disposition, Disposition::Accepted);
        assert_eq!(
            sender.calls(),
            vec![
                "https://fcm.googleapis.com/new",
                "https://fcm.googleapis.com/old"
            ]
        );
    }

    /// A restart with nothing queued opens reconciled immediately —
    /// `reconciled = len(recovered) == 0`.
    #[tokio::test]
    async fn restart_with_empty_queue_opens_reconciled() {
        let dir = tempfile::tempdir().unwrap();
        let push = Push::new(dir.path()).unwrap();
        assert!(push.is_reconciled());
        push.subscribe(sub("device-1", "https://fcm.googleapis.com/one"), &[])
            .unwrap();
        drop(push);
        let push = Push::new(dir.path()).unwrap();
        assert!(push.is_reconciled(), "subscriptions alone don't gate");
    }

    /// Under paused time the 250ms tick can never fire — a delivery
    /// lands only because `publish` raised `wake`. Mirrors the
    /// oracle's buffered `m.signal()` wakeup.
    #[tokio::test(start_paused = true)]
    async fn worker_wakes_on_publish_under_paused_time() {
        let push = Push::default();
        push.subscribe(sub("device-1", "https://fcm.googleapis.com/one"), &[])
            .unwrap();
        let sender = Arc::new(StubSender::default());
        let token = cancel();
        let handle = spawn_worker(push.clone(), sender.clone(), token.clone());
        // created_at in the past → the entry is already due; the wall
        // clock (Timestamp::now) is what the drain reads, while tokio's
        // paused clock governs the ticker — so only the `wake` notify
        // can trigger this drain.
        publish(
            &push,
            question_key("device-1", "evt-1"),
            Timestamp::now().add_ns(-10 * 1_000_000_000),
        );
        for _ in 0..1000 {
            tokio::task::yield_now().await;
            if !sender.calls().is_empty() {
                break;
            }
        }
        assert_eq!(sender.calls(), vec!["https://fcm.googleapis.com/one"]);
        // A tick after the fact finds an empty queue.
        tokio::time::advance(Duration::from_millis(300)).await;
        tokio::task::yield_now().await;
        assert_eq!(sender.calls().len(), 1);
        token.cancel();
        handle.await.unwrap();
    }
}
