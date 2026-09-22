//! `HerdRouter` — the [`ActionRouter`] that dispatches client actions to
//! Herdr and owns this session's pane watches.
//!
//! Contract notes:
//!
//! - `route()` is **sync** (actor-core rule): Herdr calls are spawned onto
//!   the runtime and their results arrive as [`ClientSink`] pushes —
//!   result message first, `action_receipt` last, so the client sees
//!   content before the lifecycle terminal.
//! - Receipt phases follow the dispatch-boundary taxonomy (doc 08 rule 4 /
//!   `lerdr_herdr::error::HerdrError::phase`): `NotStarted` →
//!   `failed_before_dispatch`, `DispatchedUnknown` → `dispatched_unknown`,
//!   `Refused` → `confirmed` carrying the refusal in `error`.
//! - Actions without a backend yet answer `dispatched_unknown` — the same
//!   honest terminal state `StubRouter` gives.
//! - Topology broadcasts are per-connection: each router forwards
//!   `workspaces`/`agents`/`herdr_status` to its own client on every
//!   revision (all replaceable — bursts coalesce in the send buffer).

use std::collections::BTreeMap;
use std::sync::Arc;

use lerdr_core::audit;
use lerdr_core::protocol::{
    action_receipt_response, error_codes, error_response, ActionReceipt, ActionReceiptPhase,
    ApiError, Inbound, Outbound, PaneContent, RequestScope,
};
use lerdr_herdr::{DispatchPhase, HerdrError, PaneReadResult, ReadFormat, ReadSource};
use lerdr_relay::router::{ActionRouter, ClientContext, RouterReply};
use lerdr_relay::session::ClientSink;
use tokio_util::sync::CancellationToken;
use tracing::Instrument;

use crate::actions::{self, ActionContext};
use crate::actor::TopologyHandle;
use crate::snapshot::topology_broadcast;
use crate::watches::{watch_interval, WatchSet, WatchSpec, DEFAULT_LINES};

/// How a router resolves its own client's push endpoint. Wired to
/// `Relay::client_sink` at construction — the lookup is lazy because the
/// sink registers only after the handshake commits.
pub type ClientSinkLookup = Arc<dyn Fn(&str) -> Option<ClientSink> + Send + Sync>;

/// Cross-session action state — the oracle's singletons (`paneSizeM`,
/// the acknowledgment ledger, the profile resolver, the question store,
/// the upload manager, the activity journal) live once per relay,
/// not once per connection.
struct ActionShared {
    leases: actions::leases::Leases,
    acks: actions::Acks,
    profiles: actions::profiles::Resolver,
    questions: actions::questions::Questions,
    uploads: actions::uploads::Uploads,
    activities: actions::activity::Journal,
    push: actions::push::Push,
    speech: actions::speech::Speech,
    notices: actions::Notices,
    /// `s.auditLog` — spawned handlers append `result` rows here; the
    /// session layer owns `attempt` rows and admin results.
    audit: Option<Arc<audit::AuditLog>>,
}

/// Builds one [`HerdRouter`] per accepted session.
#[derive(Clone)]
pub struct HerdRouterFactory {
    handle: TopologyHandle,
    sink_of: ClientSinkLookup,
    cancel: CancellationToken,
    shared: Arc<ActionShared>,
}

impl HerdRouterFactory {
    /// `handle` comes from [`crate::TopologyActor::spawn`]; `sink_of` is
    /// typically `move |id| relay.client_sink(id)`; `cancel` should be the
    /// relay shutdown token (watches and the lease sweeper die with the
    /// relay).
    /// `runtime_dir` roots the persisted subsystems — uploads stage under
    /// `runtime_dir/uploads`, push state under `runtime_dir/push`, the
    /// activity journal under `runtime_dir/activity` (the oracle's
    /// data-dir layout). `audit` is the process-wide write-audit log the
    /// session layer also records into (`audit.Open(cfg.CacheDir)`).
    pub fn new(
        handle: TopologyHandle,
        sink_of: ClientSinkLookup,
        cancel: CancellationToken,
        runtime_dir: std::path::PathBuf,
        audit: Option<Arc<audit::AuditLog>>,
    ) -> Self {
        let leases = actions::leases::Leases::new(handle.client.clone());
        leases.spawn_sweeper(cancel.clone());
        Self {
            handle,
            sink_of,
            cancel,
            shared: Arc::new(ActionShared {
                leases,
                acks: actions::Acks::default(),
                profiles: actions::profiles::Resolver::new(),
                questions: actions::questions::Questions::default(),
                uploads: actions::uploads::Uploads::new(runtime_dir.join("uploads")),
                activities: actions::activity::Journal::open(&runtime_dir.join("activity"))
                    .unwrap_or_else(|err| {
                        tracing::warn!("activity journal unavailable ({err}); running in-memory");
                        actions::activity::Journal::default()
                    }),
                push: actions::push::Push::new(&runtime_dir.join("push")).unwrap_or_else(|err| {
                    tracing::warn!("push persistence unavailable ({err}); running in-memory");
                    actions::push::Push::default()
                }),
                speech: actions::speech::Speech::default(),
                notices: actions::Notices::default(),
                audit,
            }),
        }
    }

