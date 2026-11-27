//! S1 end-to-end pins: the CLI against a contract-shaped fake daemon on
//! the endpoint the client itself derives. The fake speaks the real
//! envelope (protocol types only), so these tests pin dialing,
//! correlation, rendering, and the failure paths without a real daemon.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use openbutler_cli::{
    ActionsCommand, CliError, Command, DaemonCommand, EventsCommand, SessionsCommand,
    WorkspaceCommand, execute,
};
use openbutler_protocol::{
    DAEMON_PROTOCOL_VERSION, DaemonRequest, DaemonResponse, DaemonStatus, FrameDecoder,
    MAX_DAEMON_FRAME_BYTES,
};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

type Responder = Arc<dyn Fn(&DaemonRequest) -> DaemonResponse + Send + Sync>;

/// One process-wide sweep of abandoned scratch trees. A test that panics or is
/// cancelled cannot remove its directory, and gate runs would otherwise pile
/// thousands of `ob-*` trees in the system temp area. Anything older than a day
/// is by definition not a run in progress.
fn sweep_stale_scratch() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else {
            return;
        };
        let cutoff = std::time::SystemTime::now() - std::time::Duration::from_secs(24 * 60 * 60);
        for entry in entries.flatten() {
            if !entry.file_name().to_string_lossy().starts_with("ob-") {
                continue;
            }
            let abandoned = entry
                .metadata()
                .and_then(|meta| meta.modified())
                .map(|stamp| stamp < cutoff)
                .unwrap_or(false);
            if abandoned && entry.path().is_dir() {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
    });
}

fn unique_dir(tag: &str) -> PathBuf {
    sweep_stale_scratch();
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
            web: None,
            pid: 4242,
            started_at: "2026-11-22T01:02:03Z".to_owned(),
            connected_clients: 1,
            schedules: Some(4),
            unresponsive: None,
            entry: Some("dist/index.js".to_owned()),
            build_id: Some(None),
            lines: vec![
                openbutler_protocol::ScreenLine::plain("pid:           4242"),
                openbutler_protocol::ScreenLine::plain("started:       2026-11-22T01:02:03Z"),
                openbutler_protocol::ScreenLine::plain("clients:       1"),
                openbutler_protocol::ScreenLine::plain("schedules:     4"),
                openbutler_protocol::ScreenLine::plain("entry:         dist/index.js"),
                openbutler_protocol::ScreenLine::plain("build:         source"),
            ],
        };
        success(request, serde_json::to_value(&status).unwrap())
    });
    let _server = serve(&dir, responder);

    let text = execute(Command::Status, &dir).await.unwrap();
    assert_eq!(
        text,
        "pid:           4242\n\
         started:       2026-11-22T01:02:03Z\n\
         clients:       1\n\
         schedules:     4\n\
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
        success(
            request,
            json!({
                "sessions": rows.clone(),
                "lines": [
                    { "tone": "plain", "text": "sess-alpha  2026-11-22T03:00:00Z  mock/demo [running]  Alpha chat" },
                    { "tone": "plain", "text": "sess-beta  2026-11-21T22:00:00Z  openai/gpt-x  Beta chat" }
                ]
            }),
        )
    });
    let _server = serve(&dir, responder);

    let text = execute(Command::Sessions { command: None }, &dir)
        .await
        .unwrap();
    assert_eq!(
        text,
        "sess-alpha  2026-11-22T03:00:00Z  mock/demo [running]  Alpha chat\n\
         sess-beta  2026-11-21T22:00:00Z  openai/gpt-x  Beta chat"
    );
}

#[tokio::test]
async fn an_empty_session_list_says_so() {
    let dir = unique_dir("sessions-empty");
    let responder: Responder = Arc::new(|request| {
        success(
            request,
            json!({
                "sessions": [],
                "lines": [{ "tone": "plain", "text": "No sessions." }]
            }),
        )
    });
    let _server = serve(&dir, responder);

    let text = execute(Command::Sessions { command: None }, &dir)
        .await
        .unwrap();
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

    let error = execute(Command::Sessions { command: None }, &dir)
        .await
        .unwrap_err();
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

    let error = execute(Command::Sessions { command: None }, &dir)
        .await
        .unwrap_err();
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
fn serve_steps(dir: &Path, steps: Vec<Step>) -> tokio::task::JoinHandle<()> {
    #[cfg(windows)]
    {
        let name = openbutler_protocol::endpoint::endpoint_for(dir).socket_name();
        let mut options = tokio::net::windows::named_pipe::ServerOptions::new();
        options.first_pipe_instance(true);
        let server = options.create(&name).unwrap();
        tokio::spawn(async move {
            let mut server = server;
            server.connect().await.unwrap();
            steps_loop(&mut server, steps).await;
        })
    }
    #[cfg(not(windows))]
    {
        let path = openbutler_protocol::endpoint::endpoint_for(dir)
            .target()
            .clone();
        let openbutler_protocol::endpoint::Endpoint::UnixSocket(path) = path else {
            panic!("posix endpoint must be a unix socket");
        };
        let _ = std::fs::remove_file(&path);
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            steps_loop(&mut stream, steps).await;
        })
    }
}

/// The scripted answers and event flushes, over whichever endpoint the
/// platform uses.
async fn steps_loop<S>(stream: &mut S, steps: Vec<Step>)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let mut decoder = FrameDecoder::new(MAX_DAEMON_FRAME_BYTES);
    let mut buffer = vec![0u8; 8192];
    let mut index = 0usize;
    loop {
        let read = stream.read(&mut buffer).await.unwrap();
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
            stream.write_all(&bytes).await.unwrap();
            for event in &step.events {
                let mut bytes = serde_json::to_vec(event).unwrap();
                bytes.push(b'\n');
                stream.write_all(&bytes).await.unwrap();
            }
        }
        // The script is the contract: once fulfilled the fake leaves
        // the endpoint, which is how the stop flow observes a host
        // that is actually gone.
        if index == steps.len() {
            break;
        }
    }
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
    use openbutler_protocol::session::{SessionEntriesPatch, SessionPatch, TextDeltaKind};
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
                            kind: TextDeltaKind::Text,
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

/// The same patch with the continuations labelled as reasoning: the one-shot
/// lane must not print them into the answer.
fn reasoning_event(deltas: &[&str]) -> Value {
    use openbutler_protocol::DaemonEvent;
    use openbutler_protocol::session::patch::TextDelta;
    use openbutler_protocol::session::{SessionEntriesPatch, SessionPatch, TextDeltaKind};
    serde_json::to_value(DaemonEvent::Patch {
        version: DAEMON_PROTOCOL_VERSION,
        patch: SessionPatch {
            session: run_meta("sess-run"),
            entries_patch: Some(SessionEntriesPatch::Delta {
                append: Vec::new(),
                replace: Vec::new(),
                append_text: deltas
                    .iter()
                    .map(|delta| TextDelta {
                        index: 1,
                        delta: (*delta).to_owned(),
                        kind: TextDeltaKind::Reasoning,
                    })
                    .collect(),
            }),
            run: run_state(true),
            runtime: run_runtime(),
        },
    })
    .unwrap()
}

/// A patch that appends whole rows: the host sends the answer's opening row
/// this way when it is created in the same publish that stamps the reasoning
/// block.
fn append_event(entries: Vec<openbutler_protocol::SessionEntry>) -> Value {
    use openbutler_protocol::DaemonEvent;
    use openbutler_protocol::session::{SessionEntriesPatch, SessionPatch};
    serde_json::to_value(DaemonEvent::Patch {
        version: DAEMON_PROTOCOL_VERSION,
        patch: SessionPatch {
            session: run_meta("sess-run"),
            entries_patch: Some(SessionEntriesPatch::Delta {
                append: entries,
                replace: Vec::new(),
                append_text: Vec::new(),
            }),
            run: run_state(true),
            runtime: run_runtime(),
        },
    })
    .unwrap()
}

