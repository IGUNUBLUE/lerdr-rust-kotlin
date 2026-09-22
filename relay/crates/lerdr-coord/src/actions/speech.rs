//! Speech/TTS actions — the oracle's speech engine port.
//!
//! `speak_text` dispatches text to the host speech engine keyed by
//! `speech_request_id` (client-generated, used by `cancel_speech`);
//! `speech_voices_list` answers the installed voice catalog;
//! `speech_voice_install`/`speech_voice_remove` manage voice downloads.
//! No engine → `speak_text` fails with "No speech engine is installed on
//! this computer" (server.go:2361).
//!
//! Stubs answer `dispatched_unknown`; [`Speech`] carries the engine
//! handle + in-flight request set shared across sessions.

use std::sync::{Arc, Mutex};

use lerdr_core::protocol::{Inbound, Outbound};

use super::{unknown, ActionContext};

/// Shared speech state — one per relay (the oracle's speech engine
/// handle + in-flight `speech_request_id` set for cancellation).
#[derive(Clone, Default)]
pub(crate) struct Speech {
    #[allow(dead_code)]
    inner: Arc<Mutex<()>>,
}

/// `speak_text` — `{request_id, speech_request_id, text, language}`.
pub(crate) async fn speak_text(
    _ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    _message: &Inbound,
) -> Vec<Outbound> {
    unknown(request_id, action_id)
}

/// `cancel_speech` — `{speech_request_id}`; cancels by client+request id.
pub(crate) async fn cancel_speech(
    _ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    _message: &Inbound,
) -> Vec<Outbound> {
    unknown(request_id, action_id)
}

/// `speech_voices_list` — `command_result` with the voice payload.
pub(crate) async fn voices_list(
    _ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    _message: &Inbound,
) -> Vec<Outbound> {
    unknown(request_id, action_id)
}

/// `speech_voice_install` — `{language}`; installs a voice.
pub(crate) async fn voice_install(
    _ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    _message: &Inbound,
) -> Vec<Outbound> {
    unknown(request_id, action_id)
}

/// `speech_voice_remove` — `{language}`; removes a voice.
pub(crate) async fn voice_remove(
    _ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    _message: &Inbound,
) -> Vec<Outbound> {
    unknown(request_id, action_id)
}