    /// `d.broadcast` — forward journal events (`activity` rows,
    /// `activity_history` clears) through `broadcast` until `cancel`
    /// fires or the journal closes.
    pub fn spawn_activity_broadcast(
        &self,
        broadcast: impl Fn(&Outbound) + Send + Sync + 'static,
        cancel: CancellationToken,
    ) {
        let journal = self.shared.activities.clone();
        tokio::spawn(async move {
            let mut rx = journal.subscribe();
            loop {
                tokio::select! {
                    () = cancel.cancelled() => break,
                    event = rx.recv() => match event {
                        Ok(event) => broadcast(&event.into_outbound()),
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    },
                }
            }
        });
    }

    /// `hub.Broadcast`/`broadcastToAll` — drains the shared notices
    /// channel: voice-catalog changes, `update_status`, any relay-wide
    /// frame a handler emits beside its response. `exclude_client` skips
    /// the requester when it already carries the frame.
    pub fn spawn_notice_broadcast(
        &self,
        broadcast_except: impl Fn(&Outbound, &str) + Send + Sync + 'static,
        cancel: CancellationToken,
    ) {
        let mut rx = self.shared.notices.subscribe();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    () = cancel.cancelled() => break,
                    event = rx.recv() => match event {
                        Ok(notice) => broadcast_except(&notice.frame, &notice.exclude_client),
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    },
                }
            }
        });
    }

    /// The closure `Relay::with_router_factory` expects.
    pub fn into_factory(self) -> impl Fn() -> Box<dyn ActionRouter> + Send + Sync {
        move || {
            Box::new(HerdRouter::new(
                self.handle.clone(),
                self.sink_of.clone(),
                self.cancel.child_token(),
                self.shared.clone(),
            )) as Box<dyn ActionRouter>
        }
    }
}

/// Per-session router. One instance per client connection.
pub struct HerdRouter {
    handle: TopologyHandle,
    sink_of: ClientSinkLookup,
    cancel: CancellationToken,
    shared: Arc<ActionShared>,
    /// Lazily captured from the first `ClientContext`.
    client_id: Option<String>,
    /// `client.Identity().DeviceID` — captured alongside `client_id`.
    device_id: Option<String>,
    watches: WatchSet,
    /// Forwards topology revisions to this client.
    forwarder: Option<tokio::task::JoinHandle<()>>,
}

impl HerdRouter {
    fn new(
        handle: TopologyHandle,
        sink_of: ClientSinkLookup,
        cancel: CancellationToken,
        shared: Arc<ActionShared>,
    ) -> Self {
        Self {
            handle,
            sink_of,
            cancel,
            shared,
            client_id: None,
            device_id: None,
            watches: WatchSet::default(),
            forwarder: None,
        }
    }

    /// First-route initialization: client id + topology forwarder.
    fn ensure_started(&mut self, ctx: &ClientContext<'_>) {
        if self.client_id.is_some() {
            return;
        }
        self.client_id = Some(ctx.client_id.to_owned());
        self.device_id = Some(ctx.identity.device_id.clone());
        let sink_of = self.sink_of.clone();
        let mut topo_rx = self.handle.topology.clone();
        let client_id = ctx.client_id.to_owned();
        let cancel = self.cancel.clone();
        let span_id = client_id.clone();
        self.forwarder = Some(tokio::spawn(
            async move {
                // Skip the initial value — the session snapshot already
                // carries it.
                if topo_rx.changed().await.is_err() {
                    return;
                }
                loop {
                    let topology = topo_rx.borrow().clone();
                    if let Some(sink) = sink_of(&client_id) {
                        let mut gone = false;
                        for message in topology_broadcast(&topology) {
                            gone = sink.try_send(&message).is_err();
                            if gone {
                                break;
                            }
                        }
                        if gone {
                            return; // client gone — session drop stops us
                        }
                    }
                    tokio::select! {
                        _ = cancel.cancelled() => break,
                        changed = topo_rx.changed() => {
                            if changed.is_err() { break }
                        }
                    }
                }
            }
            .instrument(tracing::info_span!("topology_forwarder", client_id = %span_id)),
        ));
    }

    /// Resolve this client's push endpoint.
    fn sink(&self) -> Option<ClientSink> {
        self.client_id.as_deref().and_then(|id| (self.sink_of)(id))
    }

    /// Push `frames` to this client from an async task — result message
    /// first, receipt last; a closed/full queue stops the sequence.
    fn push_later(&self, fut: impl std::future::Future<Output = Vec<Outbound>> + Send + 'static) {
        let sink_of = self.sink_of.clone();
        let client_id = self.client_id.clone().unwrap_or_default();
        let span_id = client_id.clone();
        tokio::spawn(
            async move {
                let frames = fut.await;
                let Some(sink) = sink_of(&client_id) else {
                    return;
                };
                for message in frames {
                    if sink.try_send(&message).is_err() {
                        break;
                    }
                }
            }
            .instrument(tracing::info_span!("action", client_id = %span_id)),
        );
    }

    /// Everything a spawned action needs — the admission-time topology
    /// snapshot plus the shared subsystems.
    fn action_context(&self) -> ActionContext {
        ActionContext {
            client: self.handle.client.clone(),
            topology: self.handle.topology.borrow().clone(),
            handle: self.handle.clone(),
            leases: self.shared.leases.clone(),
            acks: self.shared.acks.clone(),
            profiles: self.shared.profiles.clone(),
            questions: self.shared.questions.clone(),
            uploads: self.shared.uploads.clone(),
            activities: self.shared.activities.clone(),
            push: self.shared.push.clone(),
            speech: self.shared.speech.clone(),
            notices: self.shared.notices.clone(),
            audit: self.shared.audit.clone(),
            client_id: self.client_id.clone().unwrap_or_default(),
            device_id: self.device_id.clone().unwrap_or_default(),
        }
    }
}