/// One continuation of the row at `index` (the host labels it `text`).
fn grow_event(index: i64, delta: &str) -> Value {
    use openbutler_protocol::DaemonEvent;
    use openbutler_protocol::session::patch::TextDelta;
    use openbutler_protocol::session::{SessionEntriesPatch, SessionPatch, TextDeltaKind};
    serde_json::to_value(DaemonEvent::Patch {
        version: DAEMON_PROTOCOL_VERSION,
        patch: SessionPatch {
            session: run_meta("sess-run"),
            entries_patch: Some(SessionEntriesPatch::Delta {
                append: Vec::new(),
                replace: Vec::new(),
                append_text: vec![TextDelta {
                    index,
                    delta: delta.to_owned(),
                    kind: TextDeltaKind::Text,
                }],
            }),
            run: run_state(true),
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
            events: vec![
                reasoning_event(&["the user", " wants", " a hello"]),
                patch_event(true, &["Hel", "lo"]),
                patch_event(false, &[]),
            ],
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

    assert_eq!(String::from_utf8_lossy(&out), "Hello\n");
    assert!(
        err.is_empty(),
        "stderr stays quiet: {}",
        String::from_utf8_lossy(&err)
    );
}

#[tokio::test]
async fn a_run_prints_an_appended_answer_row_and_its_continuation() {
    let dir = unique_dir("run-append");
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
            events: vec![
                // The answer's opening row arrives whole, after the reasoning
                // row it follows — the shape the host sends when the reasoning
                // block's duration is stamped in the same publish.
                append_event(vec![
                    serde_json::from_value(json!({ "kind": "user", "at": 1, "text": "say hello" }))
                        .unwrap(),
                    serde_json::from_value(json!({
                        "kind": "reasoning", "at": 2, "turnId": "t",
                        "text": "the user wants a hello"
                    }))
                    .unwrap(),
                    serde_json::from_value(
                        json!({ "kind": "assistant", "at": 3, "turnId": "t", "text": "Mer" }),
                    )
                    .unwrap(),
                ]),
                grow_event(2, "haba"),
                patch_event(false, &[]),
            ],
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

    // The appended row and its continuation are one word: a client that only
    // printed continuations would show "haba".
    assert_eq!(String::from_utf8_lossy(&out), "Merhaba\n");
    assert!(
        err.is_empty(),
        "stderr stays quiet: {}",
        String::from_utf8_lossy(&err)
    );
}

#[tokio::test]
async fn a_run_prints_only_the_answer_when_the_turn_uses_a_tool() {
    let dir = unique_dir("run-tool-quiet");
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
            events: vec![
                // A preamble entry, then the tool row and its result: the row
                // belongs to the transcript, not to the one-shot lane's output.
                append_event(vec![
                    serde_json::from_value(json!({
                        "kind": "assistant", "at": 1, "turnId": "t",
                        "text": "I'll check the installed Docker tooling."
                    }))
                    .unwrap(),
                ]),
                append_event(vec![
                    serde_json::from_value(json!({
                        "kind": "tool", "at": 2, "turnId": "t",
                        "tool": "execute_command", "running": false,
                        "result": "/usr/bin/docker\nDocker version 29.8.2"
                    }))
                    .unwrap(),
                ]),
                append_event(vec![
                    serde_json::from_value(json!({
                        "kind": "assistant", "at": 3, "turnId": "t",
                        "text": "Docker is installed."
                    }))
                    .unwrap(),
                ]),
                patch_event(false, &[]),
            ],
        },
    ];
    let _server = serve_steps(&dir, steps);

    let mut out = Vec::new();
    let mut err = Vec::new();
    openbutler_cli::commands::run::run(
        &dir,
        openbutler_cli::commands::run::RunOptions::new("is docker installed?"),
        &mut out,
        &mut err,
    )
    .await
    .unwrap();

    assert_eq!(
        String::from_utf8_lossy(&out),
        "I'll check the installed Docker tooling.\nDocker is installed.\n"
    );
    assert!(
        err.is_empty(),
        "stderr stays quiet: {}",
        String::from_utf8_lossy(&err)
    );
}

#[tokio::test]
async fn a_run_sends_one_message_object_and_surfaces_a_refusal() {
    let dir = unique_dir("run-refusal");
    let responder: Responder = Arc::new(|request: &DaemonRequest| match request.action.as_str() {
        "attach" => success(request, attach_fixture()),
        "session.send" => {
            // The daemon's parser reads one message object; a flat text
            // field is the wrong wire shape and would be refused there.
            assert!(
                request.payload.get("text").is_none(),
                "flat text must not be sent"
            );
            let message = request.payload.get("message").expect("message object");
            assert_eq!(message["text"], "say hello");
            assert_eq!(message["images"], json!([]));
            DaemonResponse::Failure {
                version: DAEMON_PROTOCOL_VERSION,
                id: request.id.clone(),
                ok: false,
                error: "the daemon refused the message".to_owned(),
            }
        }
        other => panic!("unexpected action {other}"),
    });
    let _server = serve(&dir, responder);
    let mut out = Vec::new();
    let mut err = Vec::new();
    let error = openbutler_cli::commands::run::run(
        &dir,
        openbutler_cli::commands::run::RunOptions::new("say hello"),
        &mut out,
        &mut err,
    )
    .await
    .expect_err("a refusal must not look like a start");
    assert!(
        error.to_string().contains("the daemon refused the message"),
        "{error}"
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
    assert_eq!(String::from_utf8_lossy(&out), "started");
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

// ------------------------------------------------- daemon lifecycle (S3)

fn status_fixture(pid: u32) -> Value {
    let status = DaemonStatus {
        pid,
        started_at: "2026-11-22T02:00:00Z".to_owned(),
        connected_clients: 2,
        schedules: None,
        unresponsive: None,
        entry: None,
        web: None,
        build_id: None,
        lines: vec![
            openbutler_protocol::ScreenLine::plain(format!("pid:           {pid}")),
            openbutler_protocol::ScreenLine::plain("started:       2026-11-22T02:00:00Z"),
            openbutler_protocol::ScreenLine::plain("clients:       2"),
        ],
    };
    serde_json::to_value(&status).unwrap()
}

#[tokio::test]
async fn daemon_status_defaults_to_the_status_report() {
    // Bare `daemon` means `status` (the source's default) and both
    // spellings share one render authority: identical text out.
    let responder_for = || -> Responder {
        Arc::new(|request| {
            assert_eq!(request.action, "status");
            success(request, status_fixture(77))
        })
    };
    let bare_dir = unique_dir("daemon-status-bare");
    let _bare_server = serve(&bare_dir, responder_for());
    let bare = execute(Command::Daemon { command: None }, &bare_dir)
        .await
        .unwrap();

    let named_dir = unique_dir("daemon-status-named");
    let _named_server = serve(&named_dir, responder_for());
    let named = execute(
        Command::Daemon {
            command: Some(DaemonCommand::Status),
        },
        &named_dir,
    )
    .await
    .unwrap();

    assert_eq!(bare, named);
    assert!(bare.starts_with("pid:           77"), "{bare}");
}

#[tokio::test]
async fn daemon_stop_asks_the_host_and_waits_for_the_endpoint() {
    let dir = unique_dir("daemon-stop");
    let steps = vec![
        Step {
            action: "status",
            result: status_fixture(9001),
            error: None,
            events: Vec::new(),
        },
        Step {
            action: "stop",
            result: json!({ "stopping": true }),
            error: None,
            events: Vec::new(),
        },
    ];
    // The fake leaves the endpoint after the script, so the stop flow
    // observes a host that is genuinely gone rather than timing out.
    let _server = serve_steps(&dir, steps);

    let text = execute(
        Command::Daemon {
            command: Some(DaemonCommand::Stop),
        },
        &dir,
    )
    .await
    .unwrap();
    assert_eq!(text, "Background host stopped.");
}

#[tokio::test]
async fn daemon_stop_on_a_missing_host_is_a_normal_answer() {
    let dir = unique_dir("daemon-nostop");
    let text = execute(
        Command::Daemon {
            command: Some(DaemonCommand::Stop),
        },
        &dir,
    )
    .await
    .unwrap();
    assert_eq!(text, "Background host is not running.");
}

#[tokio::test]
async fn daemon_start_reports_the_running_host_without_spawning() {
    let dir = unique_dir("daemon-start-active");
    let responder: Responder = Arc::new(|request| {
        assert_eq!(request.action, "status");
        success(request, status_fixture(77))
    });
    let _server = serve(&dir, responder);

    // Any executable: the running host must return before a spawn, so a
    // spawn attempt would fail this test loudly.
    let text = openbutler_cli::commands::daemon::start_with(
        &dir,
        Path::new("definitely-unused.exe"),
        None,
        false,
        false,
    )
    .await
    .unwrap();
    assert_eq!(
        text,
        "The machine custodian starts when a TUI turn arrives.\n\
         Background host running (pid 77).\n\
         It stays alive after this terminal closes. Stop it with `openbutler daemon stop`."
    );
}

/// A real executable that fails on an unexpected argument: the start and
/// run paths only need a program that exits with a code, not a Windows
/// tool specifically.
fn failing_exe() -> PathBuf {
    #[cfg(windows)]
    {
        std::env::var_os("WINDIR")
            .map(|windir| PathBuf::from(windir).join("System32").join("where.exe"))
            .expect("WINDIR is set on Windows")
    }
    #[cfg(not(windows))]
    {
        PathBuf::from("/bin/false")
    }
}

/// A real executable that echoes its arguments, so a flag handed to the
/// child shows up in the log the failure quotes.
fn echoing_exe() -> PathBuf {
    #[cfg(windows)]
    {
        std::env::var_os("WINDIR")
            .map(|windir| PathBuf::from(windir).join("System32").join("findstr.exe"))
            .expect("WINDIR is set on Windows")
    }
    #[cfg(not(windows))]
    {
        PathBuf::from("/bin/echo")
    }
}

#[tokio::test]
async fn daemon_start_diagnoses_a_child_that_died_before_ready() {
    let dir = unique_dir("daemon-start-dead");
    // The stand-in fails at once, so the diagnosis must quote the log and
    // name the exit.
    let error =
        openbutler_cli::commands::daemon::start_with(&dir, &failing_exe(), None, false, false)
            .await
            .expect_err("a dead child must fail the start");
    let message = error.to_string();
    assert!(
        message.starts_with("The host exited before becoming ready (exit code "),
        "{message}"
    );
    assert!(
        !dir.join("daemon.log").metadata().is_err(),
        "the log exists"
    );
}

#[tokio::test]
async fn daemon_start_reports_a_missing_daemon_executable() {
    let dir = unique_dir("daemon-start-missing");
    let missing = dir.join("no-such-daemon.exe");
    let error = openbutler_cli::commands::daemon::start_with(&dir, &missing, None, false, false)
        .await
        .expect_err("a missing executable must fail the start");
    let message = error.to_string();
    assert!(
        message.starts_with("Could not start the background host: "),
        "{message}"
    );
}

/// `daemon start --web` hands `--web` to the spawned host and a plain start
/// does not. `findstr run` stands in for the daemon: it treats every extra
/// argument as a file to open, so `--web` surfaces in the machine log the
/// start failure quotes, while the plain argv reads the null stdin silently.
#[tokio::test]
async fn daemon_start_web_passes_the_flag_to_the_child() {
    let findstr = echoing_exe();

    let with_web = unique_dir("daemon-start-web");
    let message =
        openbutler_cli::commands::daemon::start_with(&with_web, &findstr, None, false, true)
            .await
            .expect_err("the stand-in exits before ready")
            .to_string();
    assert!(message.contains("--web"), "{message}");

    let plain = unique_dir("daemon-start-plain");
    let message =
        openbutler_cli::commands::daemon::start_with(&plain, &findstr, None, false, false)
            .await
            .expect_err("the stand-in exits before ready")
            .to_string();
    assert!(!message.contains("--web"), "{message}");
}

// ------------------------------------------------- machine log (S3)

#[tokio::test]
async fn logs_prints_the_host_lines() {
    let dir = unique_dir("logs-tail");
    let seen = Arc::new(std::sync::Mutex::new(None));
    let responder: Responder = {
        let seen = seen.clone();
        Arc::new(
            move |request: &DaemonRequest| match request.action.as_str() {
                "logs.tail" => {
                    *seen.lock().unwrap() = Some(Value::Object(request.payload.clone()));
                    success(
                        request,
                        json!({
                            "lines": [
                                { "tone": "plain", "text": "three" },
                                { "tone": "plain", "text": "four" }
                            ]
                        }),
                    )
                }
                other => panic!("unexpected action {other}"),
            },
        )
    };
    let _server = serve(&dir, responder);

    let text = execute(
        Command::Logs {
            lines: 2,
            file: None,
            level: None,
            event: None,
            json: false,
        },
        &dir,
    )
    .await
    .unwrap();
    assert_eq!(text, "three\nfour");
    assert_eq!(seen.lock().unwrap().clone(), Some(json!({ "lines": 2 })));
}

#[tokio::test]
async fn logs_forwards_an_absolute_file_without_a_second_decision() {
    let dir = unique_dir("logs-file");
    let seen = Arc::new(std::sync::Mutex::new(None));
    let responder: Responder = {
        let seen = seen.clone();
        Arc::new(
            move |request: &DaemonRequest| match request.action.as_str() {
                "logs.tail" => {
                    *seen.lock().unwrap() = Some(Value::Object(request.payload.clone()));
                    success(
                        request,
                        json!({
                            "lines": [
                                { "tone": "info", "text": "No log lines at C:\\elsewhere\\a.log." }
                            ]
                        }),
                    )
                }
                other => panic!("unexpected action {other}"),
            },
        )
    };
    let _server = serve(&dir, responder);

    let text = execute(
        Command::Logs {
            lines: 100,
            file: Some(std::path::PathBuf::from("relative.log")),
            level: None,
            event: None,
            json: false,
        },
        &dir,
    )
    .await
    .unwrap();
    assert_eq!(text, "\u{b7} No log lines at C:\\elsewhere\\a.log.");
    let payload = seen.lock().unwrap().clone().unwrap();
    let forwarded = payload["file"].as_str().unwrap().to_owned();
    assert!(
        std::path::Path::new(&forwarded).is_absolute(),
        "the caller's directory is the base: {forwarded}"
    );
    assert!(forwarded.ends_with("relative.log"), "{forwarded}");
}

#[tokio::test]
async fn logs_surfaces_the_hosts_refusal() {
    let dir = unique_dir("logs-refusal");
    let responder: Responder = Arc::new(|request: &DaemonRequest| match request.action.as_str() {
        "logs.tail" => DaemonResponse::Failure {
            version: DAEMON_PROTOCOL_VERSION,
            id: request.id.clone(),
            ok: false,
            error: "Log files are read on the host machine only.".to_owned(),
        },
        other => panic!("unexpected action {other}"),
    });
    let _server = serve(&dir, responder);

    let error = execute(
        Command::Logs {
            lines: 100,
            file: None,
            level: None,
            event: None,
            json: false,
        },
        &dir,
    )
    .await
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        "Log files are read on the host machine only."
    );
}

#[test]
fn logs_lines_range_is_clap_usage() {
    use clap::Parser;
    // The failure contract: argument problems are clap's own message
    // (exit 2 in main), never a rendered log answer.
    let error = openbutler_cli::Cli::try_parse_from(["openbutler", "logs", "--lines", "0"])
        .expect_err("--lines 0 must not parse");
    let text = error.to_string();
    assert!(text.contains("invalid value '0' for '--lines"), "{text}");
}

// ------------------------------------------------- usage report (S3)

/// A canned `usage.report` answer — the daemon's shape, lines included.
fn usage_report(label: &str, days: Value, summary: Value, lines: Vec<&str>) -> Value {
    json!({
        "label": label,
        "summary": summary,
        "days": days,
        "lines": lines,
    })
}

#[tokio::test]
async fn usage_prints_the_host_report_line_by_line() {
    let dir = unique_dir("usage-report");
    let responder: Responder = Arc::new(|request| {
        assert_eq!(request.action, "usage.report");
        // The window and the scope are forwarded verbatim; nothing is
        // recomputed here.
        assert_eq!(request.payload["days"], 7);
        assert_eq!(request.payload["session"], "sess-alpha");
        assert_eq!(request.payload["model"], "mock/demo");
        success(
            request,
            usage_report(
                "last 7 days (session sess-alp, model mock/demo)",
                json!([]),
                json!({ "requests": 2 }),
                vec![
                    "Usage last 7 days (session sess-alp, model mock/demo)",
                    "  requests 2  input 2.6k  cache read 400  cache write 0  output 500  reasoning 50  cost $0.0579",
                    "  busiest 2026-11-20 (1 requests)  longest task 2s  streak 1d",
                    "  cost source: provider for every request",
                    "",
                    "day         reqs    input    cache   output       cost",
                    "2026-11-20     1      600      400      200    $0.0123",
                    "",
                    "token shape  ▄█",
                ],
            ),
        )
    });
    let _server = serve(&dir, responder);
    let text = execute(
        Command::Usage {
            days: 7,
            session: Some("sess-alpha".to_owned()),
            model: Some("mock/demo".to_owned()),
            json: false,
        },
        &dir,
    )
    .await
    .unwrap();
    // Line for line — the CLI adds nothing and drops nothing.
    assert_eq!(
        text,
        "Usage last 7 days (session sess-alp, model mock/demo)\n  \
         requests 2  input 2.6k  cache read 400  cache write 0  output 500  reasoning 50  cost $0.0579\n  \
         busiest 2026-11-20 (1 requests)  longest task 2s  streak 1d\n  \
         cost source: provider for every request\n\
         \n\
         day         reqs    input    cache   output       cost\n\
         2026-11-20     1      600      400      200    $0.0123\n\
         \n\
         token shape  ▄█"
    );
}

#[tokio::test]
async fn usage_without_rows_says_so() {
    let dir = unique_dir("usage-empty");
    let responder: Responder = Arc::new(|request| {
        // Absent filters stay absent on the wire (no empty strings).
        assert!(request.payload.get("session").is_none());
        assert!(request.payload.get("model").is_none());
        success(
            request,
            usage_report(
                "last 30 days",
                json!([]),
                json!({ "days": 0, "requests": 0, "streakDays": 0, "busiest": null }),
                vec!["No usage recorded last 30 days."],
            ),
        )
    });
    let _server = serve(&dir, responder);
    let text = execute(
        Command::Usage {
            days: 30,
            session: None,
            model: None,
            json: false,
        },
        &dir,
    )
    .await
    .unwrap();
    assert_eq!(text, "No usage recorded last 30 days.");
}

#[tokio::test]
async fn usage_json_prints_the_host_envelope() {
    let dir = unique_dir("usage-json");
    let responder: Responder = Arc::new(|request| {
        assert_eq!(request.payload["days"], 7);
        success(
            request,
            usage_report(
                "last 7 days",
                json!([]),
                json!({ "days": 0, "streakDays": 0, "busiest": null }),
                vec!["No usage recorded last 7 days."],
            ),
        )
    });
    let _server = serve(&dir, responder);
    let text = execute(
        Command::Usage {
            days: 7,
            session: None,
            model: None,
            json: true,
        },
        &dir,
    )
    .await
    .unwrap();
    let parsed: Value = serde_json::from_str(&text).expect("valid JSON");
    assert_eq!(parsed["label"], "last 7 days");
    assert_eq!(parsed["summary"]["days"], 0);
    assert_eq!(parsed["summary"]["streakDays"], 0);
    assert!(parsed["summary"]["busiest"].is_null());
    assert!(
        parsed["days"]
            .as_array()
            .is_some_and(|days| days.is_empty())
    );
    // The machine envelope carries no rendering.
    assert!(parsed.get("lines").is_none());
}

#[tokio::test]
async fn usage_refusal_is_the_daemon_sentence() {
    let dir = unique_dir("usage-refusal");
    let responder: Responder = Arc::new(|request| DaemonResponse::Failure {
        version: DAEMON_PROTOCOL_VERSION,
        id: request.id.clone(),
        ok: false,
        error: "days must be a whole number between 1 and 3650.".to_owned(),
    });
    let _server = serve(&dir, responder);
    let error = execute(
        Command::Usage {
            days: 30,
            session: None,
            model: None,
            json: false,
        },
        &dir,
    )
    .await
    .expect_err("a refusal is an error");
    let CliError::Answer(sentence) = error else {
        panic!("expected the daemon's own sentence, got {error:?}");
    };
    assert_eq!(sentence, "days must be a whole number between 1 and 3650.");
}

// --- setup ----------------------------------------------------------------

#[test]
fn setup_refuses_without_a_terminal() {
    let dir = unique_dir("setup-guard");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_openbutler"))
        .arg("setup")
        .env("OPENBUTLER_DATA_DIR", &dir)
        .stdin(std::process::Stdio::piped())
        .output()
        .expect("spawn the binary");
    assert_eq!(output.status.code(), Some(1), "a refusal is exit class 1");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(openbutler_cli::commands::setup::TTY_REFUSAL),
        "stderr must carry the guard sentence, got: {stderr}"
    );
}

#[tokio::test]
async fn setup_save_posts_the_payload_and_names_the_file() {
    let dir = unique_dir("setup-save");
    let responder: Responder = Arc::new(|request| {
        assert_eq!(request.action, "settings.setup");
        assert_eq!(request.payload["providers"][0]["id"], "mock");
        assert_eq!(request.payload["providers"][0]["keys"][0], "sk-x");
        assert_eq!(request.payload["providers"][0]["url"], "http://h");
        assert_eq!(request.payload["defaultProvider"], "mock");
        assert_eq!(request.payload["defaultModel"], "mock/m");
        success(
            request,
            json!({
                "settingsFile": "/ob/data/settings.yaml",
                "configFile": "/ob/data/config.yaml",
            }),
        )
    });
    let _server = serve(&dir, responder);

    let file = openbutler_cli::commands::setup::save(
        &dir,
        json!({
            "providers": [{ "id": "mock", "keys": ["sk-x"], "url": "http://h" }],
            "defaultProvider": "mock",
            "defaultModel": "mock/m",
        }),
    )
    .await
    .expect("the fake daemon answers");
    assert_eq!(file, "/ob/data/settings.yaml");
}

#[tokio::test]
async fn config_prints_the_hosts_screen() {
    let dir = unique_dir("config");
    let responder: Responder = Arc::new(|request| {
        assert_eq!(request.action, "config.report");
        success(
            request,
            json!({
                "lines": [
                    { "tone": "plain", "text": "" },
                    { "tone": "plain", "text": "\u{2500}\u{2500} MCP servers \u{2500}\u{2500}\u{2500}\u{2500}" },
                    { "tone": "info", "text": "No MCP servers configured." },
                    { "tone": "plain", "text": "  Tool access            Workspace (default)" },
                    { "tone": "success", "text": "Ready to run." }
                ]
            }),
        )
    });
    let _server = serve(&dir, responder);

    let text = execute(Command::Config, &dir).await.unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], "");
    assert!(lines[1].starts_with("\u{2500}\u{2500} MCP servers "));
    assert_eq!(lines[2], "\u{b7} No MCP servers configured.");
    assert_eq!(lines[3], "  Tool access            Workspace (default)");
    assert_eq!(lines[4], "\u{2714} Ready to run.");
}

