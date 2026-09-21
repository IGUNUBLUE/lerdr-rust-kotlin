//! `herdr-e2ee-v2` handshake: auth binding, transcript proofs, ECDH, HKDF
//! key derivation, and the hello/finish message codecs.
//!
//! Byte layout (oracle: `e2ee.go`):
//!
//! ```text
//! binding       = "herdr-e2ee-v2 auth\x00" ‖ kind ‖ \x00 ‖ auth_id ‖ \x00
//!                 ‖ decimal(auth_version) ‖ \x00
//! client_proof  = HMAC(secret, "herdr-e2ee-v2 client\x00" ‖ binding
//!                 ‖ client_nonce ‖ client_public_bytes)
//! transcript    = binding ‖ client_nonce ‖ client_public ‖ server_nonce
//!                 ‖ server_public
//! server_proof  = HMAC(secret, "herdr-e2ee-v2 server\x00" ‖ transcript)
//! key_salt      = HMAC(secret, "herdr-e2ee-v2 key\x00"    ‖ transcript)
//! shared        = ECDH(server_priv, client_pub)          # raw 32B x-coord
//! c2s_key       = HKDF-SHA256(shared, salt=key_salt, "herdr-e2ee-v2 c2s", 32)
//! s2c_key       = HKDF-SHA256(shared, salt=key_salt, "herdr-e2ee-v2 s2c", 32)
//! ```
//!
//! All base64 is RawURLEncoding (no padding). [`ServerHandshake`] is the
//! synchronous state machine; the tokio driver owns I/O (rust-relay
//! conventions).

use base64::Engine;
use hkdf::Hkdf;
use hmac::{Hmac, KeyInit, Mac};
use p256::elliptic_curve::sec1::ToSec1Point;
use p256::{PublicKey, SecretKey};
use serde::Deserialize;
use sha2::Sha256;

use crate::codec::{Codec, RAW_URL};
use crate::error::E2eeError;
use crate::json;
use crate::session::Session;
use crate::VERSION;

/// Bytes in a handshake nonce.
pub const NONCE_BYTES: usize = 32;
/// Bytes in an uncompressed P-256 point (`0x04 ‖ X ‖ Y`).
pub const PUBLIC_KEY_BYTES: usize = 65;
/// Bytes in the pairing credential / invitation secret.
pub const SECRET_BYTES: usize = 32;
/// Bytes in a handshake proof (HMAC-SHA256 output).
pub const PROOF_BYTES: usize = 32;
/// `auth_id` ceiling — measured in **bytes**, matching Go `len()`.
pub const MAX_AUTH_ID_BYTES: usize = 128;
/// `locale` ceiling — bytes, not chars.
pub const MAX_LOCALE_BYTES: usize = 32;

const CLIENT_PROOF_LABEL: &[u8] = b"herdr-e2ee-v2 client\x00";
const SERVER_PROOF_LABEL: &[u8] = b"herdr-e2ee-v2 server\x00";
const KEY_SALT_LABEL: &[u8] = b"herdr-e2ee-v2 key\x00";
const BINDING_PREFIX: &[u8] = b"herdr-e2ee-v2 auth\x00";
const HKDF_INFO_C2S: &[u8] = b"herdr-e2ee-v2 c2s";
const HKDF_INFO_S2C: &[u8] = b"herdr-e2ee-v2 s2c";

type HmacSha256 = Hmac<Sha256>;

/// `E2EEAuthKind` — the two credential types admitted by the selector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthKind {
    Credential,
    Invitation,
}

impl AuthKind {
    pub fn as_str(self) -> &'static str {
        match self {
            AuthKind::Credential => "credential",
            AuthKind::Invitation => "invitation",
        }
    }

    /// Parse the wire label (`"credential"` | `"invitation"`).
    pub fn from_label(label: &str) -> Option<Self> {
        match label {
            "credential" => Some(AuthKind::Credential),
            "invitation" => Some(AuthKind::Invitation),
            _ => None,
        }
    }
}

impl std::fmt::Display for AuthKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// `E2EEAuthSelector` — the credential/invitation the client authenticates
/// under. `locale` rides the hello but is **not** part of the HMAC binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthSelector {
    pub kind: AuthKind,
    pub id: String,
    pub version: u64,
    pub locale: String,
}