impl Drop for HerdRouter {
    fn drop(&mut self) {
        self.watches.stop_all();
        if let Some(f) = self.forwarder.take() {
            f.abort();
        }
        // `ReleaseClient` — the disconnect path drops this client's pane
        // leases immediately (no grace).
        if let (Some(client_id), Ok(runtime)) = (
            self.client_id.clone(),
            tokio::runtime::Handle::try_current(),
        ) {
            let leases = self.shared.leases.clone();
            runtime.spawn(async move {
                if let Err(err) = leases.release_client(&client_id).await {
                    tracing::warn!(client_id, error = %err, "pane size release on disconnect failed");
                }
            });
        }
    }
}

/// Spawn an [`actions`] handler for a routed action: clone the request
/// pieces into the task (handler futures borrow them — they cannot outlive
/// owned locals of a closure, so this must expand at the call site),
/// await the handler, push its frames. `route` returns immediately — the
/// global ingress lock is never held across Herdr calls.
macro_rules! spawn_action {
    ($router:expr, $request_id:expr, $action_id:expr, $message:expr, $handler:expr) => {{
        let ctx = $router.action_context();
        let rid = $request_id.clone();
        let aid = $action_id.clone();
        let msg = $message.clone();
        $router.push_later(async move {
            let frames = $handler(ctx.clone(), &rid, &aid, &msg).await;
            // `sendAuditedCommandResult` — the result row for an audited
            // write. The request context is rebuilt from `Inbound` (the
            // admission-time `attempt` row already hashed the raw map);
            // attribution reads live topology at result time, matching
            // the oracle's `s.state.Agent` inside `recordWriteAudit`.
            let audit_ctx = ctx.audit.as_ref().and_then(|log| {
                audit::is_audited(&msg.r#type).then(|| {
                    (
                        log.clone(),
                        audit::RequestContext::from_inbound(&msg, &ctx.client_id),
                    )
                })
            });
            // `d.fail`/`d.failErr` — every routed failure writes a journal row.
            for frame in &frames {
                if let Outbound::CommandResult(result) = frame {
                    if result.ok == Some(false) {
                        actions::record_failure(
                            &ctx,
                            result.action.as_deref().unwrap_or_default(),
                            result.pane_id.as_deref().unwrap_or_default(),
                            result.request_id.as_deref().unwrap_or(&rid),
                            result.error.as_deref().unwrap_or_default(),
                        );
                    }
                    if let Some((log, req)) = &audit_ctx {
                        let attribution = actions::audit_attribution(
                            &ctx.handle.topology.borrow(),
                            &req.pane_id,
                        );
                        if let Err(error) =
                            log.append(audit::result_record(req, result, attribution))
                        {
                            tracing::warn!(%error, "remote write audit append failed");
                        }
                    }
                }
            }
            frames
        });
        RouterReply::empty()
    }};
}