#[tokio::test]
async fn config_surfaces_the_daemons_refusal_sentence() {
    let dir = unique_dir("config-refusal");
    let responder: Responder = Arc::new(|request| DaemonResponse::Failure {
        version: DAEMON_PROTOCOL_VERSION,
        id: request.id.clone(),
        ok: false,
        error: "settings are not loaded".to_owned(),
    });
    let _server = serve(&dir, responder);

    let error = execute(Command::Config, &dir).await.unwrap_err();
    assert!(matches!(error, CliError::Answer(_)));
    assert_eq!(error.to_string(), "settings are not loaded");
}

/// A host that is already up: the preference is persisted through the
/// daemon and the source's two lines follow the block.
#[tokio::test]
async fn daemon_start_persists_the_listener_through_the_daemon() {
    let dir = unique_dir("start-listen");
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let recorder = Arc::clone(&seen);
    let responder: Responder = Arc::new(move |request| match request.action.as_str() {
        "status" => success(
            request,
            json!({
                "pid": 9,
                "startedAt": "2026-11-23T20:00:00Z",
                "workspaceCount": 1,
                "connectedClients": 1,
            }),
        ),
        "settings.set-listen" => {
            recorder.lock().unwrap().push(request.payload.clone());
            success(request, json!({ "listen": "100.64.0.5:45280" }))
        }
        other => panic!("unexpected action {other}"),
    });
    let _server = serve(&dir, responder);

    let text = openbutler_cli::commands::daemon::start_with(
        &dir,
        Path::new("definitely-unused.exe"),
        Some("100.64.0.5:45280".to_owned()),
        false,
        false,
    )
    .await
    .unwrap();
    let expected =
        openbutler_cli::ui::kv("Listener", "100.64.0.5:45280 (remote clients attach here)");
    assert!(text.contains(&expected), "{text}");
    assert!(
        text.contains("The saved listener applies on the next `openbutler daemon restart`."),
        "{text}"
    );
    let payloads = seen.lock().unwrap();
    assert_eq!(payloads.len(), 1, "one write per start");
    assert_eq!(payloads[0]["listen"], "100.64.0.5:45280");
}