impl AuthSelector {
    pub fn new(
        kind: AuthKind,
        id: impl Into<String>,
        version: u64,
        locale: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            id: id.into(),
            version,
            locale: locale.into(),
        }
    }

    /// `e2eeAuthBinding`: `\x00`-joined, version rendered as decimal.
    pub fn binding(&self) -> Vec<u8> {
        let version = self.version.to_string();
        let mut binding = Vec::with_capacity(
            BINDING_PREFIX.len() + self.kind.as_str().len() + self.id.len() + version.len() + 3,
        );
        binding.extend_from_slice(BINDING_PREFIX);
        binding.extend_from_slice(self.kind.as_str().as_bytes());
        binding.push(0);
        binding.extend_from_slice(self.id.as_bytes());
        binding.push(0);
        binding.extend_from_slice(version.as_bytes());
        binding.push(0);
        binding
    }

    /// Selector validity per `parseE2EEClientHello`: non-empty id ≤128 bytes,
    /// version ≠ 0, locale ≤32 bytes.
    pub fn is_valid(&self) -> bool {
        !self.id.is_empty()
            && self.id.len() <= MAX_AUTH_ID_BYTES
            && self.version != 0
            && self.locale.len() <= MAX_LOCALE_BYTES
    }
}

/// Per-direction session keys. Not `Copy` — clones are deliberate, and the
/// bytes are scrubbed on drop (`ZeroizeOnDrop`). The `aes` crate's own
/// `zeroize` feature additionally scrubs the `Aes256Gcm` round keys inside
/// `Session` when it drops.
#[derive(Debug, Clone, PartialEq, Eq, zeroize::ZeroizeOnDrop)]
pub struct SessionKeys {
    /// Client → server AEAD key.
    pub c2s: [u8; 32],
    /// Server → client AEAD key.
    pub s2c: [u8; 32],
}

/// `e2eeTranscript` — binding ‖ nonces ‖ public keys, in order.
pub fn transcript(
    binding: &[u8],
    client_nonce: &[u8],
    client_public: &[u8],
    server_nonce: &[u8],
    server_public: &[u8],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(binding.len() + 2 * NONCE_BYTES + 2 * PUBLIC_KEY_BYTES);
    out.extend_from_slice(binding);
    out.extend_from_slice(client_nonce);
    out.extend_from_slice(client_public);
    out.extend_from_slice(server_nonce);
    out.extend_from_slice(server_public);
    out
}

fn auth_tag(secret: &[u8], parts: &[&[u8]]) -> [u8; PROOF_BYTES] {
    let mut mac = HmacSha256::new_from_slice(secret).expect("HMAC accepts any key length");
    for part in parts {
        mac.update(part);
    }
    mac.finalize().into_bytes().into()
}

/// `client_proof` per the spec — used by clients to prove knowledge of the
/// pairing secret.
pub fn client_proof(
    secret: &[u8],
    binding: &[u8],
    client_nonce: &[u8],
    client_public: &[u8],
) -> [u8; PROOF_BYTES] {
    auth_tag(
        secret,
        &[CLIENT_PROOF_LABEL, binding, client_nonce, client_public],
    )
}

/// `server_proof` per the spec.
pub fn server_proof(secret: &[u8], transcript: &[u8]) -> [u8; PROOF_BYTES] {
    auth_tag(secret, &[SERVER_PROOF_LABEL, transcript])
}

/// `key_salt` per the spec — HKDF salt for the session keys.
pub fn key_salt(secret: &[u8], transcript: &[u8]) -> [u8; PROOF_BYTES] {
    auth_tag(secret, &[KEY_SALT_LABEL, transcript])
}

/// Raw P-256 ECDH shared secret (the 32-byte x-coordinate), matching
/// Go `ecdh.P256().ECDH`.
pub fn ecdh_shared(private: &SecretKey, peer_public: &PublicKey) -> [u8; 32] {
    let shared = p256::ecdh::diffie_hellman(private.to_nonzero_scalar(), *peer_public.as_affine());
    shared.raw_secret_bytes().as_slice().try_into().unwrap()
}

