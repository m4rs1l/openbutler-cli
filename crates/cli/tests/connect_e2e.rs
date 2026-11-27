#![allow(clippy::unwrap_used, clippy::expect_used)]
//! `connect` and the stored backend, end to end over loopback TCP.
//!
//! The fake host speaks the real envelope: it answers `login` with the
//! contract's `LoginSuccess`, `auth` with `authenticated`, and `status` with the
//! host's own screen. That pins the client's side of the contract — the frame
//! it sends, the two files it writes, and the single sentence it adds.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use openbutler_cli::commands::connect::{self, SecretPrompt};
use openbutler_cli::remote::{self, StoredSession};
use openbutler_cli::{CliError, Command, execute};
use openbutler_protocol::{BackendTarget, DaemonResponse, FrameDecoder, MAX_DAEMON_FRAME_BYTES};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const SESSION_TOKEN: &str = "obt_test_session";

type Seen = Arc<Mutex<Vec<Value>>>;

fn unique_dir(tag: &str) -> PathBuf {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ob-cli-connect-{tag}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Binds a loopback listener and answers `connections` connections with the
/// contract's shapes, recording every request frame it saw.
async fn serve(connections: usize) -> (SocketAddr, Seen) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&seen);
    tokio::spawn(async move {
        for _ in 0..connections {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let seen = Arc::clone(&recorded);
            tokio::spawn(async move {
                let mut decoder = FrameDecoder::new(MAX_DAEMON_FRAME_BYTES);
                let mut buffer = vec![0u8; 4096];
                loop {
                    let read = stream.read(&mut buffer).await.unwrap_or(0);
                    if read == 0 {
                        return;
                    }
                    for frame in decoder.append(&buffer[..read]).unwrap() {
                        let value: Value = serde_json::from_slice(&frame).unwrap();
                        seen.lock().unwrap().push(value.clone());
                        let id = value["id"].as_str().unwrap_or_default().to_owned();
                        let action = value["action"].as_str().unwrap_or_default().to_owned();
                        let response = match action.as_str() {
                            "login" => success(
                                id,
                                json!({
                                    "deviceId": "dev-1",
                                    "token": SESSION_TOKEN,
                                    "expiresAt": "2026-12-27T09:00:00.000Z"
                                }),
                            ),
                            "auth" => success(id, json!({ "authenticated": true })),
                            "status" => success(
                                id,
                                json!({
                                    "pid": 21104,
                                    "startedAt": "2026-11-15T20:41:21.790Z",
                                    "entry": "/opt/openbutler/cli.js",
                                    "workspaceCount": 3,
                                    "connectedClients": 2,
                                    "schedules": 0,
                                    "telegram": {
                                        "state": "disabled",
                                        "paired": false,
                                        "username": null,
                                        "error": null
                                    },
                                    "lines": [{ "tone": "plain", "text": "host line" }]
                                }),
                            ),
                            other => DaemonResponse::Failure {
                                version: openbutler_protocol::DAEMON_PROTOCOL_VERSION,
                                id,
                                ok: false,
                                error: format!("unexpected action {other}"),
                            },
                        };
                        let mut bytes = serde_json::to_vec(&response).unwrap();
                        bytes.push(b'\n');
                        stream.write_all(&bytes).await.unwrap();
                    }
                }
            });
        }
    });
    (addr, seen)
}

fn success(id: String, result: Value) -> DaemonResponse {
    DaemonResponse::Success {
        version: openbutler_protocol::DAEMON_PROTOCOL_VERSION,
        id,
        ok: true,
        result,
    }
}

/// A prompt that answers from a script and claims a terminal.
struct Script {
    answers: Vec<String>,
    interactive: bool,
}

impl Script {
    fn new(answers: &[&str]) -> Self {
        Self {
            answers: answers.iter().map(|answer| (*answer).to_owned()).collect(),
            interactive: true,
        }
    }

    fn headless() -> Self {
        Self {
            answers: Vec::new(),
            interactive: false,
        }
    }
}

impl SecretPrompt for Script {
    fn is_interactive(&self) -> bool {
        self.interactive
    }

    fn ask(&mut self, _label: &str) -> Result<String, CliError> {
        Ok(self.answers.remove(0))
    }
}