/// `--no-listen` clears the preference through the same action and says so
/// in the source's words.
#[tokio::test]
async fn daemon_start_no_listen_clears_the_preference() {
    let dir = unique_dir("start-nolisten");
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let recorder = Arc::clone(&seen);
    let responder: Responder = Arc::new(move |request| match request.action.as_str() {
        "status" => success(
            request,
            json!({
                "pid": 9,
                "startedAt": "2026-11-23T20:00:00Z",
                "workspaceCount": 1,
                "connectedClients": 1,
            }),
        ),
        "settings.set-listen" => {
            recorder.lock().unwrap().push(request.payload.clone());
            success(request, json!({ "listen": null }))
        }
        other => panic!("unexpected action {other}"),
    });
    let _server = serve(&dir, responder);

    let text = openbutler_cli::commands::daemon::start_with(
        &dir,
        Path::new("definitely-unused.exe"),
        None,
        true,
        false,
    )
    .await
    .unwrap();
    assert!(
        text.contains("Remote listener cleared; this host binds no listener."),
        "{text}"
    );
    assert!(
        !text.contains("Listener "),
        "a cleared listener prints no listener line: {text}"
    );
    let payloads = seen.lock().unwrap();
    assert_eq!(payloads.len(), 1);
    assert!(payloads[0]["listen"].is_null());
}

/// `openbutler tui` refuses without a terminal before touching one, and the
/// refusal is the command's own sentence (no half-open alternate screen).
#[tokio::test]
async fn tui_refuses_without_a_terminal() {
    let dir = unique_dir("tui-guard");
    let error = execute(Command::Tui, &dir).await.unwrap_err();
    match error {
        CliError::Answer(message) => {
            assert_eq!(message, openbutler_cli::tui::TTY_REFUSAL);
        }
        other => panic!("expected the terminal refusal, got {other:?}"),
    }
}
// ------------------------------------------------------ daemon restart (S3)

#[tokio::test]
async fn daemon_restart_stops_with_the_restart_flag_then_starts_again() {
    let dir = unique_dir("daemon-restart");
    let seen = Arc::new(std::sync::Mutex::new(None));
    let responder: Responder = {
        let seen = seen.clone();
        Arc::new(
            move |request: &DaemonRequest| match request.action.as_str() {
                "status" => success(request, status_fixture(4242)),
                "stop" => {
                    *seen.lock().unwrap() = Some(Value::Object(request.payload.clone()));
                    success(request, json!({ "stopping": true }))
                }
                other => panic!("unexpected action {other}"),
            },
        )
    };
    let _server = serve(&dir, responder);

    // The spawn half needs a real host, so it fails on a missing executable;
    // reaching that error proves the stop half ran to completion first.
    let missing = dir.join("no-such-daemon.exe");
    let error = openbutler_cli::commands::daemon::restart_with(&dir, &missing)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("Could not start"), "{error}");

    let payload = seen
        .lock()
        .unwrap()
        .clone()
        .expect("the stop ask must arrive");
    assert_eq!(
        payload,
        json!({ "restart": true }),
        "the restart flow must carry the source's restart flag"
    );
}

#[tokio::test]
async fn daemon_restart_tolerates_a_host_that_is_not_running() {
    let dir = unique_dir("daemon-restart-idle");
    let missing = dir.join("no-such-daemon.exe");
    let error = openbutler_cli::commands::daemon::restart_with(&dir, &missing)
        .await
        .unwrap_err();
    // Not "cannot connect": with nothing to recycle the flow goes straight
    // to the start half.
    assert!(error.to_string().contains("Could not start"), "{error}");
}

// ------------------------------------------------ top-level stop/restart

#[tokio::test]
async fn top_level_stop_and_restart_are_the_daemon_spellings() {
    // The short spellings the source help listed; both must exist and the
    // stop half must be the same flow, sentence included.
    use clap::Parser;
    let stop = openbutler_cli::Cli::try_parse_from(["openbutler", "stop"]).unwrap();
    assert!(matches!(stop.command, Some(Command::Stop)));
    let restart = openbutler_cli::Cli::try_parse_from(["openbutler", "restart"]).unwrap();
    assert!(matches!(restart.command, Some(Command::Restart)));

    let dir = unique_dir("top-level-stop");
    let steps = vec![
        Step {
            action: "status",
            result: status_fixture(9101),
            error: None,
            events: Vec::new(),
        },
        Step {
            action: "stop",
            result: json!({ "stopping": true }),
            error: None,
            events: Vec::new(),
        },
    ];
    let _server = serve_steps(&dir, steps);

    let text = execute(Command::Stop, &dir).await.unwrap();
    assert_eq!(text, "Background host stopped.");
}

// --------------------------------------------------------- daemon run (S3)

#[tokio::test]
async fn daemon_run_hands_back_the_child_status() {
    // A real executable that exits 1 for an unexpected argument: the point
    // is that the foreground run returns the child's code, not an error.
    let exe = failing_exe();
    let dir = unique_dir("daemon-run");
    let code = openbutler_cli::commands::daemon::run_foreground_with(&dir, &exe, None, false)
        .await
        .unwrap();
    assert_eq!(code, 1);
}

#[tokio::test]
async fn daemon_run_reports_a_missing_executable() {
    let dir = unique_dir("daemon-run-missing");
    let missing = dir.join("no-such-daemon.exe");
    let error = openbutler_cli::commands::daemon::run_foreground_with(&dir, &missing, None, false)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("Could not start"), "{error}");
}
// ------------------------------------------------------ list-models (P4)

#[tokio::test]
async fn list_models_prints_the_hosts_lines_and_carries_the_flags() {
    use clap::Parser;
    // The source kept these as top-level flags (cli-main.ts L140): the
    // parse must accept them without a subcommand.
    let cli = openbutler_cli::Cli::try_parse_from([
        "openbutler",
        "--list-models",
        "--filter",
        "tools",
        "--limit",
        "5",
        "--provider",
        "mock",
    ])
    .expect("the flag set parses");
    assert!(cli.list_models);
    assert_eq!(cli.filter.as_deref(), Some("tools"));
    assert_eq!(cli.limit, Some(5));
    assert_eq!(cli.provider.as_deref(), Some("mock"));

    let dir = unique_dir("list-models");
    let seen: Arc<std::sync::Mutex<Option<Value>>> = Arc::new(std::sync::Mutex::new(None));
    let captured = seen.clone();
    let responder: Responder = Arc::new(move |request: &DaemonRequest| {
        match request.action.as_str() {
            "catalog.listToolModels" => {
                *captured.lock().unwrap() = Some(Value::Object(request.payload.clone()));
                success(
                    request,
                    json!({
                        "lines": [
                            { "tone": "plain", "text": "\u{2500}\u{2500} Models (1) \u{2500}\u{2500}\u{2500}" },
                            { "tone": "plain", "text": "  PROVIDER  MODEL  NAME" },
                            { "tone": "info", "text": "2 models match; showing 1." }
                        ],
                        "total": 2,
                        "shown": 1,
                        "limit": 1
                    }),
                )
            }
            other => panic!("unexpected action {other}"),
        }
    });
    let _server = serve(&dir, responder);

    let text = execute(
        Command::ListModels {
            provider: Some("mock".to_owned()),
            filter: Some("tools".to_owned()),
            limit: Some(1),
        },
        &dir,
    )
    .await
    .unwrap();
    // Byte-exact: the client applies the tones and adds nothing.
    assert_eq!(
        text,
        "\u{2500}\u{2500} Models (1) \u{2500}\u{2500}\u{2500}\n  PROVIDER  MODEL  NAME\n\u{b7} 2 models match; showing 1."
    );
    assert_eq!(
        seen.lock().unwrap().clone(),
        Some(json!({ "provider": "mock", "filter": "tools", "limit": 1 }))
    );
}

