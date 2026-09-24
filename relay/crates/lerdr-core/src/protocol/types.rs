//! Shared protocol vocabulary: versions, roles, receipt phases, the action
//! catalog, and the bounded `ApiError` — ported from
//! `internal/protocol/protocol.go`.

use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize};

use super::inbound::TargetRef;
use crate::json::{de_default, opt_map_is_empty};

/// `protocol.Version` — the frozen wire protocol.
pub const VERSION: i64 = 3;

/// `protocol.EncryptedWebSocketSubprotocol`.
pub const ENCRYPTED_WEBSOCKET_SUBPROTOCOL: &str = "herdr-e2ee-v2";

/// `protocol.HybridTransportCapability`.
pub const HYBRID_TRANSPORT_CAPABILITY: &str = "herdr-hybrid-v2";

/// `protocol.AgentResponseCopyCapability`.
pub const AGENT_RESPONSE_COPY_CAPABILITY: &str = "agent_response_copy";
/// `protocol.SpeechSynthesisCapability`.
pub const SPEECH_SYNTHESIS_CAPABILITY: &str = "speech_synthesis";
/// `protocol.SpeechVoiceManagementCapability`.
pub const SPEECH_VOICE_MANAGEMENT_CAPABILITY: &str = "speech_voice_management";

/// `protocol.Capabilities` — the capability list advertised in `push_config`.
///
/// `"focus"` was the first Phase-5 §0 addition; `"pane_search"`,
/// `"pane_links"`, and `"layout"` cover the §1 pane-content families.
/// The relay advertises each while Herdr evidence does not refute the
/// whole backing method family (`lerdr-coord`'s `caps_update` carries
/// the mid-session flip).
pub const CAPABILITIES: &[&str] = &[
    "attention_classification",
    "clear_activities",
    "directory_browser",
    "workspace_management",
    "worktree_management",
    "self_update",
    "structured_questions",
    "slash_commands",
    "conversation_history",
    "pane_size_lease",
    "pane_size_lease_rows",
    "workspace_inspection",
    "semantic_input",
    "secret_input",
    "invitation_qr",
    "focus",
    "pane_search",
    "pane_links",
    "layout",
];

/// Error codes emitted by the relay (`ErrorInvalidRequest` etc.).
pub mod error_codes {
    pub const INVALID_REQUEST: &str = "invalid_request";
    pub const UNKNOWN_ACTION: &str = "unknown_action";
    pub const INCOMPATIBLE_PROTOCOL: &str = "incompatible_protocol";
    pub const READER_DENIED: &str = "reader_denied";
    /// Phase-5 §0 — the action's required capability is not live on this
    /// session (absent from the advertised set, the client's announced
    /// set, or both).
    pub const CAPABILITY_UNSUPPORTED: &str = "capability_unsupported";
}

/// `ActionReceiptPhase` — a wire string; unknown phases round-trip.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct ActionReceiptPhase(pub String);

