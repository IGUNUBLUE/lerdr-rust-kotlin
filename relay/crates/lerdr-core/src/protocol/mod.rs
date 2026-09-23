//! `protocol` — the frozen wire contract (`protocol v3` / `herdr-e2ee-v2`),
//! ported from `internal/protocol/protocol.go`.
//!
//! - [`Inbound`] / [`Inbound::decode`]: client -> server envelopes, including
//!   the `type:"command"` unwrap and dropped-field semantics.
//! - [`Outbound`]: typed server -> client messages, byte-exact with Go's
//!   `json.Marshal` output (map-built envelopes serialize sorted-key order;
//!   struct-built payloads keep Go field order).
//! - [`classify_action`], [`requires_protocol`], [`compatible`],
//!   [`RequestScope`]: the dispatch metadata catalog.

mod inbound;
mod outbound;
mod types;

pub use inbound::{DecodeError, Inbound, TargetRef};
pub use outbound::*;
pub use types::{
    bounded_utf8, classify_action, compatible, error_codes, required_capability, requires_protocol,
    ActionClass, ActionMetadata, ActionReceipt, ActionReceiptPhase, ApiError, DeviceContext,
    DeviceRole, OpaquePage, RequestScope, AGENT_RESPONSE_COPY_CAPABILITY, CAPABILITIES,
    ENCRYPTED_WEBSOCKET_SUBPROTOCOL, HYBRID_TRANSPORT_CAPABILITY, SPEECH_SYNTHESIS_CAPABILITY,
    SPEECH_VOICE_MANAGEMENT_CAPABILITY, VERSION,
};
