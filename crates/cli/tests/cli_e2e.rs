//! S1 end-to-end pins: the CLI against a contract-shaped fake daemon on
//! the endpoint the client itself derives. The fake speaks the real
//! envelope (protocol types only), so these tests pin dialing,
//! correlation, rendering, and the failure paths without a real daemon.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use openbutler_cli::{CliError, Command, execute};
use openbutler_protocol::status::TelegramState;
use openbutler_protocol::{
    DAEMON_PROTOCOL_VERSION, DaemonRequest, DaemonResponse, DaemonStatus, FrameDecoder,
    MAX_DAEMON_FRAME_BYTES, TelegramStatus,
};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

type Responder = Arc<dyn Fn(&DaemonRequest) -> DaemonResponse + Send + Sync>;

fn unique_dir(tag: &str) -> PathBuf {
    use std::sync::atomic::AtomicUsize;
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ob-cli-e2e-{tag}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Binds the derived endpoint first (so the dial cannot race the bind),
/// then serves frames until the client hangs up.
#[cfg(windows)]
fn serve(dir: &Path, responder: Responder) -> tokio::task::JoinHandle<()> {
    let name = openbutler_protocol::endpoint::endpoint_for(dir).socket_name();
    let mut options = tokio::net::windows::named_pipe::ServerOptions::new();
    options.first_pipe_instance(true);
    let server = options.create(&name).unwrap();
    tokio::spawn(async move {
        let mut server = server;
        server.connect().await.unwrap();
        frame_loop(&mut server, responder).await;
    })
}

#[cfg(not(windows))]
fn serve(dir: &Path, responder: Responder) -> tokio::task::JoinHandle<()> {
    let path = openbutler_protocol::endpoint::endpoint_for(dir)
        .target()
        .clone();
    let openbutler_protocol::endpoint::Endpoint::UnixSocket(path) = path else {
        panic!("posix endpoint must be a unix socket");
    };
    let listener = tokio::net::UnixListener::bind(&path).unwrap();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        frame_loop(&mut stream, responder).await;
    })
}

async fn frame_loop<S>(stream: &mut S, responder: Responder)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let mut decoder = FrameDecoder::new(MAX_DAEMON_FRAME_BYTES);
    let mut buffer = vec![0u8; 4096];
    loop {
        let read = stream.read(&mut buffer).await.unwrap();
        if read == 0 {
            break;
        }
        for frame in decoder.append(&buffer[..read]).unwrap() {
            let request: DaemonRequest = serde_json::from_slice(&frame).unwrap();
            let response = responder(&request);
            let mut bytes = serde_json::to_vec(&response).unwrap();
            bytes.push(b'\n');
            stream.write_all(&bytes).await.unwrap();
        }
    }
}

fn success(request: &DaemonRequest, result: serde_json::Value) -> DaemonResponse {
    DaemonResponse::Success {
        version: DAEMON_PROTOCOL_VERSION,
        id: request.id.clone(),
        ok: true,
        result,
    }
}

#[tokio::test]
async fn status_renders_the_host_report_line_by_line() {
    let dir = unique_dir("status");
    let responder: Responder = Arc::new(|request| {
        assert_eq!(request.action, "status");
        let status = DaemonStatus {
            pid: 4242,
            started_at: "2026-11-22T01:02:03Z".to_owned(),
            workspace_count: 3,
            connected_clients: 1,
            schedules: Some(4),
            telegram: Some(TelegramStatus {
                state: TelegramState::Polling,
                paired: true,
                username: Some("helper_bot".to_owned()),
                error: None,
            }),
            unresponsive: None,
            entry: Some("dist/index.js".to_owned()),
            build_id: Some(None),
        };
        success(request, serde_json::to_value(&status).unwrap())
    });
    let _server = serve(&dir, responder);

    let text = execute(Command::Status, &dir).await.unwrap();
    assert_eq!(
        text,
        "pid:           4242\n\
         started:       2026-11-22T01:02:03Z\n\
         workspaces:    3\n\
         clients:       1\n\
         schedules:     4\n\
         telegram:      polling (@helper_bot)\n\
         entry:         dist/index.js\n\
         build:         source"
    );
}

#[tokio::test]
async fn sessions_renders_full_ids_models_and_the_running_marker() {
    let dir = unique_dir("sessions");
    let rows = json!([
        {
            "id": "sess-alpha", "title": "Alpha chat", "preview": "hi",
            "updatedAt": "2026-11-22T03:00:00Z",
            "providerId": "mock", "modelId": "demo", "running": true
        },
        {
            "id": "sess-beta", "title": "Beta chat", "preview": "yo",
            "updatedAt": "2026-11-21T22:00:00Z",
            "providerId": "openai", "modelId": "gpt-x"
        }
    ]);
    let responder: Responder = Arc::new(move |request| {
        assert_eq!(request.action, "session.list");
        success(request, rows.clone())
    });
    let _server = serve(&dir, responder);

    let text = execute(Command::Sessions, &dir).await.unwrap();
    assert_eq!(
        text,
        "sess-alpha  2026-11-22T03:00:00Z  mock/demo [running]  Alpha chat\n\
         sess-beta  2026-11-21T22:00:00Z  openai/gpt-x  Beta chat"
    );
}

#[tokio::test]
async fn an_empty_session_list_says_so() {
    let dir = unique_dir("sessions-empty");
    let responder: Responder = Arc::new(|request| success(request, json!([])));
    let _server = serve(&dir, responder);

    let text = execute(Command::Sessions, &dir).await.unwrap();
    assert_eq!(text, "No sessions.");
}

#[tokio::test]
async fn a_daemon_failure_surfaces_its_sentence_verbatim() {
    let dir = unique_dir("failure");
    let responder: Responder = Arc::new(|request| DaemonResponse::Failure {
        version: DAEMON_PROTOCOL_VERSION,
        id: request.id.clone(),
        ok: false,
        error: "This subagent session is view-only.".to_owned(),
    });
    let _server = serve(&dir, responder);

    let error = execute(Command::Sessions, &dir).await.unwrap_err();
    assert!(matches!(error, CliError::Answer(_)));
    assert_eq!(error.to_string(), "This subagent session is view-only.");
}

#[tokio::test]
async fn a_response_for_another_request_is_refused() {
    let dir = unique_dir("correlation");
    let responder: Responder = Arc::new(|_request| DaemonResponse::Success {
        version: DAEMON_PROTOCOL_VERSION,
        id: "not-the-requested-id".to_owned(),
        ok: true,
        result: json!([]),
    });
    let _server = serve(&dir, responder);

    let error = execute(Command::Sessions, &dir).await.unwrap_err();
    assert!(matches!(error, CliError::Client(_)));
    assert!(error.to_string().contains("different request id"));
}

#[tokio::test]
async fn a_missing_daemon_is_an_actionable_message() {
    let dir = unique_dir("absent");

    let error = execute(Command::Status, &dir).await.unwrap_err();
    let message = error.to_string();
    assert!(
        message.contains("cannot connect to the daemon"),
        "{message}"
    );
    assert!(message.contains("is it running?"), "{message}");
}
