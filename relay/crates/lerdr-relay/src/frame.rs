//! Logical-frame duplex abstraction — the port of Go's `FrameConn`
//! (`internal/transport/conn.go`). One `read_frame` yields exactly one logical
//! frame; one `write_frame` consumes one. The handshake driver and the session
//! pumps are written against these traits, so tests run over in-process
//! duplex transports (`duplex`) and production over axum WebSockets (`ws`).

use std::future::Future;

use lerdr_e2ee::Codec;

/// `CloseStatus` — the close semantics reported to the peer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseStatus {
    /// Ordinary end of a connection (`websocket.StatusNormalClosure`).
    Normal,
    /// The relay is shutting down (`websocket.StatusGoingAway`).
    GoingAway,
    /// Device authentication refused permanently — the phone must stop
    /// reconnecting and pair again. Wire code
    /// [`UNAUTHORIZED_CLOSE_CODE`].
    Unauthorized,
}

/// `UnauthorizedCloseCode` — application-range WS close code (4401) that no
/// proxy rewrites.
pub const UNAUTHORIZED_CLOSE_CODE: u16 = 4401;

impl CloseStatus {
    /// The numeric WS close code.
    pub fn code(self) -> u16 {
        match self {
            CloseStatus::Normal => 1000,
            CloseStatus::GoingAway => 1001,
            CloseStatus::Unauthorized => UNAUTHORIZED_CLOSE_CODE,
        }
    }
}

/// Read half failure. [`ReadError::Closed`] is an orderly peer close (or a
/// close frame observed); anything else means the transport is dead.
#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    /// The peer closed the connection. Carries the close code and reason when
    /// the transport surfaced them (`None` on bare EOF).
    #[error("frame connection closed (code {code:?}): {reason}")]
    Closed { code: Option<u16>, reason: String },
    /// Transport-level failure — a dead connection, or a frame class this
    /// transport refuses (e.g. binary WS frames on the encrypted path).
    #[error("frame read failed: {0}")]
    Failed(#[source] Box<dyn std::error::Error + Send + Sync>),
}

impl ReadError {
    /// Wrap an arbitrary error as a transport failure.
    pub fn failed<E: std::error::Error + Send + Sync + 'static>(err: E) -> Self {
        ReadError::Failed(Box::new(err))
    }

    /// Whether the peer went away cleanly.
    pub fn is_closed(&self) -> bool {
        matches!(self, ReadError::Closed { .. })
    }
}

/// Write half failure.
#[derive(Debug, thiserror::Error)]
pub enum WriteError {
    /// The connection is already closed.
    #[error("frame connection closed")]
    Closed,
    /// Transport-level failure.
    #[error("frame write failed: {0}")]
    Failed(#[source] Box<dyn std::error::Error + Send + Sync>),
}

impl WriteError {
    /// Wrap an arbitrary error as a transport failure.
    pub fn failed<E: std::error::Error + Send + Sync + 'static>(err: E) -> Self {
        WriteError::Failed(Box::new(err))
    }
}

/// The read half of a frame connection.
pub trait FrameRead: Send {
    /// Block until one logical frame arrives. `Err(ReadError::Closed)` on a
    /// clean peer close.
    fn read_frame(&mut self) -> impl Future<Output = Result<Vec<u8>, ReadError>> + Send + '_;
}

/// The write half of a frame connection.
pub trait FrameWrite: Send {
    /// Send one logical frame.
    fn write_frame<'a>(
        &'a mut self,
        frame: &'a [u8],
    ) -> impl Future<Output = Result<(), WriteError>> + Send + 'a;

    /// Best-effort graceful close — the close handshake is attempted but the
    /// implementation bounds the wait (Go: `wsCloseTimeout`, 1 s).
    fn close<'a>(
        &'a mut self,
        status: CloseStatus,
        reason: &'a str,
    ) -> impl Future<Output = ()> + Send + 'a;

    /// Drop the connection without a closing handshake. Idempotent.
    fn close_now(&mut self);
}

/// A logical-frame duplex connection — split into read/write halves owned by
/// the read/write pumps respectively.
pub trait FrameIo: Send {
    type Reader: FrameRead;
    type Writer: FrameWrite;

    /// The encrypted-frame codec this transport negotiates (`conn.Codec()`).
    /// The WS path is [`Codec::Json`]: the Go oracle rejects binary frames on
    /// the encrypted socket (`requireText`).
    fn codec(&self) -> Codec;
    /// Transport label for logs and metrics (`conn.TransportName()`).
    fn transport(&self) -> &'static str;
    /// Split into the read and write halves.
    fn split(self) -> (Self::Reader, Self::Writer);
}

