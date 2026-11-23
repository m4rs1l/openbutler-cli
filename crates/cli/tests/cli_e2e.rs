//! S1 end-to-end pins: the CLI against a contract-shaped fake daemon on
//! the endpoint the client itself derives. The fake speaks the real
//! envelope (protocol types only), so these tests pin dialing,
//! correlation, rendering, and the failure paths without a real daemon.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use openbutler_cli::{CliError, Command, DaemonCommand, execute};
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
            // The script is the contract: once fulfilled the fake leaves
            // the endpoint, which is how the stop flow observes a host
            // that is actually gone.
            if index == steps.len() {
                break;
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

// ------------------------------------------------- daemon lifecycle (S3)

fn status_fixture(pid: u32) -> Value {
    let status = DaemonStatus {
        pid,
        started_at: "2026-11-22T02:00:00Z".to_owned(),
        workspace_count: 1,
        connected_clients: 2,
        schedules: None,
        telegram: None,
        unresponsive: None,
        entry: None,
        build_id: None,
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
async fn daemon_pair_mints_a_code_and_renders_the_redeem_hint() {
    let dir = unique_dir("daemon-pair");
    let steps = vec![Step {
        action: "pair.create",
        result: json!({ "code": "a1b2c3d4e5f6", "expiresAt": 1_763_778_000_000u64 }),
        error: None,
        events: Vec::new(),
    }];
    let _server = serve_steps(&dir, steps);

    let text = execute(
        Command::Daemon {
            command: Some(DaemonCommand::Pair),
        },
        &dir,
    )
    .await
    .unwrap();
    let mut lines = text.lines();
    assert_eq!(lines.next().unwrap(), "Pairing code: a1b2c3d4e5f6");
    let expires = lines.next().unwrap();
    // Local wall clock (TZ-dependent value): shape-pinned as HH:MM:SS.
    assert!(expires.starts_with("Expires:"), "{expires}");
    let clock = expires.trim_start_matches("Expires:").trim_start();
    assert_eq!(clock.len(), 8, "{expires}");
    assert_eq!(clock.matches(':').count(), 2, "{expires}");
    assert_eq!(
        lines.next().unwrap(),
        "Redeem it on the client within 5 minutes: openbutler connect <host:port> --code a1b2c3d4e5f6"
    );
    assert_eq!(lines.next(), None);
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
    let text =
        openbutler_cli::commands::daemon::start_with(&dir, Path::new("definitely-unused.exe"))
            .await
            .unwrap();
    assert_eq!(
        text,
        "The machine custodian and its workspace runtime initialize when a TUI or Telegram turn arrives.\n\
         Background host running (pid 77).\n\
         workspaces:    1\n\
         It stays alive after this terminal closes. Stop it with `openbutler daemon stop`."
    );
}

#[tokio::test]
async fn daemon_start_diagnoses_a_child_that_died_before_ready() {
    let dir = unique_dir("daemon-start-dead");
    // `where run` finds nothing, writes its own message to the log, and
    // exits — the diagnosis must quote that log and name the exit.
    let where_exe = std::env::var_os("WINDIR")
        .map(|windir| PathBuf::from(windir).join("System32").join("where.exe"))
        .expect("WINDIR is set on Windows");
    let error = openbutler_cli::commands::daemon::start_with(&dir, &where_exe)
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
    let error = openbutler_cli::commands::daemon::start_with(&dir, &missing)
        .await
        .expect_err("a missing executable must fail the start");
    let message = error.to_string();
    assert!(
        message.starts_with("Could not start the background host: "),
        "{message}"
    );
}

// ------------------------------------------------- machine log (S3)

#[tokio::test]
async fn logs_tails_the_machine_log() {
    let dir = unique_dir("logs-tail");
    std::fs::create_dir_all(&dir).unwrap();
    // CRLF like a Windows child writes.
    std::fs::write(
        dir.join("daemon.log"),
        "one\r\ntwo\r\nthree\r\nfour\r\nfive\r\n",
    )
    .unwrap();

    let text = execute(
        Command::Logs {
            lines: 3,
            file: None,
        },
        &dir,
    )
    .await
    .unwrap();
    assert_eq!(text, "three\nfour\nfive");
}

#[tokio::test]
async fn logs_on_a_missing_file_says_so() {
    let dir = unique_dir("logs-missing");
    let text = execute(
        Command::Logs {
            lines: 100,
            file: None,
        },
        &dir,
    )
    .await
    .unwrap();
    assert_eq!(
        text,
        format!("No log lines at {}.", dir.join("daemon.log").display())
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

/// The daemon's `openbutler_usage` DDL — the contract the reader was
/// written against (copied from `ledger/usage.rs`; if it changes, these
/// pins fail before a user does).
fn seed_usage_db(dir: &Path) {
    use rusqlite::Connection;
    std::fs::create_dir_all(dir).unwrap();
    let db = Connection::open(dir.join("state.db")).unwrap();
    db.execute_batch(
        "CREATE TABLE openbutler_usage (
            id TEXT PRIMARY KEY,
            at_ms INTEGER NOT NULL,
            ended_ms INTEGER,
            duration_ms INTEGER,
            tz_offset_min INTEGER NOT NULL,
            session_id TEXT NOT NULL,
            turn_id TEXT,
            event_id TEXT,
            run_kind TEXT NOT NULL,
            workspace_root TEXT,
            provider_id TEXT NOT NULL,
            model_id TEXT NOT NULL,
            attempt INTEGER NOT NULL DEFAULT 1,
            status TEXT NOT NULL,
            input_tokens INTEGER,
            no_cache_tokens INTEGER,
            cache_read_tokens INTEGER,
            cache_write_tokens INTEGER,
            output_tokens INTEGER,
            text_tokens INTEGER,
            reasoning_tokens INTEGER,
            total_tokens INTEGER,
            usage_source TEXT NOT NULL,
            context_tokens INTEGER,
            price_input REAL,
            price_output REAL,
            price_reasoning REAL,
            price_cache_read REAL,
            price_cache_write REAL,
            price_version TEXT,
            cost_usd REAL,
            cost_source TEXT,
            upstream_provider TEXT,
            generation_id TEXT
        );",
    )
    .unwrap();
}

/// One seeded usage row. The argument list is the row itself — a
/// fixture mirroring the INSERT, not an API.
#[allow(clippy::too_many_arguments)]
fn insert_usage_row(
    dir: &Path,
    id: &str,
    at_ms: i64,
    tz_offset_min: i64,
    session: &str,
    provider: &str,
    model: &str,
    input: Option<i64>,
    no_cache: Option<i64>,
    cache_read: Option<i64>,
    output: Option<i64>,
    reasoning: Option<i64>,
    cost: Option<f64>,
    duration_ms: i64,
    cost_source: &str,
) {
    use rusqlite::Connection;
    let db = Connection::open(dir.join("state.db")).unwrap();
    db.execute(
        "INSERT INTO openbutler_usage (
            id, at_ms, tz_offset_min, session_id, run_kind, provider_id, model_id, status,
            usage_source, input_tokens, no_cache_tokens, cache_read_tokens, cache_write_tokens,
            output_tokens, reasoning_tokens, cost_usd, duration_ms, cost_source
        ) VALUES (?1, ?2, ?3, ?4, 'chat', ?5, ?6, 'ok', 'provider', ?7, ?8, ?9, 0, ?10, ?11, ?12, ?13, ?14)",
        rusqlite::params![
            id,
            at_ms,
            tz_offset_min,
            session,
            provider,
            model,
            input,
            no_cache,
            cache_read,
            output,
            reasoning,
            cost,
            duration_ms,
            cost_source
        ],
    )
    .unwrap();
}

/// Today's UTC midnight as a millisecond anchor, so rows land in the
/// window whatever day the test runs.
fn utc_midnight_today_ms() -> i64 {
    chrono::Utc::now()
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc()
        .timestamp_millis()
}

fn day_string(at_ms: i64) -> String {
    use chrono::TimeZone;
    chrono::Utc
        .timestamp_millis_opt(at_ms)
        .single()
        .unwrap()
        .format("%Y-%m-%d")
        .to_string()
}

#[tokio::test]
async fn usage_renders_the_report_line_by_line() {
    let dir = unique_dir("usage-report");
    seed_usage_db(&dir);
    let midnight = utc_midnight_today_ms();
    // Row A: three days back, zero offset (plain UTC day).
    let at_a = midnight - 3 * 86_400_000;
    // Row B: yesterday 23:00 UTC with a +180 min recorded offset — the
    // local day rolls into today, so the row's own offset is what counts.
    let at_b = midnight - 3_600_000;
    let day_a = day_string(at_a);
    let day_b = day_string(at_b + 180 * 60_000);
    assert_ne!(day_a, day_b);
    insert_usage_row(
        &dir,
        "row-a",
        at_a,
        0,
        "sess-alpha",
        "mock",
        "demo",
        Some(1000),
        Some(600),
        Some(400),
        Some(200),
        Some(50),
        Some(0.0123),
        1500,
        "provider-inline",
    );
    insert_usage_row(
        &dir,
        "row-b",
        at_b,
        180,
        "sess-beta",
        "mock",
        "demo",
        Some(2000),
        None,
        None,
        Some(300),
        None,
        Some(0.0456),
        500,
        "provider-inline",
    );

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
    let expected = format!(
        "Usage last 30 days\n  \
         requests 2  input 2.6k  cache read 400  cache write 0  output 500  reasoning 50  cost $0.0579\n  \
         busiest {day_a} (1 requests)  longest task 2s  streak 1d\n  \
         cost source: provider for every request\n\
         \n\
         day         reqs    input    cache   output       cost\n\
         {day_a}     1      600      400      200    $0.0123\n\
         {day_b}     1     2.0k        0      300    $0.0456\n\
         \n\
         token shape  ▄█"
    );
    assert_eq!(text, expected);
}

#[tokio::test]
async fn usage_without_rows_says_so() {
    let dir = unique_dir("usage-empty");
    seed_usage_db(&dir);
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
async fn usage_filters_by_session_and_model() {
    let dir = unique_dir("usage-filter");
    seed_usage_db(&dir);
    let midnight = utc_midnight_today_ms();
    insert_usage_row(
        &dir,
        "keep",
        midnight - 86_400_000,
        0,
        "sess-alpha",
        "mock",
        "demo",
        Some(500),
        None,
        None,
        Some(100),
        None,
        Some(0.001),
        300,
        "provider-inline",
    );
    insert_usage_row(
        &dir,
        "other-session",
        midnight - 86_400_000,
        0,
        "sess-beta",
        "mock",
        "demo",
        Some(9_999),
        None,
        None,
        Some(9_999),
        None,
        Some(9.9),
        900,
        "provider-inline",
    );
    insert_usage_row(
        &dir,
        "other-model",
        midnight - 86_400_000,
        0,
        "sess-alpha",
        "other",
        "model-x",
        Some(9_999),
        None,
        None,
        Some(9_999),
        None,
        Some(9.9),
        900,
        "provider-inline",
    );

    let text = execute(
        Command::Usage {
            days: 30,
            session: Some("sess-alpha".to_owned()),
            model: Some("mock/demo".to_owned()),
            json: false,
        },
        &dir,
    )
    .await
    .unwrap();
    let lines: Vec<&str> = text.lines().collect();
    // The source's `slice(0, 8)`: the first eight characters, not the
    // whole id.
    assert_eq!(
        lines[0],
        "Usage last 30 days (session sess-alp, model mock/demo)"
    );
    assert!(lines[1].contains("requests 1"), "{}", lines[1]);
    assert!(lines[1].contains("cost $0.0010"), "{}", lines[1]);
}

#[tokio::test]
async fn usage_json_shape_is_the_source_envelope() {
    let dir = unique_dir("usage-json");
    seed_usage_db(&dir);
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
    let parsed: serde_json::Value = serde_json::from_str(&text).expect("valid JSON");
    assert_eq!(parsed["label"], "last 7 days");
    assert_eq!(parsed["summary"]["days"], 0);
    assert_eq!(parsed["summary"]["streakDays"], 0);
    assert!(parsed["summary"]["busiest"].is_null());
    assert!(
        parsed["days"]
            .as_array()
            .is_some_and(|days| days.is_empty())
    );
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
async fn config_renders_the_report_the_daemon_computed() {
    let dir = unique_dir("config");
    let responder: Responder = Arc::new(|request| {
        assert_eq!(request.action, "config.report");
        success(
            request,
            json!({
                "mcpServers": [],
                "defaultWorkspace": null,
                "toolAccess": "Workspace (default)",
                "hasProviders": true,
                "catalogLoaded": false,
                "catalogLoading": true,
                "providers": [
                    {
                        "id": "mock", "enabled": true, "keyCount": 1,
                        "toolCapableCount": null, "isDefault": true, "custom": true,
                        "url": "http://127.0.0.1:9/v1", "defaultModel": "test-model"
                    }
                ],
                "default": { "providerId": "mock", "modelId": "test-model", "source": "settings" },
                "reasoning": "default",
                "showReasoning": true,
                "readiness": { "ready": true, "problems": [], "hints": [] }
            }),
        )
    });
    let _server = serve(&dir, responder);

    let text = execute(Command::Config, &dir).await.unwrap();
    let lines: Vec<&str> = text.lines().map(str::trim_end).collect();
    assert_eq!(
        lines[0], "",
        "the screen opens with the section's blank line"
    );
    assert!(
        lines[1].starts_with("\u{2500}\u{2500} MCP servers "),
        "{:?}",
        lines[1]
    );
    assert!(text.contains("\u{b7} No MCP servers configured."), "{text}");
    assert!(
        text.contains("  Tool access            Workspace (default)"),
        "{text}"
    );
    assert!(
        text.contains("  mock *    on     1     unknown  test-model  custom"),
        "the provider row is the daemon's row, laid out: {text}"
    );
    assert!(
        text.contains("  Default                mock / test-model"),
        "{text}"
    );
    assert!(
        text.contains("The catalog is downloading in the background"),
        "{text}"
    );
    assert!(text.contains("\u{2714} Ready to run."), "{text}");
    assert!(
        lines
            .last()
            .is_some_and(|line| line.starts_with("\u{b7} The catalog is downloading")),
        "the download note closes the screen: {text}"
    );
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
