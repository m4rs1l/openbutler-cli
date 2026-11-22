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
use serde_json::{Value, json};
use std::time::Duration;
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

// ---------------------------------------------------------------- run (S2)

#[derive(Clone)]
struct Step {
    action: &'static str,
    result: Value,
    error: Option<&'static str>,
    events: Vec<Value>,
}

/// Answers each request from a script (action-checked) and pushes the
/// scripted event frames after the response, like the real host flushes
/// its pending events.
#[cfg(windows)]
fn serve_steps(dir: &Path, steps: Vec<Step>) -> tokio::task::JoinHandle<()> {
    let name = openbutler_protocol::endpoint::endpoint_for(dir).socket_name();
    let mut options = tokio::net::windows::named_pipe::ServerOptions::new();
    options.first_pipe_instance(true);
    let server = options.create(&name).unwrap();
    tokio::spawn(async move {
        let mut server = server;
        server.connect().await.unwrap();
        let mut decoder = FrameDecoder::new(MAX_DAEMON_FRAME_BYTES);
        let mut buffer = vec![0u8; 8192];
        let mut index = 0usize;
        loop {
            let read = server.read(&mut buffer).await.unwrap();
            if read == 0 {
                break;
            }
            for frame in decoder.append(&buffer[..read]).unwrap() {
                let request: DaemonRequest = serde_json::from_slice(&frame).unwrap();
                let step = steps
                    .get(index)
                    .unwrap_or_else(|| panic!("unexpected request #{index}: {}", request.action));
                assert_eq!(request.action, step.action, "script step #{index}");
                index += 1;
                let response = match &step.error {
                    Some(message) => DaemonResponse::Failure {
                        version: DAEMON_PROTOCOL_VERSION,
                        id: request.id.clone(),
                        ok: false,
                        error: (*message).to_owned(),
                    },
                    None => success(&request, step.result.clone()),
                };
                let mut bytes = serde_json::to_vec(&response).unwrap();
                bytes.push(b'\n');
                server.write_all(&bytes).await.unwrap();
                for event in &step.events {
                    let mut bytes = serde_json::to_vec(event).unwrap();
                    bytes.push(b'\n');
                    server.write_all(&bytes).await.unwrap();
                }
            }
        }
    })
}

fn run_meta(id: &str) -> openbutler_protocol::session::view::SessionMeta {
    use openbutler_protocol::session::view::SessionMeta;
    SessionMeta {
        project: None,
        version: 1,
        id: id.to_owned(),
        title: "New session".to_owned(),
        created_at: "2026-11-22T05:00:00.000Z".to_owned(),
        updated_at: "2026-11-22T05:00:00.000Z".to_owned(),
        last_activity_at: Some("2026-11-22T05:00:00.000Z".to_owned()),
        provider_id: "mock".to_owned(),
        model_id: "demo".to_owned(),
        reasoning: None,
        show_reasoning: None,
        reasoning_variants: None,
        supports_vision: None,
        reasoning_support: None,
        reasoning_metadata_source: None,
        reasoning_titles: None,
        workspace_root: Some("/tmp/ws".to_owned()),
        context_tokens: None,
        context_estimate_tokens: None,
        context_window_tokens: None,
        key_positions: Some(Default::default()),
        has_user_message: None,
        automatic_wake_held: None,
        preview: None,
        first_user_message: None,
        last_assistant_message: None,
    }
}

fn run_runtime() -> openbutler_protocol::RuntimeViewData {
    openbutler_protocol::RuntimeViewData {
        supports_session_attachments: None,
        supports_projects: None,
        workspace_root: "/tmp/ws".to_owned(),
        model_name: "Test Model".to_owned(),
        provider_name: Some("MockProvider".to_owned()),
        provider_id: "mock".to_owned(),
        model_id: "demo".to_owned(),
        context_window_tokens: None,
        custodian_context_limit_tokens: None,
        supports_vision: false,
        reasoning_variants: Vec::new(),
        reasoning_metadata_source: None,
        reasoning_support: openbutler_protocol::runtime::view::ReasoningSupport::Available,
        reasoning_default: String::new(),
        reasoning_preference: String::new(),
        show_reasoning_default: false,
        reasoning_titles: None,
        approval_mode: None,
    }
}

fn run_state(busy: bool) -> openbutler_protocol::session::run::RunState {
    openbutler_protocol::session::run::RunState {
        busy,
        running_turn_id: if busy {
            Some("turn-1".to_owned())
        } else {
            None
        },
        pending_approval: None,
        pending_approval_id: None,
        pending_question: None,
        queued_message_count: 0,
        first_queued_message: None,
        queued_messages: Vec::new(),
        session_save_error: None,
        streaming_assistant_index: None,
    }
}

fn attach_fixture() -> Value {
    use openbutler_protocol::session::{SessionSnapshotData, SessionView};
    use openbutler_protocol::{AttachResult, LocalClientKind};
    serde_json::to_value(AttachResult {
        host_entry: None,
        host_build_id: Some(None),
        session_attachments: Some(true),
        workspace_key: Some("/tmp/ws".to_owned()),
        client_kind: Some(LocalClientKind::Terminal),
        snapshot: SessionSnapshotData {
            session: SessionView {
                meta: run_meta("sess-run"),
                entries: Vec::new(),
            },
            run: run_state(false),
            entries_from: None,
            total_entries: None,
        },
        runtime: run_runtime(),
        saved_sessions: Vec::new(),
    })
    .unwrap()
}

