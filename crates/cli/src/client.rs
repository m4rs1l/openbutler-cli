//! The framed client: one connection, one request at a time.
//!
//! Frames, envelope shapes, and every validation rule come from
//! `openbutler-protocol`; this module only owns transport (dialing the
//! derived endpoint and moving bytes) and the correlation check the
//! envelope does not perform on the client side (response id echoes the
//! request id).

use std::path::Path;

use openbutler_protocol::endpoint::ResolvedEndpoint;
use openbutler_protocol::{
    DaemonEvent, DaemonRequest, DaemonResponse, FrameDecoder, MAX_DAEMON_FRAME_BYTES,
    validate_response,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Transport failures with the context a human needs to act.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// Nothing is listening where this data directory points.
    #[error("cannot connect to the daemon at {endpoint} — is it running? (data dir: {data_dir})")]
    Unreachable {
        endpoint: String,
        data_dir: String,
        #[source]
        source: std::io::Error,
    },
    /// The socket spoke but the exchange failed.
    #[error("daemon I/O failed: {0}")]
    Io(#[from] std::io::Error),
    /// A frame breached the contract cap or split badly.
    #[error("daemon frame error: {0}")]
    Frame(#[from] openbutler_protocol::FrameError),
    /// A frame was not JSON.
    #[error("daemon sent a frame that is not JSON")]
    Json(#[from] serde_json::Error),
    /// The response settled a different request (or arrived malformed).
    #[error("daemon answered a different request id (expected {expected})")]
    CorrelationMismatch { expected: String },
    /// A response frame where only events belong (the run loop's lane).
    #[error("daemon sent an unexpected response frame (id {id})")]
    StrayResponse { id: String },
    /// The response failed the envelope's own response gate.
    #[error("daemon response violates the contract: {0}")]
    Response(#[from] openbutler_protocol::ValidationError),
}

#[cfg(windows)]
type Stream = tokio::net::windows::named_pipe::NamedPipeClient;
#[cfg(not(windows))]
type Stream = tokio::net::UnixStream;

/// One live connection to the daemon.
pub struct Client {
    stream: Stream,
    decoder: FrameDecoder,
    buffer: Vec<u8>,
    /// Frames a single read completed but the caller has not asked for yet:
    /// one read can carry a burst (the host flushes a response and the events
    /// behind it back to back), and dropping the tail loses state changes.
    pending: std::collections::VecDeque<Vec<u8>>,
    data_dir: String,
    endpoint: String,
}

impl Client {
    /// Dials the endpoint derived from `data_dir`.
    pub async fn connect(data_dir: &Path) -> Result<Self, ClientError> {
        let resolved = openbutler_protocol::endpoint::endpoint_for(data_dir);
        let endpoint = resolved.socket_name();
        let stream = dial(&resolved)
            .await
            .map_err(|source| ClientError::Unreachable {
                endpoint: endpoint.clone(),
                data_dir: data_dir.display().to_string(),
                source,
            })?;
        Ok(Self {
            stream,
            decoder: FrameDecoder::new(MAX_DAEMON_FRAME_BYTES),
            buffer: vec![0u8; 16 * 1024],
            pending: std::collections::VecDeque::new(),
            data_dir: data_dir.display().to_string(),
            endpoint,
        })
    }

    /// Sends one request and waits for its correlated response. Frames
    /// that belong to the event lane are dropped — use
    /// [`Self::call_with_events`] when a caller renders them.
    pub async fn call(&mut self, request: &DaemonRequest) -> Result<DaemonResponse, ClientError> {
        self.call_with_events(request, |_| {}).await
    }

    /// Sends one request and waits for its correlated response, handing
    /// every event frame on the same socket to `on_event` as it arrives
    /// (the answer is correlated, events stream past it).
    pub async fn call_with_events<F>(
        &mut self,
        request: &DaemonRequest,
        mut on_event: F,
    ) -> Result<DaemonResponse, ClientError>
    where
        F: FnMut(DaemonEvent),
    {
        let mut bytes = serde_json::to_vec(request)?;
        bytes.push(b'\n');
        self.stream.write_all(&bytes).await?;
        loop {
            let frame = match self.next_frame().await? {
                Some(frame) => frame,
                None => {
                    return Err(ClientError::Io(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "daemon closed the connection before answering",
                    )));
                }
            };
            let value: serde_json::Value = serde_json::from_slice(&frame)?;
            if value.get("event").is_some() {
                let event: DaemonEvent = serde_json::from_value(value)?;
                on_event(event);
                continue;
            }
            let response: DaemonResponse = serde_json::from_value(value)?;
            if response.id() != request.id {
                return Err(ClientError::CorrelationMismatch {
                    expected: request.id.clone(),
                });
            }
            validate_response(&response, request.version)?;
            return Ok(response);
        }
    }

    /// Reads the next event frame (the run loop's lane); a response here
    /// is a protocol fault, EOF is an I/O fault. Timeout is the caller's
    /// deadline, not this method's.
    pub async fn next_event(&mut self) -> Result<DaemonEvent, ClientError> {
        let frame = match self.next_frame().await? {
            Some(frame) => frame,
            None => {
                return Err(ClientError::Io(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "daemon closed the connection mid-run",
                )));
            }
        };
        let value: serde_json::Value = serde_json::from_slice(&frame)?;
        if value.get("event").is_some() {
            return Ok(serde_json::from_value(value)?);
        }
        let response: DaemonResponse = serde_json::from_value(value)?;
        Err(ClientError::StrayResponse {
            id: response.id().to_owned(),
        })
    }

    /// Reads until one complete frame is assembled, keeping every frame the
    /// read completed: the next call serves them from the queue instead of
    /// waiting for a socket read that will never come.
    async fn next_frame(&mut self) -> Result<Option<Vec<u8>>, ClientError> {
        loop {
            if let Some(frame) = self.pending.pop_front() {
                return Ok(Some(frame));
            }
            let read = self.stream.read(&mut self.buffer).await?;
            // A zero-byte read is EOF, but the decoder may still hold a
            // complete frame from bytes already consumed.
            let frames = if read == 0 {
                self.decoder.append(&[])?
            } else {
                self.decoder.append(&self.buffer[..read])?
            };
            self.pending.extend(frames);
            if read == 0 && self.pending.is_empty() {
                return Ok(None);
            }
        }
    }

    /// Where this client dialed (error context for callers that need it).
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// The data directory this connection belongs to.
    pub fn data_dir(&self) -> &str {
        &self.data_dir
    }
}

#[cfg(windows)]
async fn dial(resolved: &ResolvedEndpoint) -> std::io::Result<Stream> {
    // CreateFile itself proves the connection; no probe read (a zero-byte
    // read would wait for readability the server can never send first).
    tokio::net::windows::named_pipe::ClientOptions::new().open(resolved.socket_name())
}

#[cfg(not(windows))]
async fn dial(resolved: &ResolvedEndpoint) -> std::io::Result<Stream> {
    Stream::connect(resolved.socket_name()).await
}