impl ActionRouter for HerdRouter {
    fn route(
        &mut self,
        ctx: &ClientContext<'_>,
        scope: &RequestScope,
        message: &Inbound,
    ) -> RouterReply {
        self.ensure_started(ctx);
        let request_id = message.request_id.clone();
        let action_id = scope.action_id.clone();

        match message.r#type.as_str() {
            // --- pane read/watch -----------------------------------------
            "read_pane" => self.route_read_pane(request_id, action_id, message),
            "watch_pane" => self.route_watch_pane(request_id, action_id, message),
            "unwatch_pane" => self.route_unwatch_pane(request_id, action_id, message),
            "pane_applied" => {
                self.watches.ack(&message.pane_id);
                RouterReply::empty()
            }
            "pane_resync" => {
                self.watches.resync(&message.pane_id);
                RouterReply::empty()
            }

            // --- input ----------------------------------------------------
            "send_text" => spawn_action!(
                self,
                request_id,
                action_id,
                message,
                actions::input::send_text
            ),
            "send_keys" => spawn_action!(
                self,
                request_id,
                action_id,
                message,
                actions::input::send_keys
            ),
            "send_input" => spawn_action!(
                self,
                request_id,
                action_id,
                message,
                actions::input::send_input
            ),
            "send_secret" => spawn_action!(
                self,
                request_id,
                action_id,
                message,
                actions::input::send_secret
            ),
            "submit_prompt" => spawn_action!(
                self,
                request_id,
                action_id,
                message,
                actions::input::submit_prompt
            ),
            "agent_stop" => spawn_action!(
                self,
                request_id,
                action_id,
                message,
                actions::input::agent_stop
            ),
            "respond" => spawn_action!(
                self,
                request_id,
                action_id,
                message,
                actions::questions::respond
            ),
            "answer_question" => spawn_action!(
                self,
                request_id,
                action_id,
                message,
                actions::questions::answer_question
            ),
            "clarify_question" => spawn_action!(
                self,
                request_id,
                action_id,
                message,
                actions::questions::clarify_question
            ),
            "navigate_question" => spawn_action!(
                self,
                request_id,
                action_id,
                message,
                actions::questions::navigate_question
            ),

            // --- uploads ---------------------------------------------------
            "upload_begin" => spawn_action!(
                self,
                request_id,
                action_id,
                message,
                actions::uploads::upload_begin
            ),
            "upload_chunk" => spawn_action!(
                self,
                request_id,
                action_id,
                message,
                actions::uploads::upload_chunk
            ),
            "upload_finish" => spawn_action!(
                self,
                request_id,
                action_id,
                message,
                actions::uploads::upload_finish
            ),
            "upload_cancel" => spawn_action!(
                self,
                request_id,
                action_id,
                message,
                actions::uploads::upload_cancel
            ),

            // --- activity --------------------------------------------------
            "get_activity" => spawn_action!(
                self,
                request_id,
                action_id,
                message,
                actions::activity::get_activity
            ),
            "clear_activities" => spawn_action!(
                self,
                request_id,
                action_id,
                message,
                actions::activity::clear_activities
            ),

            // --- push -------------------------------------------------------
            "push_policy_get" => spawn_action!(
                self,
                request_id,
                action_id,
                message,
                actions::push::policy_get
            ),
            "push_policy_set" => spawn_action!(
                self,
                request_id,
                action_id,
                message,
                actions::push::policy_set
            ),
            "push_subscribe" => spawn_action!(
                self,
                request_id,
                action_id,
                message,
                actions::push::subscribe
            ),
            "push_unsubscribe" => spawn_action!(
                self,
                request_id,
                action_id,
                message,
                actions::push::unsubscribe
            ),
            "push_test_device" => spawn_action!(
                self,
                request_id,
                action_id,
                message,
                actions::push::test_device
            ),
            "push_snooze" => {
                spawn_action!(self, request_id, action_id, message, actions::push::snooze)
            }
            "push_viewed_pane" => spawn_action!(
                self,
                request_id,
                action_id,
                message,
                actions::push::viewed_pane
            ),
            "push_open_ref" => spawn_action!(
                self,
                request_id,
                action_id,
                message,
                actions::push::open_ref
            ),

            // --- speech -----------------------------------------------------
            "speak_text" => spawn_action!(
                self,
                request_id,
                action_id,
                message,
                actions::speech::speak_text
            ),
            "cancel_speech" => spawn_action!(
                self,
                request_id,
                action_id,
                message,
                actions::speech::cancel_speech
            ),
            "speech_voices_list" => spawn_action!(
                self,
                request_id,
                action_id,
                message,
                actions::speech::voices_list
            ),
            "speech_voice_install" => spawn_action!(
                self,
                request_id,
                action_id,
                message,
                actions::speech::voice_install
            ),
            "speech_voice_remove" => spawn_action!(
                self,
                request_id,
                action_id,
                message,
                actions::speech::voice_remove
            ),

            // --- misc -------------------------------------------------------
            "check_update" => spawn_action!(
                self,
                request_id,
                action_id,
                message,
                actions::misc::check_update
            ),
            "install_update" => spawn_action!(
                self,
                request_id,
                action_id,
                message,
                actions::misc::install_update
            ),
            "get_conversation_history" => spawn_action!(
                self,
                request_id,
                action_id,
                message,
                actions::misc::conversation_history
            ),
            "list_slash_commands" => spawn_action!(
                self,
                request_id,
                action_id,
                message,
                actions::misc::slash_commands
            ),
            "inventory_status" => spawn_action!(
                self,
                request_id,
                action_id,
                message,
                actions::misc::inventory_status
            ),
            "copy_agent_response" => spawn_action!(
                self,
                request_id,
                action_id,
                message,
                actions::misc::copy_agent_response
            ),
            "register_app_origin" => spawn_action!(
                self,
                request_id,
                action_id,
                message,
                actions::misc::register_app_origin
            ),

            // --- workspace -------------------------------------------------
            "workspace_create" => {
                spawn_action!(
                    self,
                    request_id,
                    action_id,
                    message,
                    actions::workspace::workspace_create
                )
            }
            "workspace_rename" => {
                spawn_action!(
                    self,
                    request_id,
                    action_id,
                    message,
                    actions::workspace::workspace_rename
                )
            }
            "workspace_reorder" => {
                spawn_action!(
                    self,
                    request_id,
                    action_id,
                    message,
                    actions::workspace::workspace_reorder
                )
            }
            "workspace_close" => {
                spawn_action!(
                    self,
                    request_id,
                    action_id,
                    message,
                    actions::workspace::workspace_close
                )
            }

            // --- worktree --------------------------------------------------
            "worktree_list" => spawn_action!(
                self,
                request_id,
                action_id,
                message,
                actions::worktree::worktree_list
            ),
            "worktree_create" => {
                spawn_action!(
                    self,
                    request_id,
                    action_id,
                    message,
                    actions::worktree::worktree_create
                )
            }
            "worktree_open" => spawn_action!(
                self,
                request_id,
                action_id,
                message,
                actions::worktree::worktree_open
            ),
            "worktree_remove" => {
                spawn_action!(
                    self,
                    request_id,
                    action_id,
                    message,
                    actions::worktree::worktree_remove
                )
            }

            // --- tabs / agents ---------------------------------------------
            "agent_rename" => spawn_action!(
                self,
                request_id,
                action_id,
                message,
                actions::tabs::agent_rename
            ),
            "tab_reorder" => spawn_action!(
                self,
                request_id,
                action_id,
                message,
                actions::tabs::tab_reorder
            ),
            "acknowledge_pane" => {
                spawn_action!(
                    self,
                    request_id,
                    action_id,
                    message,
                    actions::tabs::acknowledge_pane
                )
            }
            "agent_start" => spawn_action!(
                self,
                request_id,
                action_id,
                message,
                actions::agents::agent_start
            ),
            "agent_clear" => spawn_action!(self, request_id, action_id, message, |c, r, a, m| {
                actions::agents::agent_clear(c, r, a, m, "agent_clear")
            }),
            "agent_restart" => spawn_action!(self, request_id, action_id, message, |c, r, a, m| {
                actions::agents::agent_clear(c, r, a, m, "agent_restart")
            }),
            "refresh_agents" => {
                let ctx = self.action_context();
                self.push_later(async move { actions::agents::refresh_agents(ctx).await });
                RouterReply::empty()
            }

            // --- pane-size leases ------------------------------------------
            "lease_pane_size" => {
                let ctx = self.action_context();
                let cancel = self.cancel.clone();
                let rid = request_id;
                let msg = message.clone();
                self.push_later(async move {
                    vec![actions::leases::lease_pane_size(ctx, &cancel, &rid, &msg).await]
                });
                RouterReply::empty()
            }
            "release_pane_size" => {
                let ctx = self.action_context();
                let rid = request_id;
                let msg = message.clone();
                self.push_later(async move {
                    vec![actions::leases::release_pane_size(ctx, &rid, &msg).await]
                });
                RouterReply::empty()
            }

            // --- local / inspection ----------------------------------------
            "list_directories" => {
                let rid = request_id;
                let msg = message.clone();
                self.push_later(async move { vec![actions::local::list_directories(&rid, &msg)] });
                RouterReply::empty()
            }
            "qr_code" => {
                let rid = request_id;
                let msg = message.clone();
                self.push_later(async move { vec![actions::local::qr_code(&rid, &msg)] });
                RouterReply::empty()
            }
            kind @ ("workspace_tree"
            | "workspace_file"
            | "workspace_git_status"
            | "workspace_git_diff") => {
                let ctx = self.action_context();
                let rid = request_id;
                let kind = kind.to_owned();
                let msg = message.clone();
                self.push_later(async move {
                    vec![actions::inspect::inspect(ctx, &rid, &kind, &msg).await]
                });
                RouterReply::empty()
            }

            // --- everything else: honest terminal state --------------------
            _ => RouterReply::send(vec![receipt(
                &request_id,
                &action_id,
                ActionReceiptPhase::DISPATCHED_UNKNOWN,
                None,
            )]),
        }
    }
}