/// `hkdf.Key(sha256, shared, key_salt, info, 32)` per direction.
pub fn derive_session_keys(shared_secret: &[u8; 32], key_salt: &[u8; 32]) -> SessionKeys {
    let hk = Hkdf::<Sha256>::new(Some(key_salt), shared_secret);
    let mut c2s = [0u8; 32];
    let mut s2c = [0u8; 32];
    hk.expand(HKDF_INFO_C2S, &mut c2s)
        .expect("32 bytes is a valid HKDF output length");
    hk.expand(HKDF_INFO_S2C, &mut s2c)
        .expect("32 bytes is a valid HKDF output length");
    SessionKeys { c2s, s2c }
}

/// A parsed and validated `e2ee_client_hello` (`parsedE2EEClientHello`).
#[derive(Debug)]
pub struct ClientHello {
    pub selector: AuthSelector,
    pub nonce: [u8; NONCE_BYTES],
    /// The raw 65-byte uncompressed point, as it appeared on the wire (the
    /// transcript binds these bytes, not a re-encoding).
    pub public_bytes: [u8; PUBLIC_KEY_BYTES],
    pub public_key: PublicKey,
    pub proof: [u8; PROOF_BYTES],
}

/// A parsed and validated `e2ee_server_hello`.
#[derive(Debug)]
pub struct ServerHello {
    pub nonce: [u8; NONCE_BYTES],
    pub public_bytes: [u8; PUBLIC_KEY_BYTES],
    pub public_key: PublicKey,
    pub proof: [u8; PROOF_BYTES],
}

/// `e2eeServerFinish` payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerFinish {
    pub device_id: String,
    pub credential_id: String,
    pub role: String,
    pub locale: String,
    pub credential_version: u64,
    /// Present only when an invitation handshake issued a fresh credential.
    pub credential_secret: Option<String>,
}

#[derive(Deserialize)]
struct ClientHelloWire {
    #[serde(default, deserialize_with = "json::null_default")]
    r#type: String,
    #[serde(default, deserialize_with = "json::null_default")]
    version: i64,
    #[serde(default, deserialize_with = "json::null_default")]
    auth_kind: String,
    #[serde(default, deserialize_with = "json::null_default")]
    auth_id: String,
    #[serde(default, deserialize_with = "json::null_default")]
    auth_version: u64,
    #[serde(default, deserialize_with = "json::null_default")]
    locale: String,
    #[serde(default, deserialize_with = "json::null_default")]
    nonce: String,
    #[serde(default, deserialize_with = "json::null_default")]
    public_key: String,
    #[serde(default, deserialize_with = "json::null_default")]
    proof: String,
}

#[derive(Deserialize)]
struct ServerHelloWire {
    #[serde(default, deserialize_with = "json::null_default")]
    r#type: String,
    #[serde(default, deserialize_with = "json::null_default")]
    version: i64,
    #[serde(default, deserialize_with = "json::null_default")]
    nonce: String,
    #[serde(default, deserialize_with = "json::null_default")]
    public_key: String,
    #[serde(default, deserialize_with = "json::null_default")]
    proof: String,
}

#[derive(Deserialize)]
struct FinishWire {
    #[serde(default, deserialize_with = "json::null_default")]
    r#type: String,
    #[serde(default, deserialize_with = "json::null_default")]
    version: i64,
}

#[derive(Deserialize)]
struct ServerFinishWire {
    #[serde(default, deserialize_with = "json::null_default")]
    r#type: String,
    #[serde(default, deserialize_with = "json::null_default")]
    version: i64,
    #[serde(default, deserialize_with = "json::null_default")]
    device_id: String,
    #[serde(default, deserialize_with = "json::null_default")]
    credential_id: String,
    #[serde(default, deserialize_with = "json::null_default")]
    role: String,
    #[serde(default, deserialize_with = "json::null_default")]
    locale: String,
    #[serde(default, deserialize_with = "json::null_default")]
    credential_version: u64,
    #[serde(default)]
    credential_secret: Option<String>,
}

