//! Dispatch-boundary error taxonomy (docs/08 — "the semantic heart of the
//! client"). Every failure to complete a Herdr request is classified by where
//! it failed relative to the socket write:
//!
//! * [`HerdrError::NotStarted`] — no request bytes reached the peer. The
//!   operation provably did not run; retrying is safe.
//! * [`HerdrError::Refused`] — Herdr answered with a structured error. The
//!   operation did NOT apply; surface the refusal code.
//! * [`HerdrError::DispatchedUnknown`] — request bytes were written but no
//!   usable response came back. The operation **may have applied**; only
//!   idempotent retries are safe.

use std::fmt;
use std::io;
use std::sync::Arc;

/// Where a failed request stopped relative to the dispatch boundary.
///
/// Maps onto `ActionReceipt.phase` in lerdr-coord (doc 08 rule 4):
/// `NotStarted` → `failed_before_dispatch`, `DispatchedUnknown` →
/// `dispatched_unknown`, `Refused` → confirmed refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DispatchPhase {
    /// No request bytes reached the peer.
    NotStarted,
    /// Herdr returned a structured refusal.
    Refused,
    /// Bytes were written; the outcome is unknowable.
    DispatchedUnknown,
}

impl DispatchPhase {
    /// `true` when retrying the request cannot double-apply it.
    pub fn is_safe_to_retry(self) -> bool {
        matches!(self, DispatchPhase::NotStarted)
    }

    /// `true` when the request may have been applied by the peer.
    pub fn may_have_applied(self) -> bool {
        matches!(self, DispatchPhase::DispatchedUnknown)
    }
}

/// Herdr refusal codes that describe a transient condition: the same request
/// may succeed later without any change to the request itself. Mirrors the
/// Go client's `transientRefusalCodes`.
pub const TRANSIENT_REFUSAL_CODES: &[&str] = &["server_not_running", "agent_pane_busy"];

/// Refusal codes the Go client treats as known-structural rejections. Any
/// `code` inside a `Refused` is already definitive (the op did not apply);
/// this set exists for callers that want to distinguish recognized codes.
pub const KNOWN_REFUSAL_CODES: &[&str] = &[
    "server_not_running",
    "agent_pane_busy",
    "protocol_mismatch",
    "invalid_request",
    "workspace_not_found",
    "worktree_not_found",
    "not_git_worktree",
    "linked_worktree_source",
    "workspace_group_close_required",
    "workspace_group_changed",
    "workspace_group_primary_required",
    "workspace_group_consent_invalid",
    "workspace_group_validation_unavailable",
    "workspace_move_block_failed",
    "unknown_method",
    "method_not_found",
    "unsupported_method",
];

/// A failure to complete a Herdr socket request, classified at the dispatch
/// boundary.
///
/// `NotStarted` wraps the underlying `io::Error` in an `Arc` so the error can
/// be cloned — singleflight followers each receive a copy of the leader's
/// failure. `DispatchedUnknown` carries the post-write failure cause as a
/// nested leaf error (typically `NotStarted` wrapping the io/decode error that
/// prevented reading a response).
#[derive(Debug, Clone)]
pub enum HerdrError {
    /// No request bytes reached the peer. Safe to retry.
    NotStarted(Arc<io::Error>),
    /// Herdr returned `{"id":…,"error":{code,message}}`. The operation was
    /// rejected before applying — never silently retry a mutation on this.
    Refused { code: String, message: String },
    /// Request bytes were written but no usable response arrived. The
    /// operation may have applied.
    DispatchedUnknown(Box<HerdrError>),
}

impl HerdrError {
    /// Build a `NotStarted` from an `io::Error`.
    pub fn not_started(err: io::Error) -> Self {
        HerdrError::NotStarted(Arc::new(err))
    }

    /// Build a `NotStarted` from anything stringifiable (encode failures,
    /// semaphore shutdown, elapsed timeouts).
    pub fn not_started_msg(msg: impl Into<String>) -> Self {
        HerdrError::not_started(io::Error::other(msg.into()))
    }

    /// Build a `Refused` from a structured Herdr error body.
    pub fn refused(code: impl Into<String>, message: impl Into<String>) -> Self {
        HerdrError::Refused {
            code: code.into(),
            message: message.into(),
        }
    }

