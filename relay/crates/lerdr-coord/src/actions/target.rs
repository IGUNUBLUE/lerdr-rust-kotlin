//! Exact pane-target admission — `validateExactPaneTarget`
//! (server.go:345; the oracle calls it at :676 after the
//! `server_session_id` fence and authorization, before the action switch).
//!
//! Every pane-directed inbound must echo the `target` tuple the client
//! last saw in `agents` — server session id, pane id, terminal id,
//! generation, agent session id — and anything stale or absent fails
//! `invalid_request` naming the offending field. The exempt actions
//! release owner-scoped state already keyed by client+pane or speech
//! request id, so a replaced pane must not strand the old watch, size
//! lease, or synthesis over a stale exact target. The Phase-5 focus
//! family joins the exempt set except `focus_pane`: the tab/workspace/
//! agent-session ids are the authoritative addresses there — a pane
//! that changed underneath must not veto focusing a still-live tab,
//! workspace, or agent (`focus_pane` keeps the full tuple check — it
//! IS the pane address). `layout_apply` is exempt the same way: its
//! address is the `root` tree plus `workspace_id`/`tab_id`, so any
//! `pane_id` is context — a pane that changed underneath must not veto
//! rebuilding a layout around still-live panes.

use std::collections::BTreeMap;

use lerdr_core::protocol::{error_codes, ApiError, TargetRef};

use crate::topology::Topology;

/// `validateExactPaneTarget` — `None` admits the action; `Some(err)` is
/// the `invalid_request` reply the session layer sends verbatim.
pub(crate) fn validate_exact_pane_target(
    topology: &Topology,
    action_type: &str,
    pane_id: &str,
    target: Option<&TargetRef>,
    authenticated: bool,
) -> Option<ApiError> {
    if pane_id.is_empty() {
        return None;
    }
    // `focus_tab`/`focus_workspace`/`focus_agent` are addressed by
    // tab/workspace/agent-session id — any `pane_id` present is client
    // context, not the address, so neither the tuple nor pane equality
    // applies (`focus_pane` is NOT here: its pane is the address and
    // keeps the full check below). `layout_apply` joins them: the `root`
    // tree and `workspace_id`/`tab_id` are the addresses — its pane_ids
    // are requests inside the tree, not a target tuple.
    if matches!(
        action_type,
        "focus_tab" | "focus_workspace" | "focus_agent" | "layout_apply"
    ) {
        return None;
    }
    if matches!(
        action_type,
        "unwatch_pane" | "release_pane_size" | "cancel_speech"
    ) {
        if target.is_some_and(|target| target.pane_id != pane_id) {
            return Some(invalid_field("target.pane_id"));
        }
        return None;
    }
    let Some(target) = target else {
        // `!authenticated` passes (server.go:362) — in this relay every
        // routed message is post-handshake, so the call site passes true.
        if !authenticated {
            return None;
        }
        return Some(invalid_field("target"));
    };
    if target.pane_id != pane_id {
        return Some(invalid_field("target.pane_id"));
    }
    // `state.Agent(paneID)` — the projected record carries the same
    // `server_session_id`/`generation`/`agent_session_id` the `agents`
    // frame emitted, so the tuple is compared verbatim. The oracle reads
    // `agent.SessionID` — `TrimSpace(agent_session.value)` populated by
    // the enrich pass (server.go:518-519) — which `agent_state` projects
    // as `agent_session_id`.
    let Some(info) = topology.pane_of(pane_id) else {
        return Some(invalid_field("target"));
    };
    let agent = topology.agent_state(info);
    let server_session_id = if agent.server_session_id.is_empty() {
        "primary"
    } else {
        agent.server_session_id.as_str()
    };
    if target.server_session_id != server_session_id
        || target.terminal_id.is_empty()
        || target.terminal_id != agent.terminal_id
        || target.generation != agent.generation
        || target.agent_session_id != agent.agent_session_id
    {
        return Some(invalid_field("target"));
    }
    None
}