impl ActionReceiptPhase {
    pub const PREPARED: &'static str = "prepared";
    pub const FAILED_BEFORE_DISPATCH: &'static str = "failed_before_dispatch";
    pub const AWAITING_EVIDENCE: &'static str = "awaiting_evidence";
    pub const CONFIRMED: &'static str = "confirmed";
    pub const DISPATCHED_UNKNOWN: &'static str = "dispatched_unknown";

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for ActionReceiptPhase {
    fn from(s: &str) -> Self {
        Self(s.to_owned())
    }
}

impl From<String> for ActionReceiptPhase {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl Serialize for ActionReceiptPhase {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ActionReceiptPhase {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(Self(String::deserialize(d)?))
    }
}

/// Device role assigned at handshake (`e2ee_server_finish`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct DeviceRole(pub String);

impl DeviceRole {
    pub const READER: &'static str = "reader";
    pub const CONTROLLER: &'static str = "controller";
    pub const BOOTSTRAP: &'static str = "bootstrap";

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for DeviceRole {
    fn from(s: &str) -> Self {
        Self(s.to_owned())
    }
}

impl Serialize for DeviceRole {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for DeviceRole {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(Self(String::deserialize(d)?))
    }
}

/// `protocol.DeviceContext` — identity delivered in `e2ee_server_finish`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceContext {
    #[serde(default, deserialize_with = "de_default")]
    pub device_id: String,
    #[serde(default, deserialize_with = "de_default")]
    pub credential_id: String,
    #[serde(default, deserialize_with = "de_default")]
    pub role: DeviceRole,
    #[serde(default, deserialize_with = "de_default")]
    pub locale: String,
    #[serde(default, deserialize_with = "de_default")]
    pub credential_version: i64,
}

/// `protocol.OpaquePage` — cursor page wrapper used by history endpoints.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(bound(deserialize = "T: serde::Deserialize<'de>"))]
pub struct OpaquePage<T> {
    #[serde(default, deserialize_with = "de_default")]
    pub items: Vec<T>,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub next_cursor: String,
    #[serde(default, deserialize_with = "de_default")]
    pub truncated: bool,
    #[serde(default, deserialize_with = "de_default")]
    pub generated_at: i64,
}

/// `protocol.ApiError` — bounded error object embedded in `error` and
/// `action_receipt` envelopes.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ApiError {
    #[serde(default, deserialize_with = "de_default")]
    pub code: String,
    #[serde(default, skip_serializing_if = "opt_map_is_empty")]
    pub args: Option<BTreeMap<String, serde_json::Value>>,
}

const MAX_ERROR_ARGS: usize = 8;
const MAX_ERROR_ARG_KEY: usize = 32;
const MAX_ERROR_ARG_STRING: usize = 256;
const MAX_ERROR_ARGS_JSON: usize = 1024;
const MAX_ERROR_CODE: usize = 64;
/// `±(2^53 - 1)` — the integer range `boundedErrorArg` admits.
const MAX_SAFE_INT: i64 = (1 << 53) - 1;

impl ApiError {
    /// `protocol.NewApiError`: normalize `code`, bound args (≤8 entries,
    /// sorted; keys lowercased ≤32 chars; string args ≤256 chars; numeric
    /// args integral within ±(2^53−1)); the encoded args object must stay
    /// ≤1024 bytes.
    pub fn new(code: &str, args: BTreeMap<String, serde_json::Value>) -> Self {
        let mut result = ApiError {
            code: bounded_utf8(code.trim().to_lowercase().as_str(), MAX_ERROR_CODE),
            args: None,
        };
        if args.is_empty() {
            return result;
        }
        // Keys are sorted before truncation: BTreeMap iteration is already
        // ascending, matching Go's sort.Strings.
        let mut bounded = BTreeMap::new();
        for (key, value) in args {
            if bounded.len() == MAX_ERROR_ARGS {
                break;
            }
            let key = bounded_utf8(key.trim().to_lowercase().as_str(), MAX_ERROR_ARG_KEY);
            if key.is_empty() {
                continue;
            }
            let Some(value) = bounded_error_arg(value) else {
                continue;
            };
            bounded.insert(key.clone(), value);
            if crate::json::to_vec(&bounded)
                .map(|encoded| encoded.len() > MAX_ERROR_ARGS_JSON)
                .unwrap_or(true)
            {
                bounded.remove(&key);
                break;
            }
        }
        result.args = if bounded.is_empty() {
            None
        } else {
            Some(bounded)
        };
        result
    }
}

/// `boundedErrorArg`: keep strings (bounded), bools, and integral numbers in
/// the ±(2^53−1) range; drop everything else.
fn bounded_error_arg(value: serde_json::Value) -> Option<serde_json::Value> {
    match value {
        serde_json::Value::String(s) => Some(serde_json::Value::String(bounded_utf8(
            &s,
            MAX_ERROR_ARG_STRING,
        ))),
        serde_json::Value::Bool(_) => Some(value),
        serde_json::Value::Number(ref n) => {
            if let Some(i) = n.as_i64() {
                return (-MAX_SAFE_INT..=MAX_SAFE_INT).contains(&i).then_some(value);
            }
            if let Some(u) = n.as_u64() {
                return (u <= MAX_SAFE_INT as u64).then_some(value);
            }
            if let Some(f) = n.as_f64() {
                // Go keeps the float64; its marshal emits integral values
                // without a fraction — normalize to i64 for the same bytes.
                let integral = f.is_finite()
                    && f.trunc() == f
                    && f >= -(MAX_SAFE_INT as f64)
                    && f <= MAX_SAFE_INT as f64;
                return integral.then(|| serde_json::Value::from(f as i64));
            }
            None
        }
        _ => None,
    }
}

/// `boundedUTF8`: truncate at `limit` bytes, backing off to a UTF-8 boundary.
pub fn bounded_utf8(value: &str, limit: usize) -> String {
    if value.len() <= limit {
        return value.to_owned();
    }
    let mut end = limit;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

/// `protocol.ActionReceipt`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ActionReceipt {
    #[serde(default, deserialize_with = "de_default")]
    pub action_id: String,
    #[serde(default, deserialize_with = "de_default")]
    pub phase: ActionReceiptPhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ApiError>,
}

/// `protocol.ActionClass`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionClass {
    ReadOnly,
    Mutating,
}

