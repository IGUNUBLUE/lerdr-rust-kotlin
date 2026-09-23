//! Per-client outbound send buffer — a synchronous port of
//! `internal/transport/sendbuffer.go`.
//!
//! Capacity is bounded in items AND bytes (serialized plaintext JSON). Push
//! overflow is **rejected**, never evicted — the queued messages survive
//! untouched. Replaceable message types (the 8-type [`REPLACEABLE_TYPES`]
//! set, decided in `encodeMessage` at `internal/transport/ws.go`) coalesce
//! against an identical-type tail only; a coalesce that would exceed the
//! byte budget is rejected. Draining is FIFO.
//!
//! Go's `sync.Cond`-blocking `Pop` becomes [`SendBuffer::pop`], which
//! returns `None` on an empty or closed-and-drained buffer — the async wake
//! loop is the transport's concern, not this model's.

use std::collections::VecDeque;

/// `clientOutboundMaxItems`.
pub const DEFAULT_MAX_ITEMS: usize = 64;
/// `MaxOutboundMessageBytes` — the largest plaintext message the buffer
/// admits; producers must keep one serialized message within this bound.
pub const MAX_OUTBOUND_MESSAGE_BYTES: usize = 4 * 1024 * 1024;
/// `clientOutboundMaxBytes`.
pub const DEFAULT_MAX_BYTES: usize = MAX_OUTBOUND_MESSAGE_BYTES;

/// Message types whose newest frame supersedes a queued same-type tail —
/// the exact list from `encodeMessage` in `internal/transport/ws.go`,
/// plus Phase-5's `caps_update`: a queued older capability set is
/// superseded by the newest one, same as the snapshot streams.
/// Snapshot/state streams collapse; deltas, receipts, and per-event
/// broadcasts never do.
pub const REPLACEABLE_TYPES: &[&str] = &[
    "agents",
    "inventory_status",
    "update_status",
    "app_deploy_status",
    "herdr_status",
    "pane_content",
    "pane_unchanged",
    "pane_resync",
    "caps_update",
];

/// Whether `kind` is in the replaceable set (`encodeMessage`'s decision).
pub fn is_replaceable(kind: &str) -> bool {
    REPLACEABLE_TYPES.contains(&kind)
}

/// `pushResult` — with the rejection reason Go keeps internal surfaced for
/// callers/metrics (the fixture vectors distinguish them).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushResult {
    /// See [`RejectReason`].
    Rejected(RejectReason),
    /// Appended at the tail.
    Queued,
    /// Replaced an identical-type replaceable tail in place — queue length
    /// is unchanged, so the consumer is not re-signaled.
    Coalesced,
}

/// Why a push was refused. Ordering mirrors `pushTyped`: the coalesce path
/// is tried before the capacity checks, and the item cap is checked before
/// the byte cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectReason {
    /// Buffer closed.
    Closed,
    /// `len(items) >= max_items`.
    ItemLimit,
    /// `bytes + len(data) > max_bytes`.
    ByteLimit,
    /// A same-type replaceable tail existed but the merged payload would
    /// exceed `max_bytes` — the tail is untouched.
    CoalesceByteLimit,
}

impl PushResult {
    /// `Push`/`PushTyped`'s bool: anything but rejection admitted the data.
    pub fn admitted(&self) -> bool {
        !matches!(self, PushResult::Rejected(_))
    }
}

#[derive(Debug)]
struct BufferedMessage {
    data: Vec<u8>,
    kind: String,
    replaceable: bool,
}

/// The bounded queue. Synchronous; callers that need blocking-pop wrap it
/// in their own signaling.
#[derive(Debug)]
pub struct SendBuffer {
    items: VecDeque<BufferedMessage>,
    bytes: usize,
    max_items: usize,
    max_bytes: usize,
    closed: bool,
}