// --------------------------------------------------- permissions (S3)

fn permissions_command(
    mode: Option<&str>,
    review_timeout: Option<u64>,
    approval_timeout: Option<u64>,
    review_model: Option<&str>,
) -> Command {
    Command::Permissions {
        mode: mode.map(str::to_owned),
        review_timeout,
        approval_timeout,
        review_model: review_model.map(str::to_owned),
    }
}

fn permissions_defaults() -> Value {
    json!({
        "lines": [
            { "tone": "plain", "text": "  Current mode           Workspace (default)" },
            { "tone": "plain", "text": "  Auto-review timeout    15 seconds (default)" },
            { "tone": "plain", "text": "  Approval timeout       120 seconds (default)" },
            { "tone": "plain", "text": "  Auto-review model      (main model)" },
            { "tone": "info", "text": "Workspace paths run without prompts. Auto-review asks the selected model about actions outside the workspace." },
            { "tone": "info", "text": "Use `openbutler permissions full-access` to disable tool permission prompts." }
        ]
    })
}

#[tokio::test]
async fn permissions_renders_the_four_values_and_the_two_explanations() {
    let dir = unique_dir("permissions-report");
    let seen = Arc::new(std::sync::Mutex::new(None));
    let responder: Responder = {
        let seen = seen.clone();
        Arc::new(move |request: &DaemonRequest| {
            *seen.lock().unwrap() = Some(Value::Object(request.payload.clone()));
            success(request, permissions_defaults())
        })
    };
    let _server = serve(&dir, responder);

    let text = execute(permissions_command(None, None, None, None), &dir)
        .await
        .unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0][2..25].trim_end(), "Current mode");
    assert_eq!(&lines[0][25..], "Workspace (default)");
    assert_eq!(lines[1][2..25].trim_end(), "Auto-review timeout");
    assert_eq!(&lines[1][25..], "15 seconds (default)");
    assert_eq!(lines[2][2..25].trim_end(), "Approval timeout");
    assert_eq!(&lines[2][25..], "120 seconds (default)");
    assert_eq!(lines[3][2..25].trim_end(), "Auto-review model");
    assert_eq!(&lines[3][25..], "(main model)");
    assert_eq!(
        lines[4],
        "\u{b7} Workspace paths run without prompts. Auto-review asks the selected model about actions outside the workspace."
    );
    assert_eq!(
        lines[5],
        "\u{b7} Use `openbutler permissions full-access` to disable tool permission prompts."
    );
    assert_eq!(lines.len(), 6, "a bare call prints the two info lines only");
    assert_eq!(
        seen.lock().unwrap().clone(),
        Some(json!({})),
        "a bare call carries no fields"
    );
}

#[tokio::test]
async fn permissions_sends_every_flag_and_prints_the_success_lines() {
    let dir = unique_dir("permissions-set");
    let seen = Arc::new(std::sync::Mutex::new(None));
    let responder: Responder = {
        let seen = seen.clone();
        Arc::new(move |request: &DaemonRequest| {
            *seen.lock().unwrap() = Some(Value::Object(request.payload.clone()));
            success(
                request,
                json!({
                    "lines": [
                        { "tone": "plain", "text": "  Current mode           Approve for me (Auto-review outside workspace)" },
                        { "tone": "plain", "text": "  Auto-review timeout    45 seconds" },
                        { "tone": "plain", "text": "  Approval timeout       30 seconds" },
                        { "tone": "plain", "text": "  Auto-review model      mock/model-x" },
                        { "tone": "info", "text": "Workspace paths run without prompts. Auto-review asks the selected model about actions outside the workspace." },
                        { "tone": "info", "text": "Use `openbutler permissions full-access` to disable tool permission prompts." },
                        { "tone": "success", "text": "Saved Approve for me (Auto-review outside workspace) mode." },
                        { "tone": "success", "text": "Auto-review timeout set to 45 seconds." },
                        { "tone": "success", "text": "Approval timeout set to 30 seconds." },
                        { "tone": "success", "text": "Auto-review model set to mock/model-x." },
                        { "tone": "info", "text": "Restart the host for the reviewer model to take effect: openbutler daemon restart" }
                    ]
                }),
            )
        })
    };
    let _server = serve(&dir, responder);

    let text = execute(
        permissions_command(
            Some("auto-review"),
            Some(45),
            Some(30),
            Some("mock/model-x"),
        ),
        &dir,
    )
    .await
    .unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        seen.lock().unwrap().clone(),
        Some(json!({
            "mode": "auto-review",
            "reviewTimeoutSeconds": 45,
            "approvalTimeoutSeconds": 30,
            "reviewModel": "mock/model-x"
        }))
    );
    assert_eq!(
        &lines[0][25..],
        "Approve for me (Auto-review outside workspace)"
    );
    assert_eq!(
        lines[6],
        "\u{2714} Saved Approve for me (Auto-review outside workspace) mode."
    );
    assert_eq!(lines[7], "\u{2714} Auto-review timeout set to 45 seconds.");
    assert_eq!(lines[8], "\u{2714} Approval timeout set to 30 seconds.");
    assert_eq!(lines[9], "\u{2714} Auto-review model set to mock/model-x.");
    assert_eq!(
        lines[10],
        "\u{b7} Restart the host for the reviewer model to take effect: openbutler daemon restart"
    );
}

#[tokio::test]
async fn permissions_surfaces_the_daemons_refusal() {
    let dir = unique_dir("permissions-refusal");
    let responder: Responder = Arc::new(|request: &DaemonRequest| DaemonResponse::Failure {
        version: DAEMON_PROTOCOL_VERSION,
        id: request.id.clone(),
        ok: false,
        error: "--review-timeout must be a whole number of seconds between 5 and 600.".to_owned(),
    });
    let _server = serve(&dir, responder);

    let error = execute(permissions_command(None, Some(4), None, None), &dir)
        .await
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "--review-timeout must be a whole number of seconds between 5 and 600."
    );
}
// ------------------------------------------------------- events/actions (S3)

#[tokio::test]
async fn events_lists_the_daemons_rows_as_pretty_json() {
    let dir = unique_dir("events-list");
    let responder: Responder = Arc::new(|request: &DaemonRequest| match request.action.as_str() {
        "ledger.events" => success(
            request,
            json!([{ "eventId": "e1", "source": "timer", "status": "delivered" }]),
        ),
        other => panic!("unexpected action {other}"),
    });
    let _server = serve(&dir, responder);

    let text = execute(Command::Events { command: None }, &dir)
        .await
        .unwrap();
    // JSON.stringify(rows, null, 2): two-space indentation, key order kept.
    assert_eq!(
        text,
        "[\n  {\n    \"eventId\": \"e1\",\n    \"source\": \"timer\",\n    \"status\": \"delivered\"\n  }\n]"
    );
}

#[tokio::test]
async fn events_retry_sends_the_id_and_prints_the_answer() {
    let dir = unique_dir("events-retry");
    let seen = Arc::new(std::sync::Mutex::new(None));
    let responder: Responder = {
        let seen = seen.clone();
        Arc::new(
            move |request: &DaemonRequest| match request.action.as_str() {
                "events.retry" => {
                    *seen.lock().unwrap() = Some(Value::Object(request.payload.clone()));
                    success(request, json!({ "status": "pending" }))
                }
                other => panic!("unexpected action {other}"),
            },
        )
    };
    let _server = serve(&dir, responder);

    let text = execute(
        Command::Events {
            command: Some(EventsCommand::Retry {
                event_id: "e1".to_owned(),
            }),
        },
        &dir,
    )
    .await
    .unwrap();
    assert_eq!(text, "{\n  \"status\": \"pending\"\n}");
    assert_eq!(
        seen.lock().unwrap().clone(),
        Some(json!({ "eventId": "e1" }))
    );
}

#[tokio::test]
async fn prune_previews_with_the_source_lines() {
    let dir = unique_dir("events-prune-preview");
    let seen = Arc::new(std::sync::Mutex::new(None));
    let responder: Responder = {
        let seen = seen.clone();
        Arc::new(
            move |request: &DaemonRequest| match request.action.as_str() {
                "ledger.prune" => {
                    *seen.lock().unwrap() = Some(Value::Object(request.payload.clone()));
                    success(
                        request,
                        json!({ "events": 2, "actions": 1, "transitions": 3, "lines": [
                            { "tone": "plain", "text": "  2 event(s), 1 action(s), 3 transition(s)" },
                            { "tone": "info", "text": "Nothing was deleted; re-run with --yes to delete these 6 record(s)." }
                        ] }),
                    )
                }
                other => panic!("unexpected action {other}"),
            },
        )
    };
    let _server = serve(&dir, responder);

    let preview = execute(
        Command::Events {
            command: Some(EventsCommand::Prune {
                older_than: "30d".to_owned(),
                yes: false,
            }),
        },
        &dir,
    )
    .await
    .unwrap();
    let lines: Vec<&str> = preview.lines().collect();
    assert_eq!(lines[0], "  2 event(s), 1 action(s), 3 transition(s)");
    assert_eq!(
        lines[1],
        "\u{b7} Nothing was deleted; re-run with --yes to delete these 6 record(s)."
    );
    assert_eq!(
        seen.lock().unwrap().clone(),
        Some(json!({ "kind": "events", "olderThan": "30d", "apply": false }))
    );
}