/// `protocol.ActionMetadata`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActionMetadata {
    pub operation: &'static str,
    pub class: ActionClass,
    pub requires_protocol: bool,
    pub coordinated: bool,
    pub audited: bool,
}

const fn read_action(operation: &'static str) -> ActionMetadata {
    ActionMetadata {
        operation,
        class: ActionClass::ReadOnly,
        requires_protocol: false,
        coordinated: false,
        audited: false,
    }
}

const fn mutate_action(
    operation: &'static str,
    coordinated: bool,
    audited: bool,
) -> ActionMetadata {
    ActionMetadata {
        operation,
        class: ActionClass::Mutating,
        requires_protocol: true,
        coordinated,
        audited,
    }
}

/// install_update is the single mutation exempt from `protocol:3` (it must
/// work for clients stuck on an older protocol).
const fn mutate_action_unversioned(
    operation: &'static str,
    coordinated: bool,
    audited: bool,
) -> ActionMetadata {
    ActionMetadata {
        operation,
        class: ActionClass::Mutating,
        requires_protocol: false,
        coordinated,
        audited,
    }
}

/// `protocol.actionCatalog` — the oracle's 70 actions plus the Phase-5
/// additions: `client_caps`/`caps_update` negotiation, the `focus_*`
/// family (docs/13 §§0-1.1), and the §1 pane-content families
/// (`pane_search`, `pane_selection_read`, `pane_link_resolve`,
/// `pane_link_activate`, `layout_export`, `layout_apply`). The
/// negotiation frames classify `ReadOnly` so any authenticated device
/// may announce its set; they are absorbed by the session layer before
/// routing.
pub fn classify_action(operation: &str) -> Option<ActionMetadata> {
    let metadata = match operation {
        "acknowledge_pane" => mutate_action("acknowledge_pane", true, false),
        "agent_clear" => mutate_action("agent_clear", true, true),
        "agent_rename" => mutate_action("agent_rename", true, true),
        "agent_restart" => mutate_action("agent_restart", true, true),
        "agent_start" => mutate_action("agent_start", true, true),
        "agent_stop" => mutate_action("agent_stop", true, true),
        "answer_question" => mutate_action("answer_question", true, true),
        "cancel_speech" => read_action("cancel_speech"),
        "caps_update" => read_action("caps_update"),
        "check_update" => read_action("check_update"),
        "clarify_question" => mutate_action("clarify_question", true, true),
        "clear_activities" => mutate_action("clear_activities", false, false),
        "client_caps" => read_action("client_caps"),
        "copy_agent_response" => mutate_action("copy_agent_response", false, false),
        "deploy_app_update" => mutate_action("deploy_app_update", false, false),
        "create_device_invitation" => mutate_action("create_device_invitation", false, true),
        "device_list" => read_action("device_list"),
        "focus_agent" => mutate_action("focus_agent", true, false),
        "focus_pane" => mutate_action("focus_pane", true, false),
        "focus_tab" => mutate_action("focus_tab", true, false),
        "focus_workspace" => mutate_action("focus_workspace", true, false),
        "get_activity" => read_action("get_activity"),
        "get_conversation_history" => read_action("get_conversation_history"),
        "install_update" => mutate_action_unversioned("install_update", false, false),
        "lease_pane_size" => mutate_action("lease_pane_size", true, false),
        "list_directories" => read_action("list_directories"),
        "list_slash_commands" => read_action("list_slash_commands"),
        "layout_apply" => mutate_action("layout_apply", true, true),
        "layout_export" => read_action("layout_export"),
        "navigate_question" => mutate_action("navigate_question", true, true),
        "pane_applied" => read_action("pane_applied"),
        "pane_link_activate" => mutate_action("pane_link_activate", true, false),
        "pane_link_resolve" => read_action("pane_link_resolve"),
        "pane_search" => read_action("pane_search"),
        "pane_selection_read" => read_action("pane_selection_read"),
        "push_open_ref" => read_action("push_open_ref"),
        "push_policy_get" => read_action("push_policy_get"),
        "push_policy_set" => mutate_action("push_policy_set", false, false),
        "push_snooze" => mutate_action("push_snooze", false, false),
        "push_subscribe" => mutate_action("push_subscribe", false, false),
        "push_test_device" => mutate_action("push_test_device", false, true),
        "push_unsubscribe" => mutate_action("push_unsubscribe", false, false),
        "push_viewed_pane" => mutate_action("push_viewed_pane", false, false),
        "qr_code" => read_action("qr_code"),
        "read_pane" => read_action("read_pane"),
        "refresh_agents" => read_action("refresh_agents"),
        "register_app_origin" => mutate_action("register_app_origin", false, false),
        "release_pane_size" => mutate_action("release_pane_size", true, false),
        "rename_device" => mutate_action("rename_device", false, true),
        "respond" => mutate_action("respond", true, true),
        "send_keys" => mutate_action("send_keys", true, true),
        "send_input" => mutate_action("send_input", true, true),
        "send_secret" => mutate_action("send_secret", true, true),
        "reset_devices" => mutate_action("reset_devices", false, true),
        "revoke_device" => mutate_action("revoke_device", false, true),
        "send_text" => mutate_action("send_text", true, true),
        "speak_text" => read_action("speak_text"),
        "speech_voice_install" => mutate_action("speech_voice_install", false, true),
        "speech_voice_remove" => mutate_action("speech_voice_remove", false, true),
        "speech_voices_list" => read_action("speech_voices_list"),
        "submit_prompt" => mutate_action("submit_prompt", true, true),
        "tab_reorder" => mutate_action("tab_reorder", true, true),
        "unwatch_pane" => read_action("unwatch_pane"),
        "upload_begin" => mutate_action("upload_begin", false, true),
        "upload_cancel" => mutate_action("upload_cancel", false, true),
        "upload_chunk" => mutate_action("upload_chunk", false, true),
        "upload_finish" => mutate_action("upload_finish", false, true),
        "watch_pane" => read_action("watch_pane"),
        "webrtc_close" => read_action("webrtc_close"),
        "webrtc_ice" => read_action("webrtc_ice"),
        "webrtc_offer" => read_action("webrtc_offer"),
        "workspace_close" => mutate_action("workspace_close", true, true),
        "workspace_create" => mutate_action("workspace_create", true, true),
        "workspace_file" => read_action("workspace_file"),
        "workspace_git_diff" => read_action("workspace_git_diff"),
        "workspace_git_status" => read_action("workspace_git_status"),
        "workspace_rename" => mutate_action("workspace_rename", true, true),
        "workspace_reorder" => mutate_action("workspace_reorder", true, true),
        "workspace_tree" => read_action("workspace_tree"),
        "worktree_create" => mutate_action("worktree_create", true, true),
        "worktree_list" => read_action("worktree_list"),
        "worktree_open" => mutate_action("worktree_open", true, true),
        "worktree_remove" => mutate_action("worktree_remove", true, true),
        _ => return None,
    };
    Some(metadata)
}