fn busy_event() -> Value {
    use openbutler_protocol::DaemonEvent;
    use openbutler_protocol::session::{SessionSnapshotData, SessionView};
    serde_json::to_value(DaemonEvent::Snapshot {
        version: DAEMON_PROTOCOL_VERSION,
        snapshot: SessionSnapshotData {
            session: SessionView {
                meta: run_meta("sess-run"),
                entries: Vec::new(),
            },
            run: run_state(false),
            entries_from: None,
            total_entries: None,
        },
        runtime: run_runtime(),
    })
    .unwrap()
}

fn patch_event(busy: bool, deltas: &[&str]) -> Value {
    use openbutler_protocol::DaemonEvent;
    use openbutler_protocol::session::patch::TextDelta;
    use openbutler_protocol::session::{SessionEntriesPatch, SessionPatch};
    serde_json::to_value(DaemonEvent::Patch {
        version: DAEMON_PROTOCOL_VERSION,
        patch: SessionPatch {
            session: run_meta("sess-run"),
            entries_patch: if deltas.is_empty() {
                None
            } else {
                Some(SessionEntriesPatch::Delta {
                    append: Vec::new(),
                    replace: Vec::new(),
                    append_text: deltas
                        .iter()
                        .map(|delta| TextDelta {
                            index: 0,
                            delta: (*delta).to_owned(),
                        })
                        .collect(),
                })
            },
            run: run_state(busy),
            runtime: run_runtime(),
        },
    })
    .unwrap()
}

#[tokio::test]
async fn a_run_streams_the_answer_and_exits_cleanly() {
    let dir = unique_dir("run-stream");
    let steps = vec![
        Step {
            action: "attach",
            result: attach_fixture(),
            error: None,
            events: vec![busy_event()],
        },
        Step {
            action: "session.send",
            result: json!({ "accepted": true }),
            error: None,
            events: vec![patch_event(true, &["Hel", "lo"]), patch_event(false, &[])],
        },
    ];
    let _server = serve_steps(&dir, steps);

    let mut out = Vec::new();
    let mut err = Vec::new();
    openbutler_cli::commands::run::run(
        &dir,
        openbutler_cli::commands::run::RunOptions::new("say hello"),
        &mut out,
        &mut err,
    )
    .await
    .unwrap();

    assert_eq!(
        String::from_utf8_lossy(&out),
        "OpenButler agent\n\
         Model: Test Model / demo\n\
         Provider: MockProvider\n\
         Your request, tool definitions, and approved tool results will be sent to MockProvider.\n\
         Hello\n"
    );
    assert!(
        err.is_empty(),
        "stderr stays quiet: {}",
        String::from_utf8_lossy(&err)
    );
}

#[tokio::test]
async fn a_stalled_run_stops_with_the_source_sentence() {
    let dir = unique_dir("run-stall");
    let steps = vec![
        Step {
            action: "attach",
            result: attach_fixture(),
            error: None,
            events: vec![busy_event()],
        },
        Step {
            action: "session.send",
            result: json!({ "accepted": true }),
            error: None,
            events: vec![patch_event(true, &["started"])],
        },
        // The stall path stops the run before leaving.
        Step {
            action: "session.stop",
            result: json!(true),
            error: None,
            events: Vec::new(),
        },
    ];
    let _server = serve_steps(&dir, steps);

    let mut out = Vec::new();
    let mut err = Vec::new();
    let mut options = openbutler_cli::commands::run::RunOptions::new("never settles");
    options.start_timeout = Duration::from_secs(30);
    options.idle_timeout = Duration::from_millis(150);
    let error = openbutler_cli::commands::run::run(&dir, options, &mut out, &mut err)
        .await
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "One-shot run stopped: no session progress was reported for 10 minutes."
    );
    assert_eq!(
        String::from_utf8_lossy(&out),
        "OpenButler agent\nModel: Test Model / demo\nProvider: MockProvider\nYour request, tool definitions, and approved tool results will be sent to MockProvider.\nstarted"
    );
}

#[tokio::test]
async fn a_run_that_never_starts_fails_with_the_start_sentence() {
    let dir = unique_dir("run-nostart");
    let steps = vec![
        Step {
            action: "attach",
            result: attach_fixture(),
            error: None,
            events: vec![busy_event()],
        },
        Step {
            action: "session.send",
            result: json!({ "accepted": true }),
            error: None,
            events: Vec::new(),
        },
        Step {
            action: "session.stop",
            result: json!(true),
            error: None,
            events: Vec::new(),
        },
    ];
    let _server = serve_steps(&dir, steps);

    let mut out = Vec::new();
    let mut err = Vec::new();
    let mut options = openbutler_cli::commands::run::RunOptions::new("never busy");
    options.start_timeout = Duration::from_millis(150);
    let error = openbutler_cli::commands::run::run(&dir, options, &mut out, &mut err)
        .await
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "The background host did not start the one-shot run."
    );
}
