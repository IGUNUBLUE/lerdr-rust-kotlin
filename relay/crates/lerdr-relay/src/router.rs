//! The dispatch seam — where decoded, authorized actions leave the session
//! actor for the (future) coordinator.
//!
//! The session actor ([`serve_connection`]) owns the pre-dispatch gates
//! itself — decode, `unknown_action`, `incompatible_protocol`,
//! `server_session_id` fencing, and the per-action `authorize` check all
//! produce `failed_before_dispatch`/`error` envelopes before [`route`] is
//! ever consulted, exactly like the Go hub's handler prologue. What arrives
//! here is therefore always a *known, compatible, authorized* action; the
//! router decides what envelopes go back.
//!
//! [`serve_connection`]: crate::session::serve_connection
//!
//! [`route`]: ActionRouter::route

use lerdr_core::protocol::{
    action_receipt_response, ActionReceipt, ActionReceiptPhase, ApiError, Inbound, Outbound,
    RequestScope,
};

use crate::auth::AuthenticatedIdentity;

/// Per-connection context handed to the router — the authenticated identity
/// plus transport bookkeeping. Borrows; routers that need owned data copy.
#[derive(Debug)]
pub struct ClientContext<'a> {
    /// `client_id` — the hub's connection label (`client-N`).
    pub client_id: &'a str,
    /// The committed auth identity (device/credential/role/locale).
    pub identity: &'a AuthenticatedIdentity,
    /// `conn.TransportName()`.
    pub transport: &'static str,
}

/// What the router produced for one inbound action: the outbound envelopes
/// to queue on the client's send buffer, in order.
#[derive(Debug, Default)]
pub struct RouterReply {
    pub outbound: Vec<Outbound>,
}

impl RouterReply {
    /// Nothing to send — the action was absorbed (e.g. a pure ack).
    pub fn empty() -> Self {
        Self {
            outbound: Vec::new(),
        }
    }

    /// One or more outbound envelopes, queued FIFO.
    pub fn send(outbound: impl Into<Vec<Outbound>>) -> Self {
        Self {
            outbound: outbound.into(),
        }
    }
}

/// The seam between the session actor and real action handling. Sync by
/// contract — the actor core is a synchronous state machine; a router that
/// needs async work (Herdr dispatch) hands off to its own tasks and replies
/// through later outbound pushes, not by blocking this call.
pub trait ActionRouter: Send {
    /// Route one decoded, protocol-checked, authorized action.
    fn route(
        &mut self,
        ctx: &ClientContext<'_>,
        scope: &RequestScope,
        message: &Inbound,
    ) -> RouterReply;

    /// `validateExactPaneTarget` (server.go:676) — the session runs this
    /// after `server_session_id` fencing and `authorize`, before the
    /// write-audit attempt: a stale or absent `target` rejects a
    /// pane-directed action with `invalid_request`, and no audit row is
    /// written. Routers without live topology (the stub, test doubles)
    /// admit everything.
    fn validate_pane_target(&self, _message: &Inbound) -> Option<ApiError> {
        None
    }
}

/// Routers behind `Box<dyn ActionRouter>` keep object-safety useful.
impl ActionRouter for Box<dyn ActionRouter> {
    fn route(
        &mut self,
        ctx: &ClientContext<'_>,
        scope: &RequestScope,
        message: &Inbound,
    ) -> RouterReply {
        (**self).route(ctx, scope, message)
    }

    fn validate_pane_target(&self, message: &Inbound) -> Option<ApiError> {
        (**self).validate_pane_target(message)
    }
}

/// The first-slice stub: every routed action gets an `action_receipt` at
/// phase `dispatched_unknown` — the honest lifecycle answer for "accepted
/// past the dispatch boundary, no backend yet". It is deliberately NOT
/// `confirmed`: nothing executed, and a terminal "outcome unknown" receipt
/// tells the client exactly that instead of leaving it waiting for
/// `awaiting_evidence → confirmed` transitions that cannot arrive.
///
/// The real router (lerdr-coord) will drive `prepared → awaiting_evidence →
/// confirmed` through the coordinator ledger and emit `command_result`
/// envelopes for unary actions; this stub exists so the session pipeline —
/// gates, send buffer, sealed writes — is exercisable end to end.
#[derive(Debug, Default)]
pub struct StubRouter;

impl StubRouter {
    pub fn new() -> Self {
        Self
    }
}

impl ActionRouter for StubRouter {
    fn route(
        &mut self,
        _ctx: &ClientContext<'_>,
        scope: &RequestScope,
        message: &Inbound,
    ) -> RouterReply {
        let receipt = ActionReceipt {
            action_id: scope.action_id.clone(),
            phase: ActionReceiptPhase::from(ActionReceiptPhase::DISPATCHED_UNKNOWN),
            error: None,
        };
        RouterReply::send(vec![Outbound::ActionReceipt(action_receipt_response(
            &message.request_id,
            receipt,
        ))])
    }
}