fn store(dir: &std::path::Path, addr: SocketAddr) {
    let target = BackendTarget::new(
        addr.ip().to_string(),
        addr.port(),
        "owner",
        "laptop",
        "2026-11-27T09:00:00.000Z",
    );
    let session = StoredSession {
        session_id: "dev-1".to_owned(),
        token: SESSION_TOKEN.to_owned(),
        expires_at: "2026-12-27T09:00:00.000Z".to_owned(),
    };
    remote::save(dir, &target, &session).unwrap();
}

#[tokio::test]
async fn connect_stores_the_target_and_the_session_and_prints_the_host_screen() {
    let dir = unique_dir("login");
    let (addr, seen) = serve(1).await;
    let mut prompt = Script::new(&["correct horse battery"]);

    let text = connect::run(
        &dir,
        Some(addr.to_string()),
        Some("owner".to_owned()),
        false,
        &mut prompt,
    )
    .await
    .expect("sign-in succeeds");

    let frames = seen.lock().unwrap().clone();
    assert_eq!(frames[0]["action"], "login", "the first frame signs in");
    assert_eq!(frames[0]["username"], "owner");
    assert_eq!(frames[0]["password"], "correct horse battery");
    assert!(
        frames[0]["device"]
            .as_str()
            .is_some_and(|label| !label.is_empty()),
        "the client names itself for `device list`"
    );
    assert_eq!(
        frames[1]["action"], "status",
        "the screen comes from the host"
    );

    assert!(text.contains("host line"), "the host's screen is printed");
    assert!(
        text.contains("This client now uses"),
        "the one client line names its own file: {text}"
    );

    let target = std::fs::read_to_string(dir.join(remote::BACKEND_FILE)).expect("backend file");
    assert!(
        !target.contains(SESSION_TOKEN),
        "the target file must never hold the credential"
    );
    let backend: Value = serde_json::from_str(&target).unwrap();
    assert_eq!(backend["username"], "owner");
    assert_eq!(backend["port"], addr.port());

    let credential =
        std::fs::read_to_string(dir.join(remote::CREDENTIAL_FILE)).expect("credential file");
    assert!(
        credential.contains(SESSION_TOKEN),
        "the session lives in its own file"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_stored_backend_routes_later_commands_over_tcp() {
    let dir = unique_dir("routed");
    let (addr, seen) = serve(1).await;
    store(&dir, addr);

    let text = execute(Command::Status, &dir)
        .await
        .expect("status runs remotely");
    assert!(text.contains("host line"), "the host's screen is printed");

    let frames = seen.lock().unwrap().clone();
    assert_eq!(
        frames[0]["action"], "auth",
        "a stored backend authenticates first"
    );
    assert_eq!(frames[0]["token"], SESSION_TOKEN);
    assert_eq!(frames[1]["action"], "status");

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn forget_removes_both_files() {
    let dir = unique_dir("forget");
    let (addr, _seen) = serve(0).await;
    store(&dir, addr);

    let mut prompt = Script::headless();
    let text = connect::run(&dir, None, None, true, &mut prompt)
        .await
        .expect("forget succeeds");

    assert!(text.contains("no longer has a stored backend"), "{text}");
    assert!(!dir.join(remote::BACKEND_FILE).exists(), "target removed");
    assert!(
        !dir.join(remote::CREDENTIAL_FILE).exists(),
        "session removed"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_half_stored_backend_is_refused_not_ignored() {
    let dir = unique_dir("half");
    let (addr, _seen) = serve(0).await;
    let target = BackendTarget::new(
        addr.ip().to_string(),
        addr.port(),
        "owner",
        "laptop",
        "2026-11-27T09:00:00.000Z",
    );
    std::fs::write(
        dir.join(remote::BACKEND_FILE),
        serde_json::to_string(&target).unwrap(),
    )
    .unwrap();

    let error = execute(Command::Status, &dir)
        .await
        .expect_err("a target without a credential must not be used");
    let message = error.to_string();
    assert!(
        message.contains("cannot be read") && message.contains("connect --forget"),
        "the refusal names the file and the remedy: {message}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn connect_without_a_terminal_is_refused_before_dialing() {
    let dir = unique_dir("headless");
    let mut prompt = Script::headless();
    let error = connect::run(
        &dir,
        Some("127.0.0.1:1".to_owned()),
        Some("owner".to_owned()),
        false,
        &mut prompt,
    )
    .await
    .expect_err("no terminal, no prompt");
    assert_eq!(error.to_string(), connect::TTY_REFUSAL);
    assert!(
        !dir.join(remote::BACKEND_FILE).exists(),
        "a refused connect writes nothing"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