/// `protocol.NewApiError(protocol.ErrorInvalidRequest, {"field": …})`.
fn invalid_field(field: &str) -> ApiError {
    ApiError::new(
        error_codes::INVALID_REQUEST,
        BTreeMap::from([(
            "field".to_owned(),
            serde_json::Value::String(field.to_owned()),
        )]),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use lerdr_herdr::{AgentInfo, AgentSessionInfo, AgentSessionRefKind, SessionSnapshot};

    /// One agent: `pane-1` on `term-1` with session `sess-1`, epoch 0.
    fn topology() -> Topology {
        let mut topology = Topology::default();
        topology.accept(SessionSnapshot {
            agents: vec![AgentInfo {
                pane_id: "pane-1".into(),
                terminal_id: "term-1".into(),
                agent_session: Some(AgentSessionInfo {
                    source: "sess".into(),
                    agent: "devin".into(),
                    kind: AgentSessionRefKind::Id,
                    value: "sess-1".into(),
                }),
                ..AgentInfo::default()
            }],
            ..SessionSnapshot::default()
        });
        topology
    }

    /// The exact tuple `topology()`'s agent projects — what a client
    /// echoes back.
    fn target(generation: i64) -> TargetRef {
        TargetRef {
            server_session_id: "primary".into(),
            pane_id: "pane-1".into(),
            terminal_id: "term-1".into(),
            generation,
            agent_session_id: "sess-1".into(),
            ..TargetRef::default()
        }
    }

    fn field(err: &ApiError) -> &str {
        assert_eq!(err.code, error_codes::INVALID_REQUEST);
        err.args.as_ref().unwrap()["field"].as_str().unwrap()
    }

    #[test]
    fn no_pane_id_skips_the_check() {
        let t = topology();
        assert!(validate_exact_pane_target(&t, "send_text", "", None, true).is_none());
    }

    #[test]
    fn exempt_actions_only_check_target_pane_id() {
        let t = topology();
        for action in ["unwatch_pane", "release_pane_size", "cancel_speech"] {
            // No target still cleans up owner-scoped state.
            assert!(
                validate_exact_pane_target(&t, action, "pane-1", None, true).is_none(),
                "{action} without target"
            );
            // Every other field may be stale — only pane_id is checked.
            let mut stale = target(99);
            stale.terminal_id = "gone".into();
            stale.server_session_id = String::new();
            assert!(
                validate_exact_pane_target(&t, action, "pane-1", Some(&stale), true).is_none(),
                "{action} with stale tuple"
            );
            let mut wrong = target(0);
            wrong.pane_id = "pane-2".into();
            let err = validate_exact_pane_target(&t, action, "pane-1", Some(&wrong), true).unwrap();
            assert_eq!(field(&err), "target.pane_id", "{action}");
        }
    }

    #[test]
    fn missing_target_is_invalid_once_authenticated() {
        let t = topology();
        let err = validate_exact_pane_target(&t, "send_text", "pane-1", None, true).unwrap();
        assert_eq!(field(&err), "target");
        // Pre-handshake callers get a pass (server.go:362) — unreachable
        // for routed messages but kept for oracle parity.
        assert!(validate_exact_pane_target(&t, "send_text", "pane-1", None, false).is_none());
    }

    #[test]
    fn mismatched_target_pane_id_is_invalid() {
        let t = topology();
        let mut wrong = target(0);
        wrong.pane_id = "pane-2".into();
        let err =
            validate_exact_pane_target(&t, "send_text", "pane-1", Some(&wrong), true).unwrap();
        assert_eq!(field(&err), "target.pane_id");
    }

    #[test]
    fn unknown_pane_is_invalid() {
        let t = topology();
        let mut unknown = target(0);
        unknown.pane_id = "pane-9".into();
        let err =
            validate_exact_pane_target(&t, "send_text", "pane-9", Some(&unknown), true).unwrap();
        assert_eq!(field(&err), "target");
    }

    #[test]
    fn stale_target_fields_are_invalid() {
        let t = topology();
        let cases: Vec<fn(&mut TargetRef)> = vec![
            |t| t.server_session_id = "secondary".into(),
            |t| t.server_session_id = String::new(),
            |t| t.terminal_id = "term-2".into(),
            |t| t.terminal_id = String::new(),
            |t| t.generation = 9,
            |t| t.agent_session_id = "sess-2".into(),
            |t| t.agent_session_id = String::new(),
        ];
        for (index, mutate) in cases.into_iter().enumerate() {
            let mut stale = target(0);
            mutate(&mut stale);
            let err = validate_exact_pane_target(&t, "read_pane", "pane-1", Some(&stale), true)
                .unwrap_or_else(|| panic!("case {index} should fail"));
            assert_eq!(field(&err), "target", "case {index}");
        }
    }

    #[test]
    fn exact_target_passes() {
        let t = topology();
        assert!(
            validate_exact_pane_target(&t, "send_text", "pane-1", Some(&target(0)), true).is_none()
        );
    }

    #[test]
    fn bumped_generation_rejects_the_stale_tuple() {
        let mut t = topology();
        t.bump_generation("pane-1");
        let err =
            validate_exact_pane_target(&t, "send_text", "pane-1", Some(&target(0)), true).unwrap();
        assert_eq!(field(&err), "target");
        assert!(
            validate_exact_pane_target(&t, "send_text", "pane-1", Some(&target(1)), true).is_none()
        );
    }

    #[test]
    fn non_pane_addressed_actions_skip_target_checks() {
        let t = topology();
        // `focus_tab`/`focus_workspace`/`focus_agent` are addressed by
        // tab/workspace/session id — any pane fields are context, and a
        // stale (or absent, or mismatched) pane identity never vetoes.
        // `layout_apply` is addressed by its `root` tree — same rule.
        for action in [
            "focus_tab",
            "focus_workspace",
            "focus_agent",
            "layout_apply",
        ] {
            assert!(
                validate_exact_pane_target(&t, action, "pane-1", None, true).is_none(),
                "{action} without target"
            );
            let mut stale = target(99);
            stale.pane_id = "pane-2".into(); // even a mismatch passes —
            stale.terminal_id = "gone".into();
            assert!(
                validate_exact_pane_target(&t, action, "pane-1", Some(&stale), true).is_none(),
                "{action} with a stale/mismatched pane"
            );
        }
        // `focus_pane` is NOT exempt — the pane is its address, so the
        // full tuple check applies.
        let mut stale = target(0);
        stale.terminal_id = "gone".into();
        let err =
            validate_exact_pane_target(&t, "focus_pane", "pane-1", Some(&stale), true).unwrap();
        assert_eq!(field(&err), "target");
        assert!(
            validate_exact_pane_target(&t, "focus_pane", "pane-1", Some(&target(0)), true)
                .is_none()
        );
    }

    #[test]
    fn sessionless_pane_expects_empty_agent_session_id() {
        let mut t = Topology::default();
        t.accept(SessionSnapshot {
            agents: vec![AgentInfo {
                pane_id: "pane-1".into(),
                terminal_id: "term-1".into(),
                ..AgentInfo::default()
            }],
            ..SessionSnapshot::default()
        });
        let mut target = target(0);
        // The client omits `agent_session_id` for a sessionless pane —
        // decoded as "" — and that validates.
        target.agent_session_id = String::new();
        assert!(
            validate_exact_pane_target(&t, "read_pane", "pane-1", Some(&target), true).is_none()
        );
        target.agent_session_id = "sess-1".into();
        let err =
            validate_exact_pane_target(&t, "read_pane", "pane-1", Some(&target), true).unwrap();
        assert_eq!(field(&err), "target");
    }
}
