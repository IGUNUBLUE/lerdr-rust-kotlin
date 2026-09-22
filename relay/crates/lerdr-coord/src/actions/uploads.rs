//! Attachment uploads — the `internal/upload` + `app/uploads.go` port.
//!
//! `upload_begin` stages a session, `upload_chunk` streams base64 payloads
//! into it, `upload_finish` verifies digests and publishes the attachment
//! index, `upload_cancel` aborts. Answers are `upload_*_result` frames
//! (`{type, request_id, result}` / `{type, request_id, error}`), not
//! `command_result` — the oracle's shape. `send_text`/`submit_prompt`
//! expand `Attachment: <ref>` lines through the finished index before
//! dispatch.
//!
//! Stubs answer `dispatched_unknown`; [`Uploads`] carries the staging
//! directory the relay runtime dir provides.

use std::path::PathBuf;
use std::sync::Arc;

use lerdr_core::protocol::{Inbound, Outbound};

use super::{unknown, ActionContext};

/// Shared upload state — one per relay (the oracle's `upload.Manager`).
/// Owns staged sessions, disk persistence, and the finished-attachment
/// index `Resolve` consults.
#[derive(Clone)]
pub(crate) struct Uploads {
    #[allow(dead_code)]
    inner: Arc<UploadsInner>,
}

#[allow(dead_code)]
struct UploadsInner {
    /// `<runtime-dir>/uploads` — staging root for in-flight sessions and
    /// the published attachment records.
    dir: PathBuf,
}

impl Uploads {
    pub(crate) fn new(dir: PathBuf) -> Self {
        Self {
            inner: Arc::new(UploadsInner { dir }),
        }
    }
}

/// `handleUploadBegin`.
pub(crate) async fn upload_begin(
    _ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    _message: &Inbound,
) -> Vec<Outbound> {
    unknown(request_id, action_id)
}

/// `handleUploadChunk`.
pub(crate) async fn upload_chunk(
    _ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    _message: &Inbound,
) -> Vec<Outbound> {
    unknown(request_id, action_id)
}

/// `handleUploadFinish`.
pub(crate) async fn upload_finish(
    _ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    _message: &Inbound,
) -> Vec<Outbound> {
    unknown(request_id, action_id)
}

/// `handleUploadCancel`.
pub(crate) async fn upload_cancel(
    _ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    _message: &Inbound,
) -> Vec<Outbound> {
    unknown(request_id, action_id)
}