#[tokio::test]
async fn prune_applies_with_the_success_line() {
    let dir = unique_dir("events-prune-apply");
    let seen = Arc::new(std::sync::Mutex::new(None));
    let responder: Responder = {
        let seen = seen.clone();
        Arc::new(
            move |request: &DaemonRequest| match request.action.as_str() {
                "ledger.prune" => {
                    *seen.lock().unwrap() = Some(Value::Object(request.payload.clone()));
                    success(
                        request,
                        json!({ "events": 0, "actions": 4, "transitions": 2, "lines": [
                            { "tone": "plain", "text": "  0 event(s), 4 action(s), 2 transition(s)" },
                            { "tone": "success", "text": "Pruned 6 record(s)." }
                        ] }),
                    )
                }
                other => panic!("unexpected action {other}"),
            },
        )
    };
    let _server = serve(&dir, responder);

    let applied = execute(
        Command::Actions {
            command: Some(ActionsCommand::Prune {
                older_than: "30d".to_owned(),
                yes: true,
            }),
        },
        &dir,
    )
    .await
    .unwrap();
    let lines: Vec<&str> = applied.lines().collect();
    assert_eq!(lines[0], "  0 event(s), 4 action(s), 2 transition(s)");
    assert_eq!(lines[1], "\u{2714} Pruned 6 record(s).");
    assert_eq!(
        seen.lock().unwrap().clone(),
        Some(json!({ "kind": "actions", "olderThan": "30d", "apply": true }))
    );
}

#[tokio::test]
async fn prune_with_nothing_to_delete_says_so() {
    let dir = unique_dir("events-prune-empty");
    let responder: Responder = Arc::new(|request: &DaemonRequest| match request.action.as_str() {
        "ledger.prune" => success(
            request,
            json!({ "events": 0, "actions": 0, "transitions": 0, "lines": [
                { "tone": "info", "text": "No actions records are older than 12h; nothing to prune." }
            ] }),
        ),
        other => panic!("unexpected action {other}"),
    });
    let _server = serve(&dir, responder);

    let text = execute(
        Command::Actions {
            command: Some(ActionsCommand::Prune {
                older_than: "12h".to_owned(),
                yes: false,
            }),
        },
        &dir,
    )
    .await
    .unwrap();
    assert_eq!(
        text,
        "\u{b7} No actions records are older than 12h; nothing to prune."
    );
}

// ---------------------------------------------------------------- about

#[tokio::test]
async fn about_prints_the_hosts_screen() {
    let dir = unique_dir("about");
    let responder: Responder = Arc::new(|request| {
        assert_eq!(request.action, "about.report");
        success(
            request,
            json!({
                "lines": [
                    { "tone": "plain", "text": "" },
                    { "tone": "plain", "text": "\u{2500}\u{2500} OpenButler \u{2500}\u{2500}\u{2500}\u{2500}" },
                    { "tone": "plain", "text": "  Version                0.0.9" },
                    { "tone": "plain", "text": "  Host                   0.0.9 (pid 4242)" },
                    { "tone": "plain", "text": "" }
                ]
            }),
        )
    });
    let _server = serve(&dir, responder);

    let text = execute(Command::About, &dir).await.unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        lines[0], "",
        "the screen opens with the section's blank line"
    );
    assert!(lines[1].starts_with("\u{2500}\u{2500} OpenButler "));
    assert_eq!(lines[2], "  Version                0.0.9");
    assert_eq!(lines[3], "  Host                   0.0.9 (pid 4242)");
    // The host's closing blank line: the rendered text ends with its newline.
    assert!(text.ends_with('\n'), "the screen ends blank: {text:?}");
}

// -------------------------------------------------------------- workspace

#[tokio::test]
async fn workspace_default_shows_the_stored_root_or_the_none_sentence() {
    let dir = unique_dir("workspace-none");
    let responder: Responder = Arc::new(|request| {
        assert_eq!(request.action, "settings.set-default-workspace");
        // A bare query carries no path at all.
        assert!(request.payload.get("path").is_none());
        success(
            request,
            json!({
                "workspace": null,
                "lines": [
                    { "tone": "plain", "text": format!("  {:<22} {}", "Default workspace", "(none; bare OpenButler uses the launch directory)") }
                ]
            }),
        )
    });
    let _server = serve(&dir, responder);

    let text = execute(
        Command::Workspace {
            command: WorkspaceCommand::Default { path: None },
        },
        &dir,
    )
    .await
    .unwrap();
    assert_eq!(
        text,
        format!(
            "  {:<22} {}",
            "Default workspace", "(none; bare OpenButler uses the launch directory)"
        )
    );
}

#[tokio::test]
async fn workspace_default_sends_the_path_and_names_the_saved_root() {
    let dir = unique_dir("workspace-set");
    let responder: Responder = Arc::new(|request| {
        assert_eq!(request.action, "settings.set-default-workspace");
        assert_eq!(request.payload["path"], "C:\\work\\proj");
        success(
            request,
            json!({
                "workspace": "C:\\work\\proj",
                "lines": [
                    {
                        "tone": "success",
                        "text": "Bare OpenButler launches will use C:\\work\\proj."
                    }
                ]
            }),
        )
    });
    let _server = serve(&dir, responder);

    let text = execute(
        Command::Workspace {
            command: WorkspaceCommand::Default {
                path: Some("C:\\work\\proj".to_owned()),
            },
        },
        &dir,
    )
    .await
    .unwrap();
    assert_eq!(
        text,
        "\u{2714} Bare OpenButler launches will use C:\\work\\proj."
    );
}

// ----------------------------------------------------------------- skills

#[tokio::test]
async fn skills_renders_the_host_table() {
    let dir = unique_dir("skills-table");
    let responder: Responder = Arc::new(|request| {
        assert_eq!(request.action, "skills.list");
        assert_eq!(request.payload["workspace"], "C:\\work\\proj");
        success(
            request,
            json!({
                "workspace": "C:\\work\\proj",
                "skills": [
                    {
                        "name": "alpha", "scope": "workspace",
                        "description": "A workspace skill.",
                        "filePath": "C:\\work\\proj\\.agents\\skills\\alpha\\SKILL.md"
                    },
                    {
                        "name": "beta", "scope": "global",
                        "description": "A user skill.",
                        "filePath": "C:\\Users\\me\\.agents\\skills\\beta\\SKILL.md"
                    }
                ],
                "roots": [],
                "lines": [
                    { "tone": "plain", "text": "  NAME   SCOPE      DESCRIPTION        SKILL FILE" },
                    { "tone": "plain", "text": "  \u{2500}\u{2500}\u{2500}\u{2500}  \u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}  \u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}  \u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}" },
                    { "tone": "plain", "text": "  alpha  workspace  A workspace skill.  C:\\work\\proj\\.agents\\skills\\alpha\\SKILL.md" },
                    { "tone": "plain", "text": "  beta   global     A user skill.       C:\\Users\\me\\.agents\\skills\\beta\\SKILL.md" }
                ]
            }),
        )
    });
    let _server = serve(&dir, responder);

    let text = execute(
        Command::Skills {
            workspace: Some("C:\\work\\proj".to_owned()),
        },
        &dir,
    )
    .await
    .unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert!(lines[0].starts_with("  NAME"), "{:?}", lines[0]);
    assert!(lines[0].contains("SCOPE") && lines[0].contains("SKILL FILE"));
    // The rule line, then the rows in the host's order.
    assert!(lines[1].contains('\u{2500}'), "{:?}", lines[1]);
    assert!(lines[2].contains("alpha") && lines[2].contains("workspace"));
    assert!(lines[3].contains("beta") && lines[3].contains("global"));
}

#[tokio::test]
async fn skills_without_a_skill_names_the_roots() {
    let dir = unique_dir("skills-empty");
    let responder: Responder = Arc::new(|request| {
        assert!(request.payload.get("workspace").is_none());
        success(
            request,
            json!({
                "workspace": "C:\\data\\custodian",
                "skills": [],
                "roots": [
                    { "scope": "builtin", "path": "C:\\app\\skills" },
                    { "scope": "global", "path": "C:\\Users\\me\\.agents\\skills" }
                ],
                "lines": [
                    { "tone": "info", "text": "No Agent Skills found." },
                    { "tone": "plain", "text": "  builtin                C:\\app\\skills" },
                    { "tone": "plain", "text": "  global                 C:\\Users\\me\\.agents\\skills" },
                    { "tone": "info", "text": "Add a skill-name/SKILL.md with Agent Skills frontmatter." }
                ]
            }),
        )
    });
    let _server = serve(&dir, responder);

    let text = execute(Command::Skills { workspace: None }, &dir)
        .await
        .unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], "\u{b7} No Agent Skills found.");
    assert_eq!(
        lines[1],
        format!("  {:<22} {}", "builtin", "C:\\app\\skills")
    );
    assert_eq!(
        lines[2],
        format!("  {:<22} {}", "global", "C:\\Users\\me\\.agents\\skills")
    );
    assert_eq!(
        lines[3],
        "\u{b7} Add a skill-name/SKILL.md with Agent Skills frontmatter."
    );
}

// ------------------------------------------------------------------- keys

/// A scripted terminal: queued answers (None = EOF), captured transcript.
struct ScriptTerm {
    answers: std::collections::VecDeque<Option<String>>,
    transcript: Vec<String>,
}

impl ScriptTerm {
    fn new(answers: Vec<Option<&str>>) -> Self {
        Self {
            answers: answers
                .into_iter()
                .map(|answer| answer.map(str::to_owned))
                .collect(),
            transcript: Vec::new(),
        }
    }
}

impl openbutler_cli::commands::setup::Term for ScriptTerm {
    fn write(&mut self, text: &str) {
        self.transcript.push(text.to_owned());
    }

    fn read_line(&mut self, prompt: &str) -> Option<String> {
        self.transcript.push(prompt.to_owned());
        self.answers.pop_front().flatten()
    }

    fn read_password(&mut self, prompt: &str) -> Option<String> {
        self.transcript.push(prompt.to_owned());
        self.answers.pop_front().flatten()
    }
}

