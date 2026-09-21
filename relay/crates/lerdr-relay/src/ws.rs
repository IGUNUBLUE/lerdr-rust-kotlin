//! `FrameIo` over an axum [`WebSocket`].
//!
//! Parity with Go's `webSocketConn` (`internal/transport/conn.go`): the
//! encrypted socket carries **text frames only** (`requireText`) — a binary
//! frame is a protocol violation and kills the connection. The session codec
//! is therefore always [`Codec::Json`]; the binary codec rides the future
//! DataChannel transport, not WS.

use axum::extract::ws::{CloseFrame, Message, Utf8Bytes, WebSocket};
use futures_util::{
    stream::{SplitSink, SplitStream},
    SinkExt, StreamExt,
};
use lerdr_e2ee::Codec;

use crate::frame::{CloseStatus, FrameIo, FrameRead, FrameWrite, ReadError, WriteError};

/// A WebSocket connection as a [`FrameIo`].
pub struct WsIo {
    socket: WebSocket,
}

impl WsIo {
    pub fn new(socket: WebSocket) -> Self {
        Self { socket }
    }
}

impl FrameIo for WsIo {
    type Reader = WsReader;
    type Writer = WsWriter;

    fn codec(&self) -> Codec {
        Codec::Json
    }

    fn transport(&self) -> &'static str {
        "websocket"
    }

    fn split(self) -> (WsReader, WsWriter) {
        let (sink, stream) = self.socket.split();
        (WsReader { stream }, WsWriter::new(sink))
    }
}

/// The read half: text frames become logical frames; ping/pong are handled
/// by tungstenite below this layer; binary frames are refused outright.
pub struct WsReader {
    stream: SplitStream<WebSocket>,
}

impl FrameRead for WsReader {
    async fn read_frame(&mut self) -> Result<Vec<u8>, ReadError> {
        loop {
            match self.stream.next().await {
                Some(Ok(Message::Text(text))) => return Ok(text.as_str().as_bytes().to_vec()),
                Some(Ok(Message::Binary(_))) => {
                    return Err(ReadError::failed(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "encrypted websocket frames must be text",
                    )))
                }
                Some(Ok(Message::Close(frame))) => {
                    let (code, reason) = frame
                        .map(|f| (Some(f.code), f.reason.to_string()))
                        .unwrap_or((None, String::new()));
                    return Err(ReadError::Closed { code, reason });
                }
                // Ping/Pong: tungstenite answers pings automatically.
                Some(Ok(Message::Ping(_))) | Some(Ok(Message::Pong(_))) => continue,
                Some(Err(err)) => return Err(ReadError::failed(err)),
                None => {
                    return Err(ReadError::Closed {
                        code: None,
                        reason: String::new(),
                    })
                }
            }
        }
    }
}

/// The write half: every logical frame leaves as a WS text message.
pub struct WsWriter {
    sink: Option<SplitSink<WebSocket, Message>>,
}

impl WsWriter {
    fn new(sink: SplitSink<WebSocket, Message>) -> Self {
        Self { sink: Some(sink) }
    }
}

impl FrameWrite for WsWriter {
    async fn write_frame(&mut self, frame: &[u8]) -> Result<(), WriteError> {
        let Some(sink) = self.sink.as_mut() else {
            return Err(WriteError::Closed);
        };
        // The JSON codec only ever produces UTF-8; refuse silently impossible
        // payloads rather than panic in a sink path.
        let text = String::from_utf8(frame.to_vec()).map_err(|e| {
            WriteError::failed(std::io::Error::new(std::io::ErrorKind::InvalidData, e))
        })?;
        sink.send(Message::Text(Utf8Bytes::from(text)))
            .await
            .map_err(WriteError::failed)
    }

    async fn close(&mut self, status: CloseStatus, reason: &str) {
        let Some(sink) = self.sink.as_mut() else {
            return;
        };
        let frame = CloseFrame {
            code: status.code(),
            reason: reason.into(),
        };
        // Best-effort; the peer may already be gone.
        let _ = sink.send(Message::Close(Some(frame))).await;
        let _ = sink.close().await;
        self.sink = None;
    }

    fn close_now(&mut self) {
        // Dropping the sink half aborts the upgraded connection once the
        // read half is gone too — the supervisor always tears down both.
        self.sink = None;
    }
}