fn decode_field<const N: usize>(value: &str) -> Option<[u8; N]> {
    RAW_URL.decode(value).ok()?.try_into().ok()
}

/// Parse an SEC1 point the way Go `ecdh.P256().NewPublicKey` does: exactly 65
/// bytes starting with `0x04` (compressed and hybrid forms rejected).
fn parse_uncompressed_p256(bytes: &[u8; PUBLIC_KEY_BYTES]) -> Option<PublicKey> {
    if bytes[0] != 0x04 {
        return None;
    }
    PublicKey::from_sec1_bytes(bytes).ok()
}

/// `parseE2EEClientHello`: structural validation, selector rules, then the
/// P-256 point check. Error precedence matches Go exactly.
pub fn parse_client_hello(raw_hello: &[u8]) -> Result<ClientHello, E2eeError> {
    let hello: ClientHelloWire =
        serde_json::from_slice(raw_hello).map_err(|_| E2eeError::InvalidClientHello)?;
    if hello.r#type != "e2ee_client_hello" || hello.version != i64::from(VERSION) {
        return Err(E2eeError::UnsupportedClientHello);
    }
    let kind = AuthKind::from_label(&hello.auth_kind);
    let selector = kind.map(|kind| AuthSelector {
        kind,
        id: hello.auth_id,
        version: hello.auth_version,
        locale: hello.locale,
    });
    let selector = match selector {
        Some(selector) if selector.is_valid() => selector,
        _ => return Err(E2eeError::InvalidAuthSelector),
    };
    let nonce = decode_field(&hello.nonce).ok_or(E2eeError::InvalidClientNonce)?;
    let public_bytes = decode_field(&hello.public_key).ok_or(E2eeError::InvalidClientPublicKey)?;
    let proof = decode_field(&hello.proof).ok_or(E2eeError::InvalidClientProof)?;
    let public_key =
        parse_uncompressed_p256(&public_bytes).ok_or(E2eeError::InvalidClientPublicKey)?;
    Ok(ClientHello {
        selector,
        nonce,
        public_bytes,
        public_key,
        proof,
    })
}

/// Mirror of the client-hello parse for the server hello (client side; the Go
/// relay never parses these, so error names are this crate's own).
pub fn parse_server_hello(raw_hello: &[u8]) -> Result<ServerHello, E2eeError> {
    let hello: ServerHelloWire =
        serde_json::from_slice(raw_hello).map_err(|_| E2eeError::InvalidServerHello)?;
    if hello.r#type != "e2ee_server_hello" || hello.version != i64::from(VERSION) {
        return Err(E2eeError::UnsupportedServerHello);
    }
    let nonce = decode_field(&hello.nonce).ok_or(E2eeError::InvalidServerNonce)?;
    let public_bytes = decode_field(&hello.public_key).ok_or(E2eeError::InvalidServerPublicKey)?;
    let proof = decode_field(&hello.proof).ok_or(E2eeError::InvalidServerProof)?;
    let public_key =
        parse_uncompressed_p256(&public_bytes).ok_or(E2eeError::InvalidServerPublicKey)?;
    Ok(ServerHello {
        nonce,
        public_bytes,
        public_key,
        proof,
    })
}

/// `hmac.Equal(clientHello.proof, wantClientProof)` — constant-time.
pub fn verify_client_proof(secret: &[u8], hello: &ClientHello) -> bool {
    let binding = hello.selector.binding();
    let mut mac = HmacSha256::new_from_slice(secret).expect("HMAC accepts any key length");
    mac.update(CLIENT_PROOF_LABEL);
    mac.update(&binding);
    mac.update(&hello.nonce);
    mac.update(&hello.public_bytes);
    mac.verify_slice(&hello.proof).is_ok()
}

