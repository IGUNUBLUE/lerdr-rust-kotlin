//! The relay's canonical `agent.view.set` projection.
//!
//! `agent.view.set` installs a **transient, per-server** declarative
//! filter+sort over Herdr's agent list — it drives the desktop sidebar and
//! Herdr's own mobile Agents list, so phone and terminal share ordering.
//! Because the projection does not survive session restore or
//! `server.live_handoff`, the relay re-asserts it after every bootstrap and
//! on the `[[startup]]` hook path (which Herdr re-runs on handoff).
//!
//! The shape is the upstream documentation's canonical attention view, with
//! `source` pinned to our plugin id (`plugin:lerdr.events`):
//!
//! * filter — agents in the currently focused workspace, **or** agents
//!   `blocked`/`done` anywhere else (attention follows the user plus the
//!   things that need an answer),
//! * sort — `attention` desc, then `state_change_seq` desc.

use std::time::Duration;

use crate::capabilities::features;
use crate::error::{DispatchPhase, HerdrError};
use crate::types::{
    AgentViewField, AgentViewFilter, AgentViewSetParams, AgentViewSort, AgentViewSortField,
    AgentViewSortOrder, AgentViewValue, BuiltinViewField, BuiltinViewSortField, ViewContext,
};
use crate::{Client, FeatureState};

/// Plugin id registered by `herdr-plugin.toml` — the `source` owner Herdr
/// rejects `plugin:`-sourced sets for when the plugin is missing/disabled.
pub const AGENT_VIEW_SOURCE: &str = "plugin:lerdr.events";

/// The view's display label.
pub const AGENT_VIEW_LABEL: &str = "focus";

/// The canonical attention-sorted projection params (source
/// `plugin:lerdr.events`, label `focus`).
pub fn lerdr_agent_view() -> AgentViewSetParams {
    AgentViewSetParams {
        source: AGENT_VIEW_SOURCE.to_owned(),
        label: Some(AGENT_VIEW_LABEL.to_owned()),
        filter: Some(AgentViewFilter::Any {
            filters: vec![
                AgentViewFilter::Eq {
                    field: AgentViewField::Builtin(BuiltinViewField::WorkspaceId),
                    value: AgentViewValue::Context(ViewContext::CurrentWorkspaceId),
                },
                AgentViewFilter::In {
                    field: AgentViewField::Builtin(BuiltinViewField::Status),
                    values: vec![
                        AgentViewValue::Text("blocked".to_owned()),
                        AgentViewValue::Text("done".to_owned()),
                    ],
                },
            ],
        }),
        sort: vec![
            AgentViewSort {
                field: AgentViewSortField::Builtin(BuiltinViewSortField::Attention),
                order: AgentViewSortOrder::Desc,
            },
            AgentViewSort {
                field: AgentViewSortField::Builtin(BuiltinViewSortField::StateChangeSeq),
                order: AgentViewSortOrder::Desc,
            },
        ],
    }
}

/// Attempts per re-assert — the first try plus two retries.
const VIEW_ASSERT_ATTEMPTS: u8 = 3;
/// Delay between retries. Small: the assert rides the bootstrap/startup
/// path where a dropped projection means a wrongly ordered Agents list.
const VIEW_ASSERT_RETRY_DELAY: Duration = Duration::from_millis(250);
/// Whole-sequence cap — a wedged socket must not stall the caller.
const VIEW_ASSERT_TIMEOUT: Duration = Duration::from_secs(20);

/// How a bounded re-assert ended — the caller logs on
/// [`ViewAssertOutcome::Failed`], stays quiet otherwise.
#[derive(Debug)]
pub enum ViewAssertOutcome {
    /// The canonical view was installed (or already was — the call is
    /// idempotent).
    Installed,
    /// The capability ledger already knows `agent.view.set` is absent on
    /// this server build — no attempt was made.
    KnownUnsupported,
    /// Every attempt failed; carries the last error.
    Failed(HerdrError),
}

/// Re-assert the canonical projection ([`lerdr_agent_view`]) with bounded
/// retries. `agent.view.set` with identical params is idempotent, so the
/// non-definitive failures — `NotStarted` (bytes never left) and
/// `DispatchedUnknown` (may or may not have applied; replaying the same
/// payload is safe) — retry after [`VIEW_ASSERT_RETRY_DELAY`]; a definitive
/// `Refused` returns immediately. Skipped entirely when the ledger already
/// proved the method absent. Failures land in the capability ledger via
/// the client's noted-methods hook.
pub async fn assert_agent_view(client: &Client) -> ViewAssertOutcome {
    if client.feature(features::AGENT_VIEW_SET).state == FeatureState::Unsupported {
        return ViewAssertOutcome::KnownUnsupported;
    }
    let params = lerdr_agent_view();
    let work = async {
        let mut last = None;
        for attempt in 1..=VIEW_ASSERT_ATTEMPTS {
            match client.agent_view_set(params.clone()).await {
                Ok(_) => return Ok(()),
                Err(err) => {
                    // Refused is definitive — never retried.
                    if err.phase() == DispatchPhase::Refused {
                        return Err(err);
                    }
                    last = Some(err);
                    if attempt < VIEW_ASSERT_ATTEMPTS {
                        tokio::time::sleep(VIEW_ASSERT_RETRY_DELAY).await;
                    }
                }
            }
        }
        Err(last.expect("at least one attempt ran"))
    };
    match tokio::time::timeout(VIEW_ASSERT_TIMEOUT, work).await {
        Ok(Ok(())) => ViewAssertOutcome::Installed,
        Ok(Err(err)) => ViewAssertOutcome::Failed(err),
        Err(_) => ViewAssertOutcome::Failed(HerdrError::dispatched_msg(
            "agent.view.set reassert timed out",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The canonical payload on the wire — pinned to the upstream doc shape.
    #[test]
    fn canonical_view_wire_shape() {
        let value = serde_json::to_value(lerdr_agent_view()).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "source": "plugin:lerdr.events",
                "label": "focus",
                "filter": {
                    "op": "any",
                    "filters": [
                        {
                            "op": "eq",
                            "field": "workspace_id",
                            "value": {"context": "current_workspace_id"}
                        },
                        {
                            "op": "in",
                            "field": "status",
                            "values": ["blocked", "done"]
                        }
                    ]
                },
                "sort": [
                    {"field": "attention", "order": "desc"},
                    {"field": "state_change_seq", "order": "desc"}
                ]
            })
        );
    }
}
