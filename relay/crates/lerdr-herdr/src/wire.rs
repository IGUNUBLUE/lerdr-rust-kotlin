//! NDJSON wire plumbing shared by unary requests and the event stream.
//!
//! The dispatch boundary is tracked explicitly: [`write_request`] reports how
//! many request bytes reached the socket so callers can distinguish
//! `NotStarted` (zero bytes — safe to retry) from `DispatchedUnknown` (any
//! bytes — may have applied). This mirrors the Go client's `written > 0`
//! check, which is the oracle semantics.

use std::io;
use std::time::Duration;

use serde::Serialize;
use serde_json::value::RawValue;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time::{timeout_at, Instant};

use crate::error::HerdrError;
use crate::transport::BoxIo;

/// Hard cap on a single response/event line — the Go client's
/// `maxOutputBytes` (4 MiB). Herdr itself bounds responses at this size.
pub const MAX_LINE_BYTES: usize = 4 * 1024 * 1024;

/// Default per-request deadline when the caller does not carry one — the Go
/// client's `defaultTimeout`.
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// Wire request envelope. Field order is `id`, `method`, `params` — identical
/// to Go's sorted-key map marshal.
#[derive(Debug, Serialize)]
pub(crate) struct Request<'a, P> {
    pub id: &'a str,
    pub method: &'a str,
    pub params: &'a P,
}

/// Decoded response envelope. Exactly one of `result`/`error` is meaningful.
#[derive(Debug)]
pub(crate) struct Response {
    pub id: String,
    pub result: Option<Box<RawValue>>,
    pub error: Option<ResponseError>,
}

#[derive(Debug)]
pub(crate) struct ResponseError {
    pub code: String,
    pub message: String,
}

/// Encode one request line (JSON + `\n`). Encode failure is pre-dispatch.
pub(crate) fn encode_request<P: Serialize>(
    id: &str,
    method: &str,
    params: &P,
) -> Result<Vec<u8>, HerdrError> {
    let mut payload = serde_json::to_vec(&Request { id, method, params })
        .map_err(|e| HerdrError::not_started(io::Error::new(io::ErrorKind::InvalidData, e)))?;
    payload.push(b'\n');
    Ok(payload)
}

/// Write the full request payload. The first failed `write` before any byte
/// lands is `NotStarted`; any failure after ≥1 byte is `DispatchedUnknown` —
/// the Go client's `written > 0` boundary. Success means every byte went out.
pub(crate) async fn write_request(
    conn: &mut BoxIo,
    payload: &[u8],
    deadline: Instant,
) -> Result<(), HerdrError> {
    let mut offset = 0usize;
    while offset < payload.len() {
        match timeout_at(deadline, conn.write(&payload[offset..])).await {
            Err(_elapsed) => {
                let err = io::Error::new(io::ErrorKind::TimedOut, "herdr request write timed out");
                return Err(if offset > 0 {
                    HerdrError::dispatched_io(err)
                } else {
                    HerdrError::not_started(err)
                });
            }
            Ok(Err(err)) => {
                return Err(if offset > 0 {
                    HerdrError::dispatched_io(err)
                } else {
                    HerdrError::not_started(err)
                });
            }
            Ok(Ok(0)) => {
                let err =
                    io::Error::new(io::ErrorKind::WriteZero, "herdr socket closed during write");
                return Err(if offset > 0 {
                    HerdrError::dispatched_io(err)
                } else {
                    HerdrError::not_started(err)
                });
            }
            Ok(Ok(n)) => offset += n,
        }
    }
    Ok(())
}

/// Newline-splitting reader that preserves bytes buffered past the first
/// `\n`. The event stream needs this: Herdr can flush several event lines in
/// one write, and a fresh buffer per call would silently drop them.
pub(crate) struct LineReader {
    buf: Vec<u8>,
    max_bytes: usize,
}

impl LineReader {
    pub(crate) fn new(max_bytes: usize) -> Self {
        LineReader {
            buf: Vec::with_capacity(1024),
            max_bytes,
        }
    }

    /// Read the next newline-terminated line (returned without the `\n`).
    /// `UnexpectedEof` means the peer closed before completing a line — or,
    /// when the buffer is empty, that it closed cleanly between lines.
    pub(crate) async fn next(
        &mut self,
        conn: &mut BoxIo,
        deadline: Instant,
    ) -> io::Result<Vec<u8>> {
        let mut chunk = [0u8; 8192];
        loop {
            if let Some(pos) = self.buf.iter().position(|b| *b == b'\n') {
                let line = self.buf[..pos].to_vec();
                self.buf.drain(..=pos);
                return Ok(line);
            }
            let n = match timeout_at(deadline, conn.read(&mut chunk)).await {
                Err(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "herdr socket read timed out",
                    ))
                }
                Ok(res) => res?,
            };
            if n == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "herdr socket closed without a response",
                ));
            }
            self.buf.extend_from_slice(&chunk[..n]);
            if self.buf.len() > self.max_bytes {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("herdr response exceeds {} bytes", self.max_bytes),
                ));
            }
        }
    }
}