impl HerdRouter {
    /// `read_pane` → async `pane.read` → `pane_content`/`pane_unchanged`
    /// push → receipt. `content_fingerprint` rides the raw seam: a string
    /// equal to the fresh read's fingerprint answers `pane_unchanged` (the
    /// computed fingerprint is canonical 16-lower-hex, so any malformed
    /// wire value is simply a miss — never an error). Like the oracle, an
    /// explicit read stops the pane's watch.
    fn route_read_pane(
        &mut self,
        request_id: String,
        action_id: String,
        message: &Inbound,
    ) -> RouterReply {
        let Some(pane_id) = non_empty(&message.pane_id) else {
            return invalid_request(&request_id, &action_id, "pane_id is required");
        };
        if self.sink().is_none() {
            return refused(&request_id, &action_id, "session_not_ready");
        }
        // The oracle's `read_pane` supersedes the watch (`stopPaneWatch`) —
        // otherwise the watch could race a frame past this read's answer.
        self.watches.stop(&pane_id);
        // `HandleReadPane` acknowledges the pane — the local half of that
        // is the ledger record at the pane's current `state_change_seq`.
        if let Some(agent) = self.handle.topology.borrow().pane_of(&pane_id) {
            self.shared.acks.record(&pane_id, agent.state_change_seq);
        }
        let client = self.handle.client.clone();
        let leases = self.shared.leases.clone();
        let lines = u32::try_from(message.lines)
            .ok()
            .filter(|l| *l > 0)
            .unwrap_or(DEFAULT_LINES);
        let fingerprint = message.content_fingerprint().map(str::to_owned);
        let target = message.target.clone();
        let rid = request_id.clone();
        let aid = action_id.clone();
        self.push_later(async move {
            // `applyPaneReadLease` — an active size lease marks the read
            // viewport-only (the pane was resized for this shape).
            let viewport_columns = leases.active_columns(&pane_id).await;
            let viewport_rows = leases.active_rows(&pane_id).await;
            let settling = leases
                .resized_within(&pane_id, actions::leases::RESIZE_SETTLE_WINDOW)
                .await;
            match client
                .pane_read(
                    &pane_id,
                    ReadSource::RecentUnwrapped,
                    lines,
                    ReadFormat::Text,
                )
                .await
            {
                Ok(read) => vec![
                    read_pane_frame(
                        &pane_id,
                        &read,
                        fingerprint.as_deref(),
                        target,
                        viewport_columns.is_some(),
                        viewport_rows,
                        settling,
                    ),
                    receipt(&rid, &aid, ActionReceiptPhase::CONFIRMED, None),
                ],
                Err(err) => vec![receipt_for_herdr_error(&rid, &aid, &err)],
            }
        });
        RouterReply::empty()
    }

    /// `watch_pane` — start the event-driven watch task. `interval_ms`
    /// becomes the watch's minimum read cadence (clamped), and a wire
    /// `content_fingerprint` matching the first read adopts the current
    /// frame instead of pushing a duplicate.
    fn route_watch_pane(
        &mut self,
        request_id: String,
        action_id: String,
        message: &Inbound,
    ) -> RouterReply {
        let Some(pane_id) = non_empty(&message.pane_id) else {
            return invalid_request(&request_id, &action_id, "pane_id is required");
        };
        let Some(sink) = self.sink() else {
            return refused(&request_id, &action_id, "session_not_ready");
        };
        if !self.watches.watching(&pane_id) {
            let spec = WatchSpec {
                lines: u32::try_from(message.lines)
                    .ok()
                    .filter(|l| *l > 0)
                    .unwrap_or(DEFAULT_LINES),
                interval: watch_interval(message.interval_ms()),
                known_fingerprint: message.content_fingerprint().map(str::to_owned),
            };
            self.watches.start(
                pane_id,
                spec,
                self.handle.client.clone(),
                Arc::new(sink),
                self.handle.invalidations.clone(),
                self.cancel.clone(),
            );
        }
        RouterReply::send(vec![receipt(
            &request_id,
            &action_id,
            ActionReceiptPhase::CONFIRMED,
            None,
        )])
    }

