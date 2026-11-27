//! The framed client: one connection, one request at a time.
//!
//! Frames, envelope shapes, and every validation rule come from
//! `openbutler-protocol`; this module only owns transport (dialing the
//! derived endpoint, or the stored backend's TCP address, and moving bytes)
//! and the correlation check the envelope does not perform on the client side
//! (response id echoes the request id).
//!
//! Resolution order: a stored backend wins, so a client that signed in with
//! `connect` keeps talking to that host; without one, the derived local
//! endpoint is dialed and no admission frame is sent. There is no silent
//! fallback in either direction — a stored backend that fails to authenticate
//! is an error, never a quiet question to the local socket.

use std::path::Path;

use openbutler_protocol::BackendTarget;
use openbutler_protocol::auth::LoginSuccess;
use openbutler_protocol::constants::CLIENT_CONNECT_TIMEOUT_MS;
use openbutler_protocol::endpoint::ResolvedEndpoint;
use openbutler_protocol::{
    DaemonEvent, DaemonRequest, DaemonResponse, FrameDecoder, MAX_DAEMON_FRAME_BYTES,
    validate_response,
};
use serde_json::Value;
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
    /// The stored backend cannot be used (the message names the file).
    #[error("{0}")]
    Backend(String),
    /// The host answered an admission step with its own refusal sentence.
    #[error("{0}")]
    Refused(String),
    /// An admission answer did not carry what the contract promises.
    #[error("daemon admission answer is malformed")]
    Handshake,
    /// The TCP address could not be parsed into `host:port`.
    #[error("`{0}` is not a host and port, e.g. 100.64.0.5:45280")]
    Address(String),
}

/// One stream per transport; both speak the same framed bytes.
enum Transport {
    Local(Stream),
    Remote(tokio::net::TcpStream),
}

impl Transport {
    async fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Transport::Local(stream) => stream.read(buffer).await,
            Transport::Remote(stream) => stream.read(buffer).await,
        }
    }

    async fn write_all(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        match self {
            Transport::Local(stream) => stream.write_all(bytes).await,
            Transport::Remote(stream) => stream.write_all(bytes).await,
        }
    }
}

#[cfg(windows)]
type Stream = tokio::net::windows::named_pipe::NamedPipeClient;
#[cfg(not(windows))]
type Stream = tokio::net::UnixStream;

/// One live connection to the daemon.
pub struct Client {
    transport: Transport,
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
    /// Dials the stored backend when there is one, otherwise the derived
    /// local endpoint.
    pub async fn connect(data_dir: &Path) -> Result<Self, ClientError> {
        match crate::remote::load(data_dir).map_err(ClientError::Backend)? {
            None => Self::connect_local(data_dir).await,
            Some(stored) => {
                let address = address_of(&stored.target);
                let mut client = Self::dial_remote(&address, data_dir).await?;
                client.authenticate(&stored.session.token).await?;
                Ok(client)
            }
        }
    }

    /// Dials the local endpoint, skipping the stored backend. Host
    /// management (`daemon start|stop|restart|run`) uses this: those verbs
    /// are about this machine's process, never the remote one.
    pub async fn connect_local(data_dir: &Path) -> Result<Self, ClientError> {
        let resolved = openbutler_protocol::endpoint::endpoint_for(data_dir);
        let endpoint = resolved.socket_name();
        let stream = dial(&resolved)
            .await
            .map_err(|source| ClientError::Unreachable {
                endpoint: endpoint.clone(),
                data_dir: data_dir.display().to_string(),
                source,
            })?;
        Ok(Self::with_transport(
            Transport::Local(stream),
            data_dir,
            endpoint,
        ))
    }

    /// Dials a TCP address (`host:port`) without authenticating. The caller
    /// sends `login` or `auth` next; nothing is dispatched before that.
    pub async fn dial_remote(address: &str, data_dir: &Path) -> Result<Self, ClientError> {
        let connect = tokio::net::TcpStream::connect(address);
        let stream = tokio::time::timeout(
            std::time::Duration::from_millis(CLIENT_CONNECT_TIMEOUT_MS),
            connect,
        )
        .await
        .map_err(|_| ClientError::Unreachable {
            endpoint: address.to_owned(),
            data_dir: data_dir.display().to_string(),
            source: std::io::Error::new(std::io::ErrorKind::TimedOut, "the connection timed out"),
        })?
        .map_err(|source| ClientError::Unreachable {
            endpoint: address.to_owned(),
            data_dir: data_dir.display().to_string(),
            source,
        })?;
        Ok(Self::with_transport(
            Transport::Remote(stream),
            data_dir,
            address.to_owned(),
        ))
    }

    fn with_transport(transport: Transport, data_dir: &Path, endpoint: String) -> Self {
        Self {
            transport,
            decoder: FrameDecoder::new(MAX_DAEMON_FRAME_BYTES),
            buffer: vec![0u8; 16 * 1024],
            pending: std::collections::VecDeque::new(),
            data_dir: data_dir.display().to_string(),
            endpoint,
        }
    }

    /// Presents a stored session as the connection's first frame.
    pub async fn authenticate(&mut self, token: &str) -> Result<(), ClientError> {
        let request = DaemonRequest::new("auth-handshake", "auth").with("token", token);
        let result = self.call_ok(&request).await?;
        if result.get("authenticated").and_then(Value::as_bool) != Some(true) {
            return Err(ClientError::Handshake);
        }
        Ok(())
    }

    /// Signs this connection in with the account credentials and returns the
    /// session the host issued.
    pub async fn login(
        &mut self,
        username: &str,
        password: &str,
        device: &str,
    ) -> Result<LoginSuccess, ClientError> {
        let request = DaemonRequest::new("login-handshake", "login")
            .with("username", username)
            .with("password", password)
            .with("device", device);
        let result = self.call_ok(&request).await?;
        serde_json::from_value::<LoginSuccess>(result).map_err(|_| ClientError::Handshake)
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
        self.transport.write_all(&bytes).await?;
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

    /// One request whose answer must be a success: the admission steps use
    /// this, because a refusal there is the answer, not a payload.
    async fn call_ok(&mut self, request: &DaemonRequest) -> Result<Value, ClientError> {
        match self.call(request).await? {
            DaemonResponse::Success { result, .. } => Ok(result),
            DaemonResponse::Failure { error, .. } => Err(ClientError::Refused(error)),
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
                    "daemon closed the connection",
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
            let read = self.transport.read(&mut self.buffer).await?;
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

/// The `host:port` string a stored target dials.
pub fn address_of(target: &BackendTarget) -> String {
    format!("{}:{}", target.host, target.port)
}

/// Parses `host:port` the way the client needs it: a non-empty host, a port in
/// 1-65535. The host may be a name or a literal; the daemon's own listen rule
/// (tailnet or loopback) is the server's business, not the client's.
pub fn parse_address(spec: &str) -> Result<(String, u16), ClientError> {
    let trimmed = spec.trim();
    let Some((host, port_text)) = trimmed.rsplit_once(':') else {
        return Err(ClientError::Address(spec.to_owned()));
    };
    if host.is_empty()
        || port_text.is_empty()
        || port_text.len() > 5
        || !port_text.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(ClientError::Address(spec.to_owned()));
    }
    let port: u16 = port_text
        .parse()
        .map_err(|_| ClientError::Address(spec.to_owned()))?;
    if port == 0 {
        return Err(ClientError::Address(spec.to_owned()));
    }
    Ok((host.to_owned(), port))
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