/// Read one newline-terminated JSON line with a byte cap and a deadline —
/// the one-shot form for unary responses and the subscribe handshake.
/// Returns the line without the trailing `\n`.
pub(crate) async fn read_line(
    conn: &mut BoxIo,
    deadline: Instant,
    max_bytes: usize,
) -> io::Result<Vec<u8>> {
    LineReader::new(max_bytes).next(conn, deadline).await
}

/// Decode a response line into the envelope.
pub(crate) fn decode_response(line: &[u8]) -> Result<Response, HerdrError> {
    #[derive(serde::Deserialize)]
    struct Envelope<'a> {
        #[serde(default)]
        id: Option<&'a str>,
        #[serde(default)]
        result: Option<&'a RawValue>,
        #[serde(default)]
        error: Option<ErrBody>,
    }
    #[derive(serde::Deserialize)]
    struct ErrBody {
        code: String,
        message: String,
    }
    let env: Envelope = serde_json::from_slice(line)
        .map_err(|e| HerdrError::dispatched_io(io::Error::new(io::ErrorKind::InvalidData, e)))?;
    Ok(Response {
        id: env.id.unwrap_or_default().to_owned(),
        result: env.result.map(|r| r.to_owned()),
        error: env.error.map(|e| ResponseError {
            code: e.code,
            message: e.message,
        }),
    })
}

/// `id:""` + `code:"invalid_request"` + `message:"invalid request:…"` is
/// Herdr's pre-dispatch refusal: the request failed JSON decode/validation
/// before a dispatch decision. Go: `isPreDispatchRequestError`.
pub(crate) fn is_pre_dispatch_refusal(id: &str, code: &str, message: &str) -> bool {
    id.is_empty() && code == "invalid_request" && message.starts_with("invalid request:")
}