/// `protocol.RequiresProtocol` — unknown actions require the protocol field.
pub fn requires_protocol(message_type: &str) -> bool {
    classify_action(message_type).is_none_or(|m| m.requires_protocol)
}

/// The negotiated capability an action is gated on — Phase-5 §0's
/// `capabilities` exchange. `None` means the action dispatches regardless
/// of negotiation (every pre-Phase-5 action). A gated action is live on a
/// session only while the capability appears on BOTH the server's
/// advertised list (`push_config`/`caps_update`) and the client's
/// announced list (`client_caps`/inbound `caps_update`).
pub fn required_capability(operation: &str) -> Option<&'static str> {
    match operation {
        "focus_pane" | "focus_tab" | "focus_workspace" | "focus_agent" => Some("focus"),
        "pane_search" | "pane_selection_read" => Some("pane_search"),
        "pane_link_resolve" | "pane_link_activate" => Some("pane_links"),
        "layout_export" | "layout_apply" => Some("layout"),
        _ => None,
    }
}

/// `protocol.RequestScope` — the dispatch scope extracted from an [`Inbound`].
#[derive(Debug, Clone, PartialEq)]
pub struct RequestScope {
    pub action: ActionMetadata,
    pub target: Option<TargetRef>,
    pub action_id: String,
    pub server_session_id: String,
    pub session_id: String,
}

