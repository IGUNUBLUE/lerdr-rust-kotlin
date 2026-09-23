//! `get_conversation_history` — `handleConversationHistory` (server.go).
//!
//! The transcript machinery lives in [`crate::conversation`]; this module is
//! the action adapter: resolve the pane's agent into a [`BrowseScope`], run
//! the bounded read, then re-check the agent tuple on the authoritative
//! topology view. A pane whose agent was replaced mid-read answers
//! "Agent changed while conversation history was loading" instead of serving
//! the previous agent's transcript.
//!
//! Unavailable sources are not command failures: the oracle completes the
//! command with `page.available=false` + a `reason_code`, so `Outcome` is
//! `completed` whenever the read produced a page at all. `phase:"failed"` is
//! reserved for the dispatch-boundary errors (pane gone, read error, agent
//! swapped).

use std::path::PathBuf;

use lerdr_core::protocol::{Inbound, Outbound};
use lerdr_herdr::AgentInfo;

use super::{ActionContext, Outcome};
use crate::conversation::reader::normalize_project_context;
use crate::conversation::{BrowseRequest, BrowseScope, ConversationBrowser, ProjectContext};

const ACTION: &str = "get_conversation_history";

/// The comparable half of `coordinator.AgentState` —
/// `sameConversationTuple`: raw agent name, pane cwd, normalized project
/// context, resolved session id. The oracle also compares a per-pane
/// `Generation`; the topology projection has no equivalent counter, so the
/// tuple alone carries the check here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConversationTuple {
    agent: String,
    cwd: String,
    project: ProjectContext,
    session_id: String,
}

/// `projectContextForAgent` — `NormalizeProjectContext(agent.Agent, …)` over
/// the pane's cwd + foreground hint. `AgentInfo.agent` is `Option` where the
/// oracle's `Agent` is a plain string; `""` normalizes the same.
/// `pub(crate)` — the projector's finished-branch fence re-checks it.
pub(crate) fn conversation_tuple(agent: &AgentInfo) -> ConversationTuple {
    let provider = agent
        .agent
        .clone()
        .or_else(|| agent.agent_session.as_ref().map(|s| s.agent.clone()))
        .unwrap_or_default();
    let cwd = agent.cwd.clone().unwrap_or_default();
    ConversationTuple {
        project: normalize_project_context(
            &provider,
            ProjectContext {
                cwd: cwd.clone(),
                foreground_cwd: agent.foreground_cwd.clone().unwrap_or_default(),
            },
        ),
        agent: provider,
        cwd,
        session_id: agent
            .agent_session
            .as_ref()
            .map(|s| s.value.trim().to_owned())
            .unwrap_or_default(),
    }
}

/// The relay's `$HOME` — `os.UserHomeDir`. Reader env overrides
/// (`*_CONFIG_DIR`, `HERDR_*`/`LERDR_*` lists) are read inside
/// `crate::conversation`. `pub(crate)` — the transition projector's
/// shared `Reader` is rooted the same way.
pub(crate) fn home_dir() -> PathBuf {
    std::env::var_os("HOME").map_or_else(PathBuf::new, PathBuf::from)
}

pub(crate) async fn conversation_history(
    ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    message: &Inbound,
) -> Vec<Outbound> {
    let pane_id = message.pane_id.as_str();
    let Some(agent) = ctx.topology.pane_of(pane_id).cloned() else {
        return Outcome::failed(pane_id, "Agent is unavailable")
            .frames(request_id, ACTION, action_id);
    };
    let before = conversation_tuple(&agent);
    let request = BrowseRequest {
        scope: BrowseScope {
            provider: agent.agent.clone().unwrap_or_default(),
            cwd: agent.cwd.clone().unwrap_or_default(),
            foreground_cwd: agent.foreground_cwd.clone().unwrap_or_default(),
            session_id: agent
                .agent_session
                .as_ref()
                .map(|s| s.value.clone())
                .unwrap_or_default(),
            pane_id: pane_id.to_owned(),
            // `ServerSessionID`/`Generation` have no topology projection —
            // the browser normalizes an empty server session to "primary".
            server_session_id: String::new(),
            terminal_id: agent.terminal_id.clone(),
            generation: 0,
        },
        cursor: (!message.cursor.is_empty()).then(|| message.cursor.clone()),
        limit: message.limit,
        retry: message.retry,
    };
    // `ConversationBrowser::read_page` is `async` for API parity but its body
    // is synchronous: a bounded transcript scan plus (for the sqlite-backed
    // providers) a ~3s-capped `sqlite3` subprocess. The oracle performs the
    // same work inside its request handler; a fresh browser per request drops
    // the reader's 60s location cache, which is the correctness-preserving
    // choice — `retry` has nothing stale to evict either way.
    let browser = ConversationBrowser::new(home_dir());
    let page = match browser.read_page(request).await {
        Ok(page) => page,
        Err(_) => {
            return Outcome::failed(pane_id, "Conversation history could not be read")
                .frames(request_id, ACTION, action_id)
        }
    };
    // Post-read `sameConversationTuple` on the authoritative topology view.
    let current = ctx.handle.topology.borrow().clone();
    let Some(current_agent) = current.pane_of(pane_id) else {
        return Outcome::failed(
            pane_id,
            "Agent changed while conversation history was loading",
        )
        .frames(request_id, ACTION, action_id);
    };
    if conversation_tuple(current_agent) != before {
        return Outcome::failed(
            pane_id,
            "Agent changed while conversation history was loading",
        )
        .frames(request_id, ACTION, action_id);
    }
    Outcome::completed(pane_id, serde_json::to_value(&page).ok())
        .frames(request_id, ACTION, action_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tuple_matches_only_identical_agent_state() {
        let agent = AgentInfo {
            pane_id: "p1".to_owned(),
            agent: Some("Claude".to_owned()),
            cwd: Some("/work".to_owned()),
            foreground_cwd: Some("/work/sub".to_owned()),
            agent_session: Some(lerdr_herdr::AgentSessionInfo {
                source: String::new(),
                agent: String::new(),
                kind: Default::default(),
                value: " ses-1 ".to_owned(),
            }),
            ..AgentInfo::default()
        };
        let tuple = conversation_tuple(&agent);
        // Claude keeps absolute, non-redundant foreground hints.
        assert_eq!(tuple.project.foreground_cwd, "/work/sub");
        assert_eq!(tuple.session_id, "ses-1");

        let mut swapped = agent.clone();
        swapped.agent_session = Some(lerdr_herdr::AgentSessionInfo {
            value: "ses-2".to_owned(),
            ..agent.agent_session.clone().unwrap()
        });
        assert_ne!(conversation_tuple(&swapped), tuple);

        let mut non_claude = agent.clone();
        non_claude.agent = Some("codex".to_owned());
        // Foreground hints are Claude-only.
        assert_eq!(conversation_tuple(&non_claude).project.foreground_cwd, "");
    }
}