    /// Wrap a post-dispatch failure cause. The inner error describes *why* the
    /// outcome could not be read (transport read failure, malformed response,
    /// id mismatch); the outer variant fixes the boundary: bytes went out.
    pub fn dispatched_unknown(cause: HerdrError) -> Self {
        HerdrError::DispatchedUnknown(Box::new(cause))
    }

    /// Convenience for `DispatchedUnknown(NotStarted(io_err))`.
    pub fn dispatched_io(err: io::Error) -> Self {
        HerdrError::dispatched_unknown(HerdrError::not_started(err))
    }

    /// Convenience for a post-dispatch protocol violation (id mismatch,
    /// missing result, unexpected result type, malformed JSON).
    pub fn dispatched_msg(msg: impl Into<String>) -> Self {
        HerdrError::dispatched_io(io::Error::new(io::ErrorKind::InvalidData, msg.into()))
    }

    /// The dispatch phase of this failure.
    pub fn phase(&self) -> DispatchPhase {
        match self {
            HerdrError::NotStarted(_) => DispatchPhase::NotStarted,
            HerdrError::Refused { .. } => DispatchPhase::Refused,
            HerdrError::DispatchedUnknown(_) => DispatchPhase::DispatchedUnknown,
        }
    }

    /// `true` when retrying cannot double-apply the operation.
    pub fn is_safe_to_retry(&self) -> bool {
        self.phase().is_safe_to_retry()
    }

    /// `true` when the operation may have been applied.
    pub fn may_have_applied(&self) -> bool {
        self.phase().may_have_applied()
    }

    /// The refusal code, when Herdr answered with a structured error.
    pub fn refusal_code(&self) -> Option<&str> {
        match self {
            HerdrError::Refused { code, .. } => Some(code.as_str()),
            _ => None,
        }
    }

    /// `true` for `Refused` whose code names a transient condition
    /// (`server_not_running`, `agent_pane_busy`).
    pub fn is_transient_refusal(&self) -> bool {
        self.refusal_code()
            .is_some_and(|code| TRANSIENT_REFUSAL_CODES.contains(&code))
    }

    /// The underlying `io::Error`, when one is reachable. For
    /// `DispatchedUnknown` this returns the post-dispatch cause.
    pub fn io_error(&self) -> Option<&io::Error> {
        match self {
            HerdrError::NotStarted(err) => Some(err.as_ref()),
            HerdrError::DispatchedUnknown(cause) => cause.io_error(),
            HerdrError::Refused { .. } => None,
        }
    }
}

impl fmt::Display for HerdrError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HerdrError::NotStarted(err) => write!(f, "herdr request not dispatched: {err}"),
            HerdrError::Refused { code, message } => {
                if code.is_empty() {
                    write!(f, "herdr refused the request: {message}")
                } else if message.is_empty() {
                    write!(f, "herdr refused the request: {code}")
                } else {
                    write!(f, "herdr refused the request ({code}): {message}")
                }
            }
            HerdrError::DispatchedUnknown(cause) => {
                write!(f, "herdr request dispatched but outcome unknown: {cause}")
            }
        }
    }
}

impl std::error::Error for HerdrError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            HerdrError::NotStarted(err) => Some(err.as_ref()),
            HerdrError::Refused { .. } => None,
            HerdrError::DispatchedUnknown(cause) => Some(cause.as_ref()),
        }
    }
}

impl From<io::Error> for HerdrError {
    /// Bare `io::Error`s are classified conservatively as pre-dispatch only at
    /// construction sites that can prove it (dial, first write). Call sites
    /// after the write use [`HerdrError::dispatched_io`] instead.
    fn from(err: io::Error) -> Self {
        HerdrError::not_started(err)
    }
}

/// Why an `events.subscribe` handshake failed. Kept distinct from
/// `HerdrError` because the pre-dispatch detail drives the
/// `workspace.reordered` fallback: an `invalid_request` refusal naming the
/// `workspace.reordered` variant means "retry without it", not "failed".
#[derive(Debug, Clone)]
pub struct SubscribeError {
    /// The refusal code (`invalid_request`, `unknown_subscription`, …) or a
    /// transport detail when no structured refusal arrived.
    pub code: Option<String>,
    /// Human-readable detail.
    pub message: String,
    /// `true` when the failure happened before the request could have been
    /// dispatched (connect/write failure, or an `id:"" + invalid_request`
    /// pre-dispatch refusal).
    pub pre_dispatch: bool,
    /// The underlying dispatch error when the handshake failed on the wire.
    pub source: Option<HerdrError>,
}