/// Serialize `e2ee_client_hello` byte-identically to Go `json.Marshal`
/// (field order `type, version, auth_kind, auth_id, auth_version, locale,
/// nonce, public_key, proof`).
pub fn encode_client_hello(
    selector: &AuthSelector,
    nonce: &[u8; NONCE_BYTES],
    public_bytes: &[u8; PUBLIC_KEY_BYTES],
    proof: &[u8; PROOF_BYTES],
) -> Vec<u8> {
    let mut out = String::with_capacity(384);
    out.push_str("{\"type\":\"e2ee_client_hello\",\"version\":2,\"auth_kind\":");
    json::escape_string(&mut out, selector.kind.as_str());
    out.push_str(",\"auth_id\":");
    json::escape_string(&mut out, &selector.id);
    out.push_str(",\"auth_version\":");
    out.push_str(&selector.version.to_string());
    out.push_str(",\"locale\":");
    json::escape_string(&mut out, &selector.locale);
    out.push_str(",\"nonce\":");
    json::escape_string(&mut out, &RAW_URL.encode(nonce));
    out.push_str(",\"public_key\":");
    json::escape_string(&mut out, &RAW_URL.encode(public_bytes));
    out.push_str(",\"proof\":");
    json::escape_string(&mut out, &RAW_URL.encode(proof));
    out.push('}');
    out.into_bytes()
}

/// Serialize `e2ee_server_hello` byte-identically to Go `json.Marshal`.
pub fn encode_server_hello(
    nonce: &[u8; NONCE_BYTES],
    public_bytes: &[u8; PUBLIC_KEY_BYTES],
    proof: &[u8; PROOF_BYTES],
) -> Vec<u8> {
    let mut out = String::with_capacity(256);
    out.push_str("{\"type\":\"e2ee_server_hello\",\"version\":2,\"nonce\":");
    json::escape_string(&mut out, &RAW_URL.encode(nonce));
    out.push_str(",\"public_key\":");
    json::escape_string(&mut out, &RAW_URL.encode(public_bytes));
    out.push_str(",\"proof\":");
    json::escape_string(&mut out, &RAW_URL.encode(proof));
    out.push('}');
    out.into_bytes()
}

/// The exact client-finish plaintext (`e2eeClientFinish` marshaled).
pub const CLIENT_FINISH_JSON: &[u8] = b"{\"type\":\"e2ee_client_finish\",\"version\":2}";

/// `parseE2EEClientFinish`: UTF-8 first, then JSON type/version.
pub fn parse_client_finish(plaintext: &[u8]) -> Result<(), E2eeError> {
    let text = std::str::from_utf8(plaintext).map_err(|_| E2eeError::InvalidClientFinish)?;
    let finish: FinishWire =
        serde_json::from_str(text).map_err(|_| E2eeError::InvalidClientFinish)?;
    if finish.r#type != "e2ee_client_finish" || finish.version != i64::from(VERSION) {
        return Err(E2eeError::InvalidClientFinish);
    }
    Ok(())
}

/// Serialize `e2eeServerFinish` byte-identically to Go `json.Marshal`
/// (`credential_secret` is `omitempty` — appended only when `Some`).
pub fn encode_server_finish(finish: &ServerFinish) -> Vec<u8> {
    let mut out = String::with_capacity(256);
    out.push_str("{\"type\":\"e2ee_server_finish\",\"version\":2,\"device_id\":");
    json::escape_string(&mut out, &finish.device_id);
    out.push_str(",\"credential_id\":");
    json::escape_string(&mut out, &finish.credential_id);
    out.push_str(",\"role\":");
    json::escape_string(&mut out, &finish.role);
    out.push_str(",\"locale\":");
    json::escape_string(&mut out, &finish.locale);
    out.push_str(",\"credential_version\":");
    out.push_str(&finish.credential_version.to_string());
    if let Some(secret) = &finish.credential_secret {
        out.push_str(",\"credential_secret\":");
        json::escape_string(&mut out, secret);
    }
    out.push('}');
    out.into_bytes()
}

/// Parse an `e2ee_server_finish` plaintext (client side). UTF-8 first, then
/// type/version, mirroring `parseE2EEClientFinish`.
pub fn parse_server_finish(plaintext: &[u8]) -> Result<ServerFinish, E2eeError> {
    let text = std::str::from_utf8(plaintext).map_err(|_| E2eeError::InvalidServerFinish)?;
    let finish: ServerFinishWire =
        serde_json::from_str(text).map_err(|_| E2eeError::InvalidServerFinish)?;
    if finish.r#type != "e2ee_server_finish" || finish.version != i64::from(VERSION) {
        return Err(E2eeError::InvalidServerFinish);
    }
    Ok(ServerFinish {
        device_id: finish.device_id,
        credential_id: finish.credential_id,
        role: finish.role,
        locale: finish.locale,
        credential_version: finish.credential_version,
        credential_secret: finish.credential_secret,
    })
}

