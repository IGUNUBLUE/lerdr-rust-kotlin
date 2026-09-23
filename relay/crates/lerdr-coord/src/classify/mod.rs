//! Semantic classification — the `internal/question` + `internal/noecho`
//! port: pure, synchronous pane-content analysis.
//!
//! The modules here own everything the relay can decide from pane text and
//! the agent kind alone — no Herdr calls, no coordinator state:
//!
//! - [`text`] — ANSI/edge cleaning shared by the matchers and parsers.
//! - [`matchers`] — the hand-rolled ports of every `regexp.MustCompile`
//!   in `parser.go`/`attention.go`.
//! - [`model`] — the wire-shaped `question.Interaction` model, inbound
//!   payloads, interaction identity, and custom-answer filling.
//! - [`parse`] — `question.Parse` + `LayoutHint`: pane text → structured
//!   interaction for the five supported agent families.
//! - [`attention`] — `question.Classify`: pane text + agent kind →
//!   attention kind, prompt/command/options, approval fingerprint, and
//!   the structured interaction.
//! - [`input`] — `question.PlanInput`: interaction + payload → the
//!   per-agent keyboard contract.
//! - [`noecho`] — `internal/noecho.Match`: secret-prompt detection for
//!   `no_echo`/`no_echo_prompt`.
//! - [`store`] — the per-pane attention ledger (the coordinator `State`
//!   halves: blocked event ids, committed classifications, revision
//!   counters, unseen/ack bookkeeping) plus the lifecycle hooks
//!   `Topology::accept` drives.
//! - [`projector`] — the async transition pipeline (`onTransition` +
//!   `enrichBlockedTransition` + `CommitAttentionClassification` +
//!   `broadcastBlockedAttention` + `publishAgentPush`).

pub(crate) mod attention;
pub(crate) mod input;
pub(crate) mod matchers;
pub(crate) mod model;
pub(crate) mod noecho;
pub(crate) mod parse;
pub(crate) mod projector;
pub(crate) mod store;
pub(crate) mod text;

pub(crate) use attention::*;
pub(crate) use input::*;
pub(crate) use model::*;
pub(crate) use parse::*;
pub(crate) use store::*;