impl SubscribeError {
    pub(crate) fn transport(err: HerdrError) -> Self {
        let pre_dispatch = err.is_safe_to_retry();
        SubscribeError {
            code: err.refusal_code().map(str::to_owned),
            message: err.to_string(),
            pre_dispatch,
            source: Some(err),
        }
    }

    pub(crate) fn refused(code: String, message: String, pre_dispatch: bool) -> Self {
        SubscribeError {
            code: Some(code),
            message,
            pre_dispatch,
            source: None,
        }
    }

    /// `true` when the refusal names a subscription entry the server does not
    /// know — the caller should retry with a reduced subscription set rather
    /// than treat the request as failed. Mirrors `isUnsupportedSubscription`.
    pub fn is_unsupported_subscription(&self) -> bool {
        match self.code.as_deref() {
            Some(
                "unknown_event"
                | "unsupported_event"
                | "unknown_subscription"
                | "unsupported_subscription"
                | "invalid_subscription",
            ) => true,
            Some(_) | None => self.pre_dispatch,
        }
    }

    /// `true` when this refusal specifically rejects the `workspace.reordered`
    /// subscription entry (Herdr builds without move_block reject the whole
    /// `events.subscribe` when the name is present).
    pub fn is_workspace_reordered_rejected(&self) -> bool {
        self.pre_dispatch
            && self.code.as_deref() == Some("invalid_request")
            && self
                .message
                .contains("unknown variant `workspace.reordered`")
    }
}

impl fmt::Display for SubscribeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.code {
            Some(code) => write!(f, "herdr events subscription {code}: {}", self.message),
            None => write!(f, "herdr events subscription failed: {}", self.message),
        }
    }
}

impl std::error::Error for SubscribeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_ref()
            .map(|e| e as &(dyn std::error::Error + 'static))
    }
}

/// Terminal state of an [`crate::events::EventStream`]. Every variant means
/// the stream is done and the documented recovery loop applies: resubscribe →
/// `subscription_started` → `session.snapshot` → treat later events as
/// invalidation signals.
#[derive(Debug, Clone, thiserror::Error)]
pub enum EventStreamError {
    /// The server sent `error.code: "events_lost"` on the subscription
    /// connection — the retained event history overran while this consumer
    /// was subscribed. Cached state is stale; resync.
    #[error("herdr signalled events_lost: {0}")]
    EventsLost(String),
    /// The server sent a terminal error envelope that is not `events_lost`.
    #[error("herdr terminated the event stream ({code}): {message}")]
    Terminated { code: String, message: String },
    /// The connection closed or failed without a terminal error envelope.
    /// Per the contract a silent close is indistinguishable from lost events;
    /// resync.
    #[error("herdr event stream closed: {0}")]
    Closed(Arc<io::Error>),
    /// An event line could not be decoded.
    #[error("malformed herdr event: {0}")]
    Decode(String),
    /// The bounded in-memory queue filled faster than the consumer drained
    /// it. Rather than silently dropping events, resync.
    #[error("herdr event queue overflowed; resync required")]
    Lagged,
}

impl EventStreamError {
    /// `true` when the terminal state indicates retained history was lost
    /// (`events_lost` or queue overflow) versus a plain connection drop. Both
    /// require resync; the distinction is telemetry only.
    pub fn history_lost(&self) -> bool {
        matches!(
            self,
            EventStreamError::EventsLost(_) | EventStreamError::Lagged
        )
    }
}

/// Failure of the bootstrap step (`subscribe` → `subscription_started` →
/// `session.snapshot`). `Subscribe` carries the handshake outcome (including
/// the `workspace.reordered` rejection detail); `Snapshot` carries the
/// dispatch boundary of the snapshot request.
#[derive(Debug, thiserror::Error)]
pub enum BootstrapError {
    #[error("{0}")]
    Subscribe(#[from] SubscribeError),
    #[error("session.snapshot: {0}")]
    Snapshot(#[from] HerdrError),
}