/// The server-side handshake core: a synchronous state machine holding the
/// authenticated client hello. I/O (frame read/write, timeout, auth resolver)
/// lives in the tokio driver — this type is the unit-testable kernel of
/// `performServerE2EEHandshake` minus resolver callbacks.
pub struct ServerHandshake {
    hello: ClientHello,
    binding: Vec<u8>,
}

impl ServerHandshake {
    /// Parse the client hello and verify its proof against the resolved
    /// pairing `secret`. Corresponds to `parseE2EEClientHello` +
    /// `hmac.Equal(clientHello.proof, wantClientProof)`; the resolver's
    /// `CompleteE2EEAuth(false)` bookkeeping on failure is the driver's job.
    pub fn begin(raw_hello: &[u8], secret: &[u8; SECRET_BYTES]) -> Result<Self, E2eeError> {
        let hello = parse_client_hello(raw_hello)?;
        if !verify_client_proof(secret, &hello) {
            return Err(E2eeError::ClientProofFailed);
        }
        let binding = hello.selector.binding();
        Ok(Self { hello, binding })
    }

    pub fn hello(&self) -> &ClientHello {
        &self.hello
    }

    pub fn binding(&self) -> &[u8] {
        &self.binding
    }

    /// Emit the server-hello JSON and derive the session material.
    ///
    /// `server_private` and `server_nonce` are parameters — not generated —
    /// so fixture replay can pin them; production callers draw them from a
    /// CSPRNG (`SecretKey::random`, `rand` bytes).
    pub fn respond(
        &self,
        secret: &[u8; SECRET_BYTES],
        server_private: &SecretKey,
        server_nonce: [u8; NONCE_BYTES],
    ) -> Result<(Vec<u8>, Established), E2eeError> {
        let server_public = server_private.public_key();
        let server_public_bytes: [u8; PUBLIC_KEY_BYTES] = server_public
            .to_sec1_point(false)
            .as_bytes()
            .try_into()
            .map_err(|_| E2eeError::InvalidKeyMaterial)?;
        let shared_secret = ecdh_shared(server_private, &self.hello.public_key);
        let transcript = transcript(
            &self.binding,
            &self.hello.nonce,
            &self.hello.public_bytes,
            &server_nonce,
            &server_public_bytes,
        );
        let server_proof = server_proof(secret, &transcript);
        let key_salt = key_salt(secret, &transcript);
        let keys = derive_session_keys(&shared_secret, &key_salt);
        let hello_json = encode_server_hello(&server_nonce, &server_public_bytes, &server_proof);
        Ok((
            hello_json,
            Established {
                transcript,
                shared_secret,
                key_salt,
                keys,
            },
        ))
    }
}

/// Derived material once both hellos are fixed — the input to
/// [`Session::server`]/[`Session::client`].
pub struct Established {
    transcript: Vec<u8>,
    shared_secret: [u8; 32],
    key_salt: [u8; 32],
    keys: SessionKeys,
}

impl Established {
    pub fn transcript(&self) -> &[u8] {
        &self.transcript
    }

    pub fn shared_secret(&self) -> &[u8; 32] {
        &self.shared_secret
    }

    pub fn key_salt(&self) -> &[u8; 32] {
        &self.key_salt
    }

    pub fn session_keys(&self) -> &SessionKeys {
        &self.keys
    }

    /// The relay's session: send `s2c`, receive `c2s`, both from seq 0.
    pub fn server_session(&self, codec: Codec) -> Session {
        Session::server(&self.keys, codec).expect("derived keys are 32 bytes")
    }

    /// The client's session: send `c2s`, receive `s2c`, both from seq 0.
    pub fn client_session(&self, codec: Codec) -> Session {
        Session::client(&self.keys, codec).expect("derived keys are 32 bytes")
    }
}