    /// `unwatch_pane` — stop the task; unknown pane is a no-op receipt.
    fn route_unwatch_pane(
        &mut self,
        request_id: String,
        action_id: String,
        message: &Inbound,
    ) -> RouterReply {
        self.watches.stop(&message.pane_id);
        RouterReply::send(vec![receipt(
            &request_id,
            &action_id,
            ActionReceiptPhase::CONFIRMED,
            None,
        )])
    }
}

fn non_empty(s: &str) -> Option<String> {
    (!s.is_empty()).then(|| s.to_owned())
}

/// The `read_pane` result frame (`unchangedPaneResponse`): `pane_unchanged`
/// when the wire `content_fingerprint` equals the fresh read's,
/// `pane_content` otherwise — including absent/malformed wire values, since
/// equality against the canonical computed fingerprint is the entire
/// validation. `target` echoes the request's when present (the oracle
/// assigns `resp["target"]` only then; `pane_unchanged` emits the key
/// either way — `null` when absent).
fn read_pane_frame(
    pane_id: &str,
    read: &PaneReadResult,
    fingerprint: Option<&str>,
    target: Option<lerdr_core::protocol::TargetRef>,
    viewport_only: bool,
    viewport_rows: Option<i64>,
    resize_settling: bool,
) -> Outbound {
    let computed = crate::content_fingerprint(&read.text);
    if fingerprint == Some(computed.as_str()) {
        return crate::watches::pane_unchanged(pane_id, &computed, target);
    }
    Outbound::PaneContent(Box::new(PaneContent {
        r#type: "pane_content".to_owned(),
        pane_id: Some(pane_id.to_owned()),
        content: Some(read.text.clone()),
        content_fingerprint: Some(computed),
        format: Some("text".to_owned()),
        truncated: Some(read.truncated),
        target: target.map(lerdr_core::json::MaybeNull::Value),
        viewport_only: viewport_only.then_some(true),
        viewport_rows: if viewport_only { viewport_rows } else { None },
        // The agent re-renders after a lease resize and can push redrawn
        // rows into scrollback — frames read inside the settle window
        // must not commit as history.
        resize_settling: (viewport_only && resize_settling).then_some(true),
        ..PaneContent::default()
    }))
}

fn receipt(
    request_id: &str,
    action_id: &str,
    phase: &'static str,
    error: Option<ApiError>,
) -> Outbound {
    Outbound::ActionReceipt(action_receipt_response(
        request_id,
        ActionReceipt {
            action_id: action_id.to_owned(),
            phase: ActionReceiptPhase::from(phase),
            error,
        },
    ))
}

/// Dispatch-boundary taxonomy → receipt (doc 08 rule 4).
fn receipt_for_herdr_error(request_id: &str, action_id: &str, err: &HerdrError) -> Outbound {
    let (phase, error) = match err.phase() {
        DispatchPhase::NotStarted => (
            ActionReceiptPhase::FAILED_BEFORE_DISPATCH,
            Some(api_error("herdr_unreachable", err)),
        ),
        DispatchPhase::DispatchedUnknown => (
            ActionReceiptPhase::DISPATCHED_UNKNOWN,
            Some(api_error("dispatch_outcome_unknown", err)),
        ),
        DispatchPhase::Refused => (
            ActionReceiptPhase::CONFIRMED,
            Some(ApiError::new(
                err.refusal_code().unwrap_or("refused"),
                refusal_args(err),
            )),
        ),
    };
    receipt(request_id, action_id, phase, error)
}

fn api_error(code: &str, err: &HerdrError) -> ApiError {
    let mut args = BTreeMap::new();
    args.insert(
        "detail".to_owned(),
        serde_json::Value::String(err.to_string()),
    );
    ApiError::new(code, args)
}

fn refusal_args(err: &HerdrError) -> BTreeMap<String, serde_json::Value> {
    let mut args = BTreeMap::new();
    if let HerdrError::Refused { message, .. } = err {
        args.insert(
            "message".to_owned(),
            serde_json::Value::String(message.clone()),
        );
    }
    args
}

fn invalid_request(request_id: &str, action_id: &str, detail: &str) -> RouterReply {
    let mut args = BTreeMap::new();
    args.insert(
        "detail".to_owned(),
        serde_json::Value::String(detail.to_owned()),
    );
    RouterReply::send(vec![
        Outbound::Error(error_response(
            request_id,
            ApiError::new(error_codes::INVALID_REQUEST, args),
        )),
        receipt(
            request_id,
            action_id,
            ActionReceiptPhase::FAILED_BEFORE_DISPATCH,
            Some(ApiError::new(error_codes::INVALID_REQUEST, BTreeMap::new())),
        ),
    ])
}

fn refused(request_id: &str, action_id: &str, code: &str) -> RouterReply {
    RouterReply::send(vec![receipt(
        request_id,
        action_id,
        ActionReceiptPhase::CONFIRMED,
        Some(ApiError::new(code, BTreeMap::new())),
    )])
}