/// In-process duplex transport for tests and harnesses: length-prefixed
/// records over `tokio::io::DuplexStream`.
///
/// Record layout: `[kind:u8][len:u32BE][payload]`. `kind` 0 carries a data
/// frame; `kind` 1 carries a close (`payload` = `[code:u16BE][reason utf8]`),
/// after which the peer's read reports [`ReadError::Closed`]. Writes larger
/// than the channel buffer exert real backpressure.
pub mod duplex {
    use super::{CloseStatus, FrameIo, FrameRead, FrameWrite, ReadError, WriteError};
    use lerdr_e2ee::Codec;
    use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream, ReadHalf, WriteHalf};

    const KIND_DATA: u8 = 0;
    const KIND_CLOSE: u8 = 1;
    const HEADER: usize = 1 + 4;

    /// A pair of connected in-memory frame transports.
    pub fn pair(capacity: usize, codec: Codec) -> (DuplexIo, DuplexIo) {
        let (a, b) = tokio::io::duplex(capacity);
        (DuplexIo::new(a, codec), DuplexIo::new(b, codec))
    }

    /// One end of a [`pair`].
    pub struct DuplexIo {
        stream: DuplexStream,
        codec: Codec,
    }

    impl DuplexIo {
        pub fn new(stream: DuplexStream, codec: Codec) -> Self {
            Self { stream, codec }
        }
    }

    impl FrameIo for DuplexIo {
        type Reader = DuplexReader;
        type Writer = DuplexWriter;

        fn codec(&self) -> Codec {
            self.codec
        }
        fn transport(&self) -> &'static str {
            "duplex"
        }
        fn split(self) -> (DuplexReader, DuplexWriter) {
            let (read, write) = tokio::io::split(self.stream);
            (DuplexReader { read }, DuplexWriter { write })
        }
    }

    pub struct DuplexReader {
        read: ReadHalf<DuplexStream>,
    }

    impl FrameRead for DuplexReader {
        async fn read_frame(&mut self) -> Result<Vec<u8>, ReadError> {
            let eof = |e: std::io::Error| match e.kind() {
                std::io::ErrorKind::UnexpectedEof => ReadError::Closed {
                    code: None,
                    reason: String::new(),
                },
                _ => ReadError::failed(e),
            };
            let mut header = [0u8; HEADER];
            self.read.read_exact(&mut header).await.map_err(eof)?;
            let len = u32::from_be_bytes(header[1..].try_into().unwrap()) as usize;
            let mut payload = vec![0u8; len];
            self.read.read_exact(&mut payload).await.map_err(eof)?;
            match header[0] {
                KIND_DATA => Ok(payload),
                KIND_CLOSE => {
                    let code = (payload.len() >= 2)
                        .then(|| u16::from_be_bytes(payload[..2].try_into().unwrap()));
                    let reason =
                        String::from_utf8_lossy(&payload[2.min(payload.len())..]).into_owned();
                    Err(ReadError::Closed { code, reason })
                }
                other => Err(ReadError::failed(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("unknown frame kind {other}"),
                ))),
            }
        }
    }

    pub struct DuplexWriter {
        write: WriteHalf<DuplexStream>,
    }

    impl DuplexWriter {
        async fn write_record(&mut self, kind: u8, payload: &[u8]) -> Result<(), WriteError> {
            let mut record = Vec::with_capacity(HEADER + payload.len());
            record.push(kind);
            record.extend_from_slice(&(payload.len() as u32).to_be_bytes());
            record.extend_from_slice(payload);
            self.write
                .write_all(&record)
                .await
                .map_err(|e| match e.kind() {
                    std::io::ErrorKind::BrokenPipe => WriteError::Closed,
                    _ => WriteError::failed(e),
                })?;
            self.write.flush().await.map_err(WriteError::failed)
        }
    }

    impl FrameWrite for DuplexWriter {
        async fn write_frame(&mut self, frame: &[u8]) -> Result<(), WriteError> {
            self.write_record(KIND_DATA, frame).await
        }

        async fn close(&mut self, status: CloseStatus, reason: &str) {
            let mut payload = Vec::with_capacity(2 + reason.len());
            payload.extend_from_slice(&status.code().to_be_bytes());
            payload.extend_from_slice(reason.as_bytes());
            // Best-effort: if the peer is already gone, so be it.
            let _ = self.write_record(KIND_CLOSE, &payload).await;
            let _ = self.write.shutdown().await;
        }

        fn close_now(&mut self) {
            // Swap in a dead write half and drive a FIN on the real one; the
            // peer's next read reports bare EOF (`Closed{code: None}`).
            let (_, dead) = tokio::io::split(tokio::io::duplex(0).1);
            let mut write = std::mem::replace(&mut self.write, dead);
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    let _ = write.shutdown().await;
                });
            }
        }
    }
}