impl SendBuffer {
    /// Production capacities (64 items / 4 MiB).
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_MAX_ITEMS, DEFAULT_MAX_BYTES)
    }

    /// `newSendBuffer(maxItems, maxBytes)`.
    pub fn with_capacity(max_items: usize, max_bytes: usize) -> Self {
        Self {
            items: VecDeque::new(),
            bytes: 0,
            max_items,
            max_bytes,
            closed: false,
        }
    }

    /// `Push` — the type is sniffed from the serialized envelope's `type`
    /// field, and the message is never treated as replaceable (Go passes
    /// `false` here; the hub path calls [`push_typed`](Self::push_typed)
    /// with the `encodeMessage` verdict instead).
    pub fn push(&mut self, data: Vec<u8>) -> PushResult {
        let kind = sniff_message_type(&data).unwrap_or_default();
        self.push_typed(data, kind, false)
    }

    /// `pushTyped` — queue one serialized message.
    ///
    /// Replaceable incoming data merges with a replaceable tail **of the
    /// same type**; the merged size still has to fit the byte budget. Any
    /// other overflow rejects without touching the queue.
    pub fn push_typed(&mut self, data: Vec<u8>, kind: String, replaceable: bool) -> PushResult {
        if self.closed {
            return PushResult::Rejected(RejectReason::Closed);
        }
        if replaceable && !self.items.is_empty() {
            let tail = self.items.back().expect("non-empty");
            if tail.replaceable && tail.kind == kind {
                let next_bytes = self.bytes - tail.data.len() + data.len();
                if next_bytes > self.max_bytes {
                    return PushResult::Rejected(RejectReason::CoalesceByteLimit);
                }
                self.bytes = next_bytes;
                self.items.back_mut().expect("non-empty").data = data;
                return PushResult::Coalesced;
            }
        }
        if self.items.len() >= self.max_items {
            return PushResult::Rejected(RejectReason::ItemLimit);
        }
        if self.bytes + data.len() > self.max_bytes {
            return PushResult::Rejected(RejectReason::ByteLimit);
        }
        self.bytes += data.len();
        self.items.push_back(BufferedMessage {
            data,
            kind,
            replaceable,
        });
        PushResult::Queued
    }

    /// Non-blocking `Pop` — the front item, FIFO. `None` when empty or when
    /// the buffer is closed and drained. (Go blocks on a `sync.Cond`; this
    /// model leaves waking to the caller.)
    pub fn pop(&mut self) -> Option<Vec<u8>> {
        let item = self.items.pop_front()?;
        self.bytes -= item.data.len();
        Some(item.data)
    }

    /// `Close` — pushes reject; queued items remain drainable, matching Go
    /// (its `Pop` still returns buffered items after `Close`).
    pub fn close(&mut self) {
        self.closed = true;
    }

    pub fn is_closed(&self) -> bool {
        self.closed
    }

    /// `Len` — queued item count.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// `Bytes` — queued serialized bytes.
    pub fn bytes(&self) -> usize {
        self.bytes
    }
}

impl Default for SendBuffer {
    fn default() -> Self {
        Self::new()
    }
}

/// `messageType(data)` — sniff the envelope's `type` field without a typed
/// decode.
fn sniff_message_type(data: &[u8]) -> Option<String> {
    #[derive(serde::Deserialize)]
    struct Envelope<'a> {
        #[serde(rename = "type", borrow)]
        kind: std::borrow::Cow<'a, str>,
    }
    serde_json::from_slice::<Envelope>(data)
        .ok()
        .map(|e| e.kind.into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_over_capacity_without_evicting() {
        let mut buffer = SendBuffer::with_capacity(64, 10);
        assert_eq!(
            buffer.push_typed(vec![0; 6], "pane_delta".into(), false),
            PushResult::Queued
        );
        assert_eq!(
            buffer.push_typed(vec![0; 5], "pane_delta".into(), false),
            PushResult::Rejected(RejectReason::ByteLimit)
        );
        assert_eq!(buffer.len(), 1);
        assert_eq!(buffer.bytes(), 6);
    }

    #[test]
    fn coalesces_replaceable_tail_only() {
        let mut buffer = SendBuffer::with_capacity(64, 4096);
        assert_eq!(
            buffer.push_typed(vec![0; 100], "pane_content".into(), true),
            PushResult::Queued
        );
        assert_eq!(
            buffer.push_typed(vec![0; 120], "pane_content".into(), true),
            PushResult::Coalesced
        );
        assert_eq!(buffer.len(), 1);
        assert_eq!(buffer.bytes(), 120);
        // A different replaceable type queues behind — no merge.
        assert_eq!(
            buffer.push_typed(vec![0; 50], "pane_unchanged".into(), true),
            PushResult::Queued
        );
        assert_eq!(buffer.len(), 2);
    }

    #[test]
    fn coalesce_over_budget_rejects() {
        let mut buffer = SendBuffer::with_capacity(64, 300);
        assert_eq!(
            buffer.push_typed(vec![0; 200], "pane_content".into(), true),
            PushResult::Queued
        );
        // Merged 301 > 300: rejected, tail intact.
        assert_eq!(
            buffer.push_typed(vec![0; 301], "pane_content".into(), true),
            PushResult::Rejected(RejectReason::CoalesceByteLimit)
        );
        assert_eq!(buffer.bytes(), 200);
    }

    #[test]
    fn pop_is_fifo_and_close_keeps_drainable() {
        let mut buffer = SendBuffer::with_capacity(64, 4096);
        buffer.push_typed(b"first".to_vec(), "a".into(), false);
        buffer.push_typed(b"second".to_vec(), "b".into(), false);
        buffer.close();
        assert_eq!(
            buffer.push_typed(b"third".to_vec(), "c".into(), false),
            PushResult::Rejected(RejectReason::Closed)
        );
        assert_eq!(buffer.pop(), Some(b"first".to_vec()));
        assert_eq!(buffer.pop(), Some(b"second".to_vec()));
        assert_eq!(buffer.pop(), None);
    }

    #[test]
    fn push_sniffs_type_from_envelope() {
        let mut buffer = SendBuffer::new();
        buffer.push(br#"{"type":"pane_delta","x":1}"#.to_vec());
        buffer.push(br#"{"type":"pane_delta","x":2}"#.to_vec());
        // push() passes replaceable=false: no coalescing.
        assert_eq!(buffer.len(), 2);
    }
}