impl RequestScope {
    /// `protocol.ScopeFor` — `None` when the action is unknown.
    pub fn for_message(message: &super::inbound::Inbound) -> Option<Self> {
        let action = classify_action(&message.r#type)?;
        Some(Self {
            action,
            target: message.target.clone(),
            action_id: message.action_id.clone(),
            server_session_id: message.server_session_id.clone(),
            session_id: message.session_id.clone(),
        })
    }
}

/// `protocol.Compatible` — non-protocol-gated actions pass regardless.
pub fn compatible(message: &super::inbound::Inbound) -> bool {
    !requires_protocol(&message.r#type) || message.protocol == VERSION
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_error_bounds_args() {
        let mut args = BTreeMap::new();
        args.insert("UPPER".to_owned(), serde_json::json!("ok"));
        args.insert(
            "bad\u{0}key".to_owned(),
            serde_json::json!("dropped?".to_owned()),
        );
        let err = ApiError::new("  Unknown_Action ", args);
        assert_eq!(err.code, "unknown_action");
        let args = err.args.unwrap();
        assert_eq!(args["upper"], serde_json::json!("ok"));
    }

    #[test]
    fn api_error_drops_out_of_range_numbers() {
        let mut args = BTreeMap::new();
        args.insert("big".to_owned(), serde_json::json!(9007199254740993u64));
        args.insert("frac".to_owned(), serde_json::json!(1.5));
        args.insert("ok".to_owned(), serde_json::json!(7));
        let err = ApiError::new("x", args);
        let args = err.args.unwrap();
        assert_eq!(args.len(), 1);
        assert_eq!(args["ok"], serde_json::json!(7));
    }

    #[test]
    fn catalog_classifies_phase5_actions() {
        let known = [
            "acknowledge_pane",
            "client_caps",
            "caps_update",
            "focus_pane",
            "focus_tab",
            "focus_workspace",
            "focus_agent",
            "install_update",
            "layout_apply",
            "layout_export",
            "pane_link_activate",
            "pane_link_resolve",
            "pane_search",
            "pane_selection_read",
            "watch_pane",
            "worktree_remove",
        ];
        for action in known {
            assert!(classify_action(action).is_some(), "{action}");
        }
        assert!(!classify_action("install_update").unwrap().requires_protocol);
        assert!(classify_action("send_text").unwrap().requires_protocol);
        assert!(requires_protocol("nonsense_action"));
        assert!(classify_action("nonsense_action").is_none());
    }

    #[test]
    fn focus_actions_are_mutating_and_capability_gated() {
        for action in ["focus_pane", "focus_tab", "focus_workspace", "focus_agent"] {
            let meta = classify_action(action).unwrap();
            assert_eq!(meta.class, ActionClass::Mutating, "{action}");
            assert!(meta.requires_protocol, "{action}");
            assert_eq!(required_capability(action), Some("focus"), "{action}");
        }
        // Negotiation frames are read-only and never capability-gated.
        for action in ["client_caps", "caps_update"] {
            let meta = classify_action(action).unwrap();
            assert_eq!(meta.class, ActionClass::ReadOnly, "{action}");
            assert!(!meta.requires_protocol, "{action}");
            assert_eq!(required_capability(action), None, "{action}");
        }
        assert_eq!(required_capability("send_text"), None);
        assert!(CAPABILITIES.contains(&"focus"));
    }

    /// docs/13 §1 — the pane-content families' [R]/[M,C]/[M,C,A] marks
    /// and their capability names.
    #[test]
    fn pane_content_actions_classify_and_gate_per_spec() {
        for action in [
            "pane_search",
            "pane_selection_read",
            "pane_link_resolve",
            "layout_export",
        ] {
            let meta = classify_action(action).unwrap();
            assert_eq!(meta.class, ActionClass::ReadOnly, "{action}");
            assert!(!meta.audited, "{action}");
            assert!(!meta.coordinated, "{action}");
        }
        // [M,C] mutating+coordinated but not audited.
        let activate = classify_action("pane_link_activate").unwrap();
        assert_eq!(activate.class, ActionClass::Mutating);
        assert!(activate.requires_protocol);
        assert!(activate.coordinated);
        assert!(!activate.audited);
        // [M,C,A] — the only audited action of the six.
        let apply = classify_action("layout_apply").unwrap();
        assert_eq!(apply.class, ActionClass::Mutating);
        assert!(apply.requires_protocol);
        assert!(apply.coordinated);
        assert!(apply.audited);

        assert_eq!(required_capability("pane_search"), Some("pane_search"));
        assert_eq!(
            required_capability("pane_selection_read"),
            Some("pane_search")
        );
        assert_eq!(required_capability("pane_link_resolve"), Some("pane_links"));
        assert_eq!(
            required_capability("pane_link_activate"),
            Some("pane_links")
        );
        assert_eq!(required_capability("layout_export"), Some("layout"));
        assert_eq!(required_capability("layout_apply"), Some("layout"));
        for cap in ["pane_search", "pane_links", "layout"] {
            assert!(CAPABILITIES.contains(&cap), "{cap}");
        }
    }
}