#[tokio::test]
async fn keys_list_renders_the_table() {
    let dir = unique_dir("keys-list");
    let responder: Responder = Arc::new(|request| {
        assert_eq!(request.action, "keys.list");
        success(
            request,
            json!({
                "lines": [
                    { "tone": "plain", "text": "  PROVIDER  STATE  KEYS  ENDING" },
                    { "tone": "plain", "text": "  mock      on     2     aaa1 bbb2" }
                ],
                "providers": [
                    { "id": "mock", "enabled": true, "keyCount": 2, "tails": ["…ne", "…wo"] },
                    { "id": "off", "enabled": false, "keyCount": 0, "tails": [] }
                ]
            }),
        )
    });
    let _server = serve(&dir, responder);

    let text = execute(Command::Keys { command: None }, &dir)
        .await
        .unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert!(lines[0].starts_with("  PROVIDER"), "{:?}", lines[0]);
    assert!(lines[0].contains("ENDING"));
    // The host sent two lines; this module prints them and nothing else.
    assert_eq!(lines.len(), 2, "header and one row: {text:?}");
    assert_eq!(lines[1], "  mock      on     2     aaa1 bbb2");
}

#[tokio::test]
async fn keys_list_without_providers_says_so() {
    let dir = unique_dir("keys-empty");
    let responder: Responder = Arc::new(|request| {
        success(
            request,
            json!({
                "providers": [],
                "lines": [
                    { "tone": "info", "text": "No provider is configured." },
                    { "tone": "info", "text": "Add one with: openbutler keys add <provider>" }
                ]
            }),
        )
    });
    let _server = serve(&dir, responder);

    let text = execute(Command::Keys { command: None }, &dir)
        .await
        .unwrap();
    assert_eq!(
        text,
        "\u{b7} No provider is configured.\n\
         \u{b7} Add one with: openbutler keys add <provider>"
    );
}

#[tokio::test]
async fn keys_add_collects_masked_keys_and_names_the_save() {
    let dir = unique_dir("keys-add");
    let responder: Responder = Arc::new(|request| {
        assert_eq!(request.action, "keys.add");
        assert_eq!(request.payload["provider"], "mock");
        assert_eq!(request.payload["keys"], json!(["sk-one"]));
        success(
            request,
            json!({
                "provider": "mock",
                "added": 1,
                "lines": [{ "tone": "success", "text": "mock: 1 key saved to the secret store." }]
            }),
        )
    });
    let _server = serve(&dir, responder);

    let mut term = ScriptTerm::new(vec![Some("sk-one"), Some("n")]);
    let text = openbutler_cli::commands::keys::run_with(
        &mut term,
        &dir,
        openbutler_cli::commands::keys::KeysRequest::Add {
            provider: "mock".to_owned(),
        },
    )
    .await
    .unwrap();
    assert_eq!(text, "\u{2714} mock: 1 key saved to the secret store.");
    assert!(
        term.transcript
            .iter()
            .any(|line| line.contains("Paste the API key for mock")),
        "{:?}",
        term.transcript
    );
    // Only the tail ever appears in the transcript.
    assert!(
        term.transcript.iter().any(|line| line.contains("…-one")),
        "{:?}",
        term.transcript
    );
    assert!(
        !term.transcript.iter().any(|line| line.contains("sk-one")),
        "{:?}",
        term.transcript
    );
}

#[tokio::test]
async fn keys_add_with_nothing_typed_says_nothing_added() {
    let dir = unique_dir("keys-add-empty");
    // No daemon: nothing is typed, so nothing is sent.
    let mut term = ScriptTerm::new(vec![Some(""), None]);
    let text = openbutler_cli::commands::keys::run_with(
        &mut term,
        &dir,
        openbutler_cli::commands::keys::KeysRequest::Add {
            provider: "mock".to_owned(),
        },
    )
    .await
    .unwrap();
    assert_eq!(text, "\u{b7} Nothing added.");
    assert!(
        term.transcript
            .iter()
            .any(|line| line.contains("mock needs at least one API key.")),
        "{:?}",
        term.transcript
    );
}

#[tokio::test]
async fn keys_remove_confirms_then_removes() {
    let dir = unique_dir("keys-remove");
    let responder: Responder = Arc::new(|request| match request.action.as_str() {
        "keys.list" => success(
            request,
            json!({
                "providers": [{ "id": "mock", "enabled": true, "keyCount": 2, "tails": [] }],
                "lines": []
            }),
        ),
        "keys.remove" => {
            assert_eq!(request.payload["provider"], "mock");
            success(
                request,
                json!({
                    "provider": "mock",
                    "removed": 2,
                    "lines": [{ "tone": "success", "text": "mock: keys removed. The provider entry itself stays." }]
                }),
            )
        }
        other => panic!("unexpected action {other}"),
    });
    let _server = serve(&dir, responder);

    let mut term = ScriptTerm::new(vec![Some("y")]);
    let text = openbutler_cli::commands::keys::run_with(
        &mut term,
        &dir,
        openbutler_cli::commands::keys::KeysRequest::Remove {
            provider: "mock".to_owned(),
        },
    )
    .await
    .unwrap();
    assert_eq!(
        text,
        "\u{2714} mock: keys removed. The provider entry itself stays."
    );
    assert!(
        term.transcript
            .iter()
            .any(|line| line.contains("Remove 2 keys for mock?")),
        "{:?}",
        term.transcript
    );
}

#[tokio::test]
async fn keys_remove_declined_writes_nothing() {
    let dir = unique_dir("keys-remove-no");
    let remove_called = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = remove_called.clone();
    let responder: Responder = Arc::new(move |request| match request.action.as_str() {
        "keys.list" => success(
            request,
            json!({
                "providers": [{ "id": "mock", "enabled": true, "keyCount": 1, "tails": [] }],
                "lines": []
            }),
        ),
        other => {
            flag.store(true, Ordering::SeqCst);
            panic!("unexpected action {other}");
        }
    });
    let _server = serve(&dir, responder);

    let mut term = ScriptTerm::new(vec![Some("n")]);
    let text = openbutler_cli::commands::keys::run_with(
        &mut term,
        &dir,
        openbutler_cli::commands::keys::KeysRequest::Remove {
            provider: "mock".to_owned(),
        },
    )
    .await
    .unwrap();
    assert_eq!(text, "\u{b7} Cancelled. Nothing was written.");
    assert!(!remove_called.load(Ordering::SeqCst));
}

#[tokio::test]
async fn keys_remove_unknown_provider_surfaces_the_daemon_sentence() {
    let dir = unique_dir("keys-remove-unknown");
    let responder: Responder = Arc::new(|request| match request.action.as_str() {
        "keys.list" => success(request, json!({ "providers": [], "lines": [] })),
        "keys.remove" => DaemonResponse::Failure {
            version: DAEMON_PROTOCOL_VERSION,
            id: request.id.clone(),
            ok: false,
            error: "Unknown provider \"nope\". Configure it first with openbutler setup."
                .to_owned(),
        },
        other => panic!("unexpected action {other}"),
    });
    let _server = serve(&dir, responder);

    let mut term = ScriptTerm::new(vec![]);
    let error = openbutler_cli::commands::keys::run_with(
        &mut term,
        &dir,
        openbutler_cli::commands::keys::KeysRequest::Remove {
            provider: "nope".to_owned(),
        },
    )
    .await
    .expect_err("a refusal is an error");
    let CliError::Answer(sentence) = error else {
        panic!("expected the daemon's own sentence");
    };
    assert!(sentence.starts_with("Unknown provider \"nope\"."));
}

#[test]
fn keys_add_refuses_without_a_terminal() {
    let dir = unique_dir("keys-guard");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_openbutler"))
        .args(["keys", "add", "mock"])
        .env("OPENBUTLER_DATA_DIR", &dir)
        .stdin(std::process::Stdio::piped())
        .output()
        .expect("spawn the binary");
    assert_eq!(output.status.code(), Some(1), "a refusal is exit class 1");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("openbutler keys add needs an interactive terminal."),
        "{stderr}"
    );
}

#[tokio::test]
async fn sessions_prune_previews_the_host_lines() {
    let dir = unique_dir("sessions-prune-preview");
    let seen = Arc::new(std::sync::Mutex::new(None));
    let responder: Responder = {
        let seen = seen.clone();
        Arc::new(
            move |request: &DaemonRequest| match request.action.as_str() {
                "sessions.prune" => {
                    *seen.lock().unwrap() = Some(Value::Object(request.payload.clone()));
                    success(
                        request,
                        json!({
                            "count": 1,
                            "totalBytes": 1024,
                            "lines": [
                                { "tone": "plain", "text": "  2020-01-02  ses-111111111  1.0 KB" },
                                { "tone": "info", "text": "1 session file(s), 1.0 KB total. Nothing was deleted; re-run with --yes to delete them." }
                            ]
                        }),
                    )
                }
                other => panic!("unexpected action {other}"),
            },
        )
    };
    let _server = serve(&dir, responder);

    let preview = execute(
        Command::Sessions {
            command: Some(SessionsCommand::Prune {
                older_than: "30d".to_owned(),
                yes: false,
            }),
        },
        &dir,
    )
    .await
    .unwrap();
    let lines: Vec<&str> = preview.lines().collect();
    assert_eq!(lines[0], "  2020-01-02  ses-111111111  1.0 KB");
    assert_eq!(
        lines[1],
        "\u{b7} 1 session file(s), 1.0 KB total. Nothing was deleted; re-run with --yes to delete them."
    );
    assert_eq!(
        seen.lock().unwrap().clone(),
        Some(json!({ "olderThan": "30d", "apply": false }))
    );
}