/// Classify a decoded response envelope against the request id.
///
/// * matching-id error, or a pre-dispatch refusal → `Refused`
/// * error with a foreign id → `DispatchedUnknown` (transport corruption)
/// * success with foreign id → `DispatchedUnknown`
/// * success with no/empty result → `DispatchedUnknown`
/// * otherwise → the raw result payload
pub(crate) fn classify_response(
    response: Response,
    request_id: &str,
) -> Result<Box<RawValue>, HerdrError> {
    if let Some(error) = response.error {
        if response.id != request_id
            && !is_pre_dispatch_refusal(&response.id, &error.code, &error.message)
        {
            return Err(HerdrError::dispatched_msg(format!(
                "herdr response id mismatch: got {:?}, want {request_id:?}",
                response.id
            )));
        }
        return Err(HerdrError::refused(error.code, error.message));
    }
    if response.id != request_id {
        return Err(HerdrError::dispatched_msg(format!(
            "herdr response id mismatch: got {:?}, want {request_id:?}",
            response.id
        )));
    }
    match response.result {
        Some(result) if result.get() != "null" => Ok(result),
        _ => Err(HerdrError::dispatched_msg(
            "herdr response has no result".to_string(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::DispatchPhase;

    #[test]
    fn encode_request_shape() {
        let payload = encode_request(
            "lerdr-api-1",
            "pane.read",
            &serde_json::json!({"pane_id":"wE:pE"}),
        )
        .unwrap();
        assert_eq!(payload.last(), Some(&b'\n'));
        let v: serde_json::Value = serde_json::from_slice(&payload[..payload.len() - 1]).unwrap();
        assert_eq!(v["id"], "lerdr-api-1");
        assert_eq!(v["method"], "pane.read");
        assert_eq!(v["params"]["pane_id"], "wE:pE");
    }

    #[test]
    fn classify_success() {
        let line = br#"{"id":"a","result":{"type":"ok"}}"#;
        let r = decode_response(line).unwrap();
        let raw = classify_response(r, "a").unwrap();
        assert!(raw.get().contains("\"ok\""));
    }

    #[test]
    fn classify_refused_matching_id() {
        let line = br#"{"id":"a","error":{"code":"pane_not_found","message":"no such pane"}}"#;
        let r = decode_response(line).unwrap();
        let err = classify_response(r, "a").unwrap_err();
        assert_eq!(err.phase(), DispatchPhase::Refused);
        assert_eq!(err.refusal_code(), Some("pane_not_found"));
    }

    #[test]
    fn classify_pre_dispatch_refusal_empty_id() {
        // id:"" + invalid_request + "invalid request:…" — herdr rejected
        // before dispatch; still a definitive Refused.
        let line = br#"{"id":"","error":{"code":"invalid_request","message":"invalid request: unknown method `nope`"}}"#;
        let r = decode_response(line).unwrap();
        let err = classify_response(r, "a").unwrap_err();
        assert_eq!(err.phase(), DispatchPhase::Refused);
        assert_eq!(err.refusal_code(), Some("invalid_request"));
    }

    #[test]
    fn classify_foreign_error_id_is_dispatched_unknown() {
        let line = br#"{"id":"zzz","error":{"code":"internal","message":"boom"}}"#;
        let r = decode_response(line).unwrap();
        let err = classify_response(r, "a").unwrap_err();
        assert_eq!(err.phase(), DispatchPhase::DispatchedUnknown);
    }

    #[test]
    fn classify_success_wrong_id() {
        let line = br#"{"id":"zzz","result":{"type":"ok"}}"#;
        let r = decode_response(line).unwrap();
        let err = classify_response(r, "a").unwrap_err();
        assert_eq!(err.phase(), DispatchPhase::DispatchedUnknown);
    }

    #[test]
    fn classify_no_result() {
        let line = br#"{"id":"a"}"#;
        let r = decode_response(line).unwrap();
        let err = classify_response(r, "a").unwrap_err();
        assert_eq!(err.phase(), DispatchPhase::DispatchedUnknown);
    }

    #[test]
    fn classify_null_result() {
        let line = br#"{"id":"a","result":null}"#;
        let r = decode_response(line).unwrap();
        let err = classify_response(r, "a").unwrap_err();
        assert_eq!(err.phase(), DispatchPhase::DispatchedUnknown);
    }

    #[test]
    fn decode_malformed_json() {
        let err = decode_response(b"{not json").unwrap_err();
        assert_eq!(err.phase(), DispatchPhase::DispatchedUnknown);
    }

    #[tokio::test]
    async fn read_line_bounds() {
        let (client, server) = tokio::net::UnixStream::pair().unwrap();
        let mut conn: BoxIo = Box::new(client);
        let mut server = server;
        tokio::spawn(async move {
            server.write_all(b"hello\nrest").await.unwrap();
        });
        let line = read_line(&mut conn, Instant::now() + Duration::from_secs(5), 1024)
            .await
            .unwrap();
        assert_eq!(line, b"hello");
    }

    #[tokio::test]
    async fn line_reader_preserves_lines_after_first_newline() {
        // The event-stream bug this guards: one socket read can deliver
        // several NDJSON lines; the reader must not drop them.
        let (client, mut server) = tokio::net::UnixStream::pair().unwrap();
        let mut conn: BoxIo = Box::new(client);
        server.write_all(b"one\ntwo\nthree\n").await.unwrap();
        server.shutdown().await.unwrap();
        let mut lines = LineReader::new(1024);
        let deadline = Instant::now() + Duration::from_secs(5);
        assert_eq!(lines.next(&mut conn, deadline).await.unwrap(), b"one");
        assert_eq!(lines.next(&mut conn, deadline).await.unwrap(), b"two");
        assert_eq!(lines.next(&mut conn, deadline).await.unwrap(), b"three");
        let err = lines.next(&mut conn, deadline).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[tokio::test]
    async fn line_reader_joins_fragments() {
        let (client, mut server) = tokio::net::UnixStream::pair().unwrap();
        let mut conn: BoxIo = Box::new(client);
        tokio::spawn(async move {
            server.write_all(b"hel").await.unwrap();
            server.write_all(b"lo\nwor").await.unwrap();
            server.write_all(b"ld\n").await.unwrap();
        });
        let mut lines = LineReader::new(1024);
        let deadline = Instant::now() + Duration::from_secs(5);
        assert_eq!(lines.next(&mut conn, deadline).await.unwrap(), b"hello");
        assert_eq!(lines.next(&mut conn, deadline).await.unwrap(), b"world");
    }

    #[tokio::test]
    async fn read_line_oversize() {
        let (client, mut server) = tokio::net::UnixStream::pair().unwrap();
        let mut conn: BoxIo = Box::new(client);
        tokio::spawn(async move {
            server.write_all(&vec![b'x'; 9000]).await.unwrap();
        });
        let err = read_line(&mut conn, Instant::now() + Duration::from_secs(5), 1024)
            .await
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn read_line_eof_before_newline() {
        let (client, server) = tokio::net::UnixStream::pair().unwrap();
        let mut conn: BoxIo = Box::new(client);
        drop(server); // peer closes without writing
        let err = read_line(&mut conn, Instant::now() + Duration::from_secs(5), 1024)
            .await
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[tokio::test(start_paused = true)]
    async fn write_timeout_before_bytes_is_not_started() {
        // A peer that never reads and a zero-length deadline: the write
        // deadline elapses before any byte lands → NotStarted.
        let (client, _server) = tokio::net::UnixStream::pair().unwrap();
        let mut conn: BoxIo = Box::new(client);
        let err = write_request(&mut conn, b"{}", Instant::now())
            .await
            .unwrap_err();
        assert_eq!(err.phase(), DispatchPhase::NotStarted);
    }
}