#[cfg(test)]
mod tests {
    use super::*;
    use lerdr_core::json::MaybeNull;
    use lerdr_core::protocol::TargetRef;
    use std::time::Duration;

    fn pane_read(text: &str) -> PaneReadResult {
        PaneReadResult {
            text: text.to_owned(),
            ..PaneReadResult::default()
        }
    }

    fn inbound(map: serde_json::Map<String, serde_json::Value>) -> Inbound {
        Inbound::decode_map(&map).expect("decode")
    }

    #[test]
    fn receipt_phases() {
        let r = |phase: &'static str| match receipt("r1", "a1", phase, None) {
            Outbound::ActionReceipt(m) => m.receipt.unwrap().phase.as_str().to_owned(),
            _ => panic!("expected receipt"),
        };
        assert_eq!(r(ActionReceiptPhase::CONFIRMED), "confirmed");
        assert_eq!(
            r(ActionReceiptPhase::DISPATCHED_UNKNOWN),
            "dispatched_unknown"
        );
    }

    #[test]
    fn herdr_error_phase_mapping() {
        let io = || std::io::Error::new(std::io::ErrorKind::BrokenPipe, "gone");
        match receipt_for_herdr_error("r", "a", &HerdrError::NotStarted(Arc::new(io()))) {
            Outbound::ActionReceipt(m) => {
                assert_eq!(m.receipt.unwrap().phase.as_str(), "failed_before_dispatch");
            }
            _ => panic!(),
        }
        let refused_err = HerdrError::Refused {
            code: "agent_pane_busy".into(),
            message: "busy".into(),
        };
        match receipt_for_herdr_error("r", "a", &refused_err) {
            Outbound::ActionReceipt(m) => {
                let r = m.receipt.unwrap();
                assert_eq!(r.phase.as_str(), "confirmed");
                assert_eq!(r.error.unwrap().code, "agent_pane_busy");
            }
            _ => panic!(),
        }
    }

    // -- read_pane fingerprint path ---------------------------------------

    /// `unchangedPaneResponse`: wire `content_fingerprint` == fresh read's
    /// → `pane_unchanged` echoing the request's `target` (`null` absent).
    #[test]
    fn read_pane_fingerprint_hit_is_pane_unchanged() {
        let frame = read_pane_frame(
            "wE:pE",
            &pane_read("hello world\n"),
            Some("a948904f2f0f479b"),
            None,
            false,
            None,
            false,
        );
        match frame {
            Outbound::PaneUnchanged(m) => {
                assert_eq!(m.pane_id.as_deref(), Some("wE:pE"));
                assert_eq!(m.content_fingerprint.as_deref(), Some("a948904f2f0f479b"));
                assert!(matches!(m.target, Some(MaybeNull::Null)));
            }
            other => panic!("expected pane_unchanged, got {other:?}"),
        }
    }

    /// Stale fingerprint → full `pane_content` carrying the fresh one.
    #[test]
    fn read_pane_fingerprint_miss_is_pane_content() {
        let frame = read_pane_frame(
            "wE:pE",
            &pane_read("hello world\n"),
            Some("0000000000000000"),
            None,
            false,
            None,
            false,
        );
        match frame {
            Outbound::PaneContent(m) => {
                assert_eq!(m.content.as_deref(), Some("hello world\n"));
                assert_eq!(m.content_fingerprint.as_deref(), Some("a948904f2f0f479b"));
            }
            other => panic!("expected pane_content, got {other:?}"),
        }
        // Absent entirely — same full-content answer.
        assert!(matches!(
            read_pane_frame(
                "wE:pE",
                &pane_read("hello world\n"),
                None,
                None,
                false,
                None,
                false
            ),
            Outbound::PaneContent(_)
        ));
    }

    /// Malformed wire fingerprints are a miss, never an error: wrong
    /// length, non-hex, wrong case, empty — the canonical computed value
    /// can never equal any of them.
    #[test]
    fn read_pane_malformed_fingerprint_is_a_miss() {
        let read = pane_read("hello world\n");
        for bad in [
            "",
            "a948904f2f0f479",   // 15 chars
            "a948904f2f0f479bb", // 17 chars
            "A948904F2F0F479B",  // uppercase hex
            "zzzzzzzzzzzzzzzz",  // non-hex
        ] {
            assert!(
                matches!(
                    read_pane_frame("wE:pE", &read, Some(bad), None, false, None, false),
                    Outbound::PaneContent(_)
                ),
                "{bad:?} should be a fingerprint miss"
            );
        }
    }

    /// Both answer shapes echo the request `target` when present (the
    /// oracle assigns `resp["target"]` before the unchanged check).
    #[test]
    fn read_pane_frames_echo_request_target() {
        let target = || {
            Some(TargetRef {
                pane_id: "wE:pE".to_owned(),
                ..TargetRef::default()
            })
        };
        let read = pane_read("hello world\n");
        match read_pane_frame(
            "wE:pE",
            &read,
            Some("a948904f2f0f479b"),
            target(),
            false,
            None,
            false,
        ) {
            Outbound::PaneUnchanged(m) => {
                assert!(matches!(m.target, Some(MaybeNull::Value(_))))
            }
            other => panic!("expected pane_unchanged, got {other:?}"),
        }
        match read_pane_frame("wE:pE", &read, None, target(), false, None, false) {
            Outbound::PaneContent(m) => {
                assert!(matches!(m.target, Some(MaybeNull::Value(_))))
            }
            other => panic!("expected pane_content, got {other:?}"),
        }
    }

    // -- watch_pane raw fields --------------------------------------------

    /// `interval_ms` rides the raw seam into the clamped cadence;
    /// `content_fingerprint` is the watch's initial-known fingerprint.
    #[test]
    fn watch_pane_wire_fields_reach_the_spec() {
        let msg = inbound(
            serde_json::json!({"type":"watch_pane","pane_id":"wE:pE","interval_ms":500})
                .as_object()
                .unwrap()
                .clone(),
        );
        assert_eq!(
            watch_interval(msg.interval_ms()),
            Duration::from_millis(500)
        );
        assert_eq!(msg.content_fingerprint(), None);

        let msg = inbound(
            serde_json::json!({"type":"watch_pane","pane_id":"wE:pE","interval_ms":10,"content_fingerprint":"a948904f2f0f479b"})
                .as_object()
                .unwrap()
                .clone(),
        );
        assert_eq!(
            watch_interval(msg.interval_ms()),
            crate::watches::MIN_WATCH_INTERVAL
        );
        assert_eq!(msg.content_fingerprint(), Some("a948904f2f0f479b"));

        let msg = inbound(
            serde_json::json!({"type":"watch_pane","pane_id":"wE:pE","interval_ms":120000})
                .as_object()
                .unwrap()
                .clone(),
        );
        assert_eq!(
            watch_interval(msg.interval_ms()),
            crate::watches::MAX_WATCH_INTERVAL
        );
    }

    // -- write-audit result rows -----------------------------------------

    /// `sendAuditedCommandResult` at the spawn seam: a routed audited
    /// action's `command_result` appends a `result` row to the shared log.
    /// The `attempt` row is the session's (admission), so only `result`
    /// lands here — with no sink wired, frames drop but the audit append
    /// already ran.
    #[tokio::test]
    async fn audited_spawned_action_appends_result_row() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = Arc::new(audit::AuditLog::open(dir.path()).expect("audit opens"));
        let handle = crate::TopologyActor::spawn(
            lerdr_herdr::Client::unix(std::path::PathBuf::from("/nonexistent-herdr.sock")),
            CancellationToken::new(),
        );
        let factory = HerdRouterFactory::new(
            handle,
            Arc::new(|_: &str| None),
            CancellationToken::new(),
            dir.path().join("runtime"),
            Some(log.clone()),
        );
        let mut router = factory.into_factory()();
        let identity = lerdr_relay::auth::AuthenticatedIdentity {
            device_id: "dev-1".to_owned(),
            credential_id: "cred-1".to_owned(),
            role: lerdr_relay::auth::Role::Controller,
            locale: "en".to_owned(),
            credential_version: 1,
        };
        let ctx = ClientContext {
            client_id: "conn-9",
            identity: &identity,
            transport: "ws",
        };
        let message = inbound(
            serde_json::json!({"type":"send_secret","protocol":3,"request_id":"r9","action_id":"a9","target":{"pane_id":"w:t:p"},"text":"x"})
                .as_object()
                .unwrap()
                .clone(),
        );
        let scope = RequestScope::for_message(&message).expect("send_secret scopes");
        router.route(&ctx, &scope, &message);

        // The audit append runs inside the spawned task — poll briefly.
        let path = dir.path().join("audit").join("remote-writes.jsonl");
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let row = loop {
            let found = std::fs::read_to_string(&path)
                .unwrap_or_default()
                .lines()
                .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
                .find(|v| v["stage"] == "result");
            if let Some(row) = found {
                break row;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "result row never landed"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        assert_eq!(row["action"], "send_secret");
        assert_eq!(row["request_id"], "r9");
        assert_eq!(row["pane_id"], "w:t:p");
        assert_eq!(row["client_id"], "connection:conn-9");
        assert_eq!(row["connection_id"], "conn-9");
        assert_eq!(row["ok"], false, "no Herdr → failed result");
        assert!(row.get("details").is_none(), "result rows carry no details");
    }

    /// A non-audited routed action emits no rows even when it fails.
    #[tokio::test]
    async fn non_audited_action_appends_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = Arc::new(audit::AuditLog::open(dir.path()).expect("audit opens"));
        let handle = crate::TopologyActor::spawn(
            lerdr_herdr::Client::unix(std::path::PathBuf::from("/nonexistent-herdr.sock")),
            CancellationToken::new(),
        );
        let factory = HerdRouterFactory::new(
            handle,
            Arc::new(|_: &str| None),
            CancellationToken::new(),
            dir.path().join("runtime"),
            Some(log.clone()),
        );
        let mut router = factory.into_factory()();
        let identity = lerdr_relay::auth::AuthenticatedIdentity {
            device_id: "dev-1".to_owned(),
            credential_id: "cred-1".to_owned(),
            role: lerdr_relay::auth::Role::Controller,
            locale: "en".to_owned(),
            credential_version: 1,
        };
        let ctx = ClientContext {
            client_id: "conn-9",
            identity: &identity,
            transport: "ws",
        };
        // `get_settings` is a real routed read — never audited.
        let message = inbound(
            serde_json::json!({"type":"push_policy_get","protocol":3,"request_id":"r10","action_id":"a10"})
                .as_object()
                .unwrap()
                .clone(),
        );
        let scope = RequestScope::for_message(&message).expect("push_policy_get scopes");
        router.route(&ctx, &scope, &message);

        tokio::time::sleep(Duration::from_millis(150)).await;
        let path = dir.path().join("audit").join("remote-writes.jsonl");
        let contents = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(contents.is_empty(), "non-audited action wrote: {contents}");
    }
}