#[tokio::test]
async fn sessions_prune_applies_with_the_success_line() {
    let dir = unique_dir("sessions-prune-apply");
    let seen = Arc::new(std::sync::Mutex::new(None));
    let responder: Responder = {
        let seen = seen.clone();
        Arc::new(
            move |request: &DaemonRequest| match request.action.as_str() {
                "sessions.prune" => {
                    *seen.lock().unwrap() = Some(Value::Object(request.payload.clone()));
                    success(
                        request,
                        json!({
                            "count": 1,
                            "totalBytes": 1024,
                            "deleted": 1,
                            "failed": 0,
                            "lines": [
                                { "tone": "plain", "text": "  2020-01-02  ses-111111111  1.0 KB" },
                                { "tone": "success", "text": "Deleted 1 session file(s) (1.0 KB)." }
                            ]
                        }),
                    )
                }
                other => panic!("unexpected action {other}"),
            },
        )
    };
    let _server = serve(&dir, responder);

    let output = execute(
        Command::Sessions {
            command: Some(SessionsCommand::Prune {
                older_than: "30d".to_owned(),
                yes: true,
            }),
        },
        &dir,
    )
    .await
    .unwrap();
    assert_eq!(
        output.lines().last().unwrap(),
        "\u{2714} Deleted 1 session file(s) (1.0 KB)."
    );
    assert_eq!(
        seen.lock().unwrap().clone(),
        Some(json!({ "olderThan": "30d", "apply": true }))
    );
}

#[tokio::test]
async fn sessions_prune_keeps_the_duration_refusal_verbatim() {
    let dir = unique_dir("sessions-prune-refusal");
    let responder: Responder = Arc::new(|request: &DaemonRequest| match request.action.as_str() {
        "sessions.prune" => DaemonResponse::Failure {
            version: DAEMON_PROTOCOL_VERSION,
            id: request.id.clone(),
            ok: false,
            error: "Invalid duration \"3x\". Use a value like 30d, 12h, 90m, or 2w.".to_owned(),
        },
        other => panic!("unexpected action {other}"),
    });
    let _server = serve(&dir, responder);

    let error = execute(
        Command::Sessions {
            command: Some(SessionsCommand::Prune {
                older_than: "3x".to_owned(),
                yes: false,
            }),
        },
        &dir,
    )
    .await
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        "Invalid duration \"3x\". Use a value like 30d, 12h, 90m, or 2w."
    );
}

#[test]
fn sessions_prune_needs_the_window() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_openbutler"))
        .args(["sessions", "prune"])
        .output()
        .expect("spawn the binary");
    assert_eq!(
        output.status.code(),
        Some(2),
        "a usage error is exit class 2"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--older-than"), "{stderr}");
}

#[tokio::test]
async fn mcp_list_prints_the_hosts_screen() {
    let dir = unique_dir("mcp-list");
    let responder: Responder = Arc::new(|request| {
        assert_eq!(request.action, "mcp.manage");
        assert_eq!(
            request
                .payload
                .get("command")
                .and_then(serde_json::Value::as_str),
            Some("list")
        );
        success(
            request,
            json!({
                "servers": [],
                "lines": [
                    { "tone": "plain", "text": "\u{2500}\u{2500} MCP servers \u{2500}\u{2500}" },
                    { "tone": "info", "text": "No MCP servers configured." }
                ]
            }),
        )
    });
    let _server = serve(&dir, responder);

    let text = execute(
        Command::Mcp {
            action: Some("list".to_owned()),
        },
        &dir,
    )
    .await
    .unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2, "{text:?}");
    assert_eq!(lines[0], "\u{2500}\u{2500} MCP servers \u{2500}\u{2500}");
    // An info line carries the palette's middle dot; the host's text follows.
    assert_eq!(lines[1], "\u{b7} No MCP servers configured.");
}

#[tokio::test]
async fn mcp_without_a_terminal_lists_and_says_so() {
    let dir = unique_dir("mcp-notty");
    let responder: Responder = Arc::new(|request| {
        success(
            request,
            json!({
                "servers": [],
                "lines": [{ "tone": "info", "text": "No MCP servers configured." }]
            }),
        )
    });
    let _server = serve(&dir, responder);

    // The test process has no terminal on stdin, so add degrades to the
    // source's read-only path plus its hint.
    let text = execute(
        Command::Mcp {
            action: Some("add".to_owned()),
        },
        &dir,
    )
    .await
    .unwrap();
    assert!(text.contains("No MCP servers configured."), "{text:?}");
    assert!(
        text.ends_with("Use an interactive terminal with openbutler mcp to add or remove servers."),
        "{text:?}"
    );
}

#[tokio::test]
async fn mcp_refuses_an_unknown_subcommand() {
    let dir = unique_dir("mcp-usage");
    let error = execute(
        Command::Mcp {
            action: Some("frobnicate".to_owned()),
        },
        &dir,
    )
    .await
    .expect_err("a refusal");
    assert_eq!(error.to_string(), "Usage: openbutler mcp [list|add|remove]");
}

#[tokio::test]
async fn schedule_add_reads_the_file_and_prints_the_hosts_line() {
    let dir = unique_dir("schedule-add");
    let file = dir.join("schedule.json");
    std::fs::write(
        &file,
        r#"{"name":"morning","trigger":{"adapter":"daily","options":{"at":"09:00"}},"instruction":"hi","workspaceRoot":"."}"#,
    )
    .unwrap();
    let responder: Responder = Arc::new(|request| {
        assert_eq!(request.action, "schedule.manage");
        assert_eq!(request.payload["input"]["action"], "add");
        assert_eq!(request.payload["input"]["schedule"]["name"], "morning");
        success(
            request,
            json!({
                "lines": [{
                    "tone": "success",
                    "text": "Schedule morning saved (every day at 09:00). Timers are live."
                }]
            }),
        )
    });
    let _server = serve(&dir, responder);
    let text = execute(
        Command::Schedule {
            words: vec!["add".to_owned(), file.display().to_string()],
        },
        &dir,
    )
    .await
    .unwrap();
    assert!(text.contains("Timers are live."), "{text:?}");
}
// --- admin create ------------------------------------------------------------

/// Answers from a script and claims a terminal, so the masked prompts stay
/// exercised without a real tty.
struct ScriptedPrompt(Vec<String>);

impl openbutler_cli::commands::connect::SecretPrompt for ScriptedPrompt {
    fn is_interactive(&self) -> bool {
        true
    }

    fn ask(&mut self, _label: &str) -> Result<String, CliError> {
        Ok(self.0.remove(0))
    }
}

#[tokio::test]
async fn admin_create_sends_the_payload_and_prints_only_the_host_line() {
    let dir = unique_dir("admin-create");
    let responder: Responder = Arc::new(|request| {
        assert_eq!(request.action, "admin.create");
        assert_eq!(
            request.get("username").and_then(Value::as_str),
            Some("owner")
        );
        assert_eq!(
            request.get("password").and_then(Value::as_str),
            Some("correct horse battery"),
            "the password reaches the host as the terminal typed it"
        );
        DaemonResponse::Success {
            version: DAEMON_PROTOCOL_VERSION,
            id: request.id.clone(),
            ok: true,
            result: json!({
                "username": "owner",
                "lines": [{ "tone": "success", "text": "Admin account owner created." }]
            }),
        }
    });
    let _server = serve(&dir, responder);
    let mut prompt = ScriptedPrompt(vec![
        "correct horse battery".to_owned(),
        "correct horse battery".to_owned(),
    ]);
    let text = openbutler_cli::commands::admin::create(&dir, "owner", &mut prompt)
        .await
        .expect("create succeeds");
    assert_eq!(
        text, "✔ Admin account owner created.",
        "the client prints the host's sentence and adds nothing"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn admin_create_refuses_a_mismatch_before_anything_is_sent() {
    let dir = unique_dir("admin-mismatch");
    let mut prompt = ScriptedPrompt(vec![
        "correct horse battery".to_owned(),
        "something else".to_owned(),
    ]);
    let error = openbutler_cli::commands::admin::create(&dir, "owner", &mut prompt)
        .await
        .expect_err("the two prompts must agree");
    assert_eq!(
        error.to_string(),
        openbutler_cli::commands::admin::MISMATCH,
        "the confirmation never leaves the terminal"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
// -------------------------------------------------------- approve (C)

#[tokio::test]
async fn approve_lists_the_hosts_dials() {
    let dir = unique_dir("approve-list");
    let seen: Arc<std::sync::Mutex<Option<Value>>> = Arc::new(std::sync::Mutex::new(None));
    let captured = seen.clone();
    let responder: Responder = Arc::new(move |request: &DaemonRequest| {
        match request.action.as_str() {
            "auto_review.approve" => {
                *captured.lock().unwrap() = Some(Value::Object(request.payload.clone()));
                success(
                    request,
                    json!({ "lines": [
                        { "tone": "plain", "text": "\u{2500}\u{2500} Recent auto-review denials \u{2500}\u{2500}" },
                        { "tone": "plain", "text": "  #  TOOL" },
                        { "tone": "info", "text": "Approve one with `openbutler approve <n>`." }
                    ] }),
                )
            }
            other => panic!("unexpected action {other}"),
        }
    });
    let _server = serve(&dir, responder);

    let text = execute(Command::Approve { index: None }, &dir)
        .await
        .unwrap();
    assert_eq!(
        text,
        "\u{2500}\u{2500} Recent auto-review denials \u{2500}\u{2500}\n  #  TOOL\n\u{b7} Approve one with `openbutler approve <n>`."
    );
    assert_eq!(seen.lock().unwrap().clone(), Some(json!({})));
}

#[tokio::test]
async fn approve_sends_the_number_and_prints_the_hosts_answer() {
    let dir = unique_dir("approve-one");
    let seen: Arc<std::sync::Mutex<Option<Value>>> = Arc::new(std::sync::Mutex::new(None));
    let captured = seen.clone();
    let responder: Responder = Arc::new(move |request: &DaemonRequest| {
        match request.action.as_str() {
            "auto_review.approve" => {
                *captured.lock().unwrap() = Some(Value::Object(request.payload.clone()));
                success(
                    request,
                    json!({ "lines": [{ "tone": "success", "text": "Approved `execute_command` for one retry." }] }),
                )
            }
            other => panic!("unexpected action {other}"),
        }
    });
    let _server = serve(&dir, responder);

    let text = execute(Command::Approve { index: Some(1) }, &dir)
        .await
        .unwrap();
    assert_eq!(text, "\u{2714} Approved `execute_command` for one retry.");
    assert_eq!(seen.lock().unwrap().clone(), Some(json!({ "index": 1 })));
}
