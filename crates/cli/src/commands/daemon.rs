//! `openbutler daemon <subcommand>` — host lifecycle over the local pipe.
//!
//! `status` shares the render authority with `openbutler status` (one
//! report, two spellings). `stop` fires the host's own `stop` action —
//! it acks before shutting down — and then waits for the endpoint to go
//! away. Nothing here terminates a process: this build keeps no pid
//! record, so identity can never be verified, and the timeout answer is
//! the source's refusal sentence saying exactly that.

use std::path::Path;
use std::time::Duration;

use crate::client::ClientError;
use crate::{CliError, commands};
use chrono::TimeZone;

/// The payload `pair.create` answers with (the source's shape).
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct IssuedPair {
    code: String,
    expires_at: u64,
}

/// Poll interval while waiting for the host to leave the endpoint (the
/// source polls `status` at 250 ms in `requireBackgroundHost`).
const POLL: Duration = Duration::from_millis(250);
/// The source's shutdown window; after it the source would verify the
/// process identity before terminating, which this build cannot do.
const STOP_TIMEOUT: Duration = Duration::from_secs(30);
/// Boot deadline before a silent child is killed (the source's
/// `DAEMON_START_TIMEOUT_MS`).
const START_TIMEOUT: Duration = Duration::from_secs(20);
/// Backoffs between spawn attempts when a child dies transiently (the
/// source's `STARTUP_RETRY_BACKOFF_MS`: under memory pressure the first
/// spawn can lose an allocation while a retry succeeds).
const STARTUP_RETRY_BACKOFF_MS: [u64; 2] = [750, 2_000];

/// A child that died during boot: the message is shown, and `retryable`
/// is the source's transient/deterministic verdict.
struct StartupFailure {
    message: String,
    retryable: bool,
}

/// `openbutler daemon status` — the host's report, as `status` renders it.
pub async fn status(data_dir: &Path) -> Result<String, CliError> {
    commands::status::run(data_dir).await
}

/// `openbutler daemon stop` — graceful stop, then wait for the endpoint.
///
/// Not running is a normal answer (exit 0), a running host is asked to
/// stop and polled until its endpoint disappears, and a host that is
/// still there after the window fails with the source's refusal: there
/// is no pid record to verify an identity against, so it is never
/// terminated blindly.
pub async fn stop(data_dir: &Path) -> Result<String, CliError> {
    let pid = match status_pid(data_dir).await {
        Ok(pid) => pid,
        Err(CliError::Client(ClientError::Unreachable { .. })) => {
            return Ok("Background host is not running.".to_owned());
        }
        Err(error) => return Err(error),
    };
    // The host acks `stop` before it begins shutting down, so the reply
    // is read while the endpoint is still alive. A refusal is held: like
    // the source, it is only reported if the host is still there when
    // the window expires (a racing restart already answered the need).
    let ack = commands::ask(data_dir, "stop").await;
    let deadline = tokio::time::Instant::now() + STOP_TIMEOUT;
    loop {
        tokio::time::sleep(POLL).await;
        match status_pid(data_dir).await {
            Err(CliError::Client(ClientError::Unreachable { .. })) => {
                return Ok("Background host stopped.".to_owned());
            }
            Err(error) => return Err(error),
            Ok(_) => {}
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(match ack {
                Err(error) => error,
                Ok(_) => CliError::Stop(format!(
                    "OpenButler host process {pid} did not stop. Its process identity cannot be verified, so it was not terminated."
                )),
            });
        }
    }
}

/// `openbutler daemon pair` — mint one-time code a remote client
/// redeems. The five-minute TTL lives daemon-side (`pair.create`), and
/// the redeem command in the hint is the source's canonical form.
pub async fn pair(data_dir: &Path) -> Result<String, CliError> {
    let result = commands::ask(data_dir, "pair.create").await?;
    let issued: IssuedPair = serde_json::from_value(result).map_err(|source| CliError::Decode {
        action: "pair.create",
        source,
    })?;
    // Local wall clock, the source's `toLocaleTimeString` equivalent;
    // an unconvertible instant falls back to the raw milliseconds
    // rather than a wrong zone.
    let expires = chrono::Local
        .timestamp_millis_opt(i64::try_from(issued.expires_at).unwrap_or(i64::MAX))
        .single()
        .map(|time| time.format("%H:%M:%S").to_string())
        .unwrap_or_else(|| issued.expires_at.to_string());
    Ok(format!(
        "Pairing code: {}\nExpires:      {}\nRedeem it on the client within 5 minutes: openbutler connect <host:port> --code {}",
        issued.code, expires, issued.code
    ))
}

/// `openbutler daemon start` — spawn the host (or report the one that
/// is already answering) and wait until its endpoint responds.
///
/// Divergences from the source, deliberate and listed: the
/// `defaultModelReady` guard is a settings/key/catalog computation that
/// belongs daemon-side (the owner's backend-owned rule) and this
/// build's status carries no model field yet, so start does not
/// pre-check; the `--listen`/`--web` flags and their settings writes
/// land with their own slices.
pub async fn start(
    data_dir: &Path,
    listen: Option<String>,
    no_listen: bool,
) -> Result<String, CliError> {
    let exe = resolve_daemon_exe()?;
    start_with(data_dir, &exe, listen, no_listen).await
}

/// Start with an explicit executable — the test seam for the spawn
/// paths (`main` goes through [`start`]'s resolution).
pub async fn start_with(
    data_dir: &Path,
    exe: &Path,
    listen: Option<String>,
    no_listen: bool,
) -> Result<String, CliError> {
    // One connection carries the whole start: the probe and the write.
    // The source could probe and write without a host (its files were the
    // CLI's); here both are the daemon's, so a single socket does the run.
    let connected = match crate::client::Client::connect(data_dir).await {
        Ok(client) => Some(client),
        Err(ClientError::Unreachable { .. }) => None,
        Err(error) => return Err(CliError::Client(error)),
    };
    if let Some(mut client) = connected {
        let status = decode_status(call_on(&mut client, "status", None).await?)?;
        let saved = persist_listen_on(&mut client, listen.as_deref(), no_listen).await?;
        return Ok(render_started(&status, saved.as_deref(), no_listen, true));
    }
    std::fs::create_dir_all(data_dir).map_err(|error| {
        CliError::Start(format!(
            "Could not prepare the data directory {}: {error}",
            data_dir.display()
        ))
    })?;

    let mut attempt_index = 0usize;
    loop {
        match spawn_and_wait(data_dir, exe, listen.as_deref(), no_listen).await {
            Ok(()) => break,
            Err(failure) => {
                // The source retries only a transient failure while a
                // backoff remains; anything else is the owner's answer.
                let backoff = STARTUP_RETRY_BACKOFF_MS.get(attempt_index).copied();
                match backoff {
                    Some(milliseconds) if failure.retryable => {
                        tokio::time::sleep(Duration::from_millis(milliseconds)).await;
                        attempt_index += 1;
                    }
                    _ => return Err(CliError::Start(failure.message)),
                }
            }
        }
    }
    // The source re-reads status after readiness; a host that answered
    // the probe answers this too. The cold-start flags reached the child
    // directly, and the same value is now the saved preference so every
    // later boot binds it as well (the source persisted before spawning;
    // the write is the daemon's here, so it lands once the host answers).
    let mut client = crate::client::Client::connect(data_dir).await?;
    let status = decode_status(call_on(&mut client, "status", None).await?)?;
    let saved = persist_listen_on(&mut client, listen.as_deref(), no_listen).await?;
    Ok(render_started(&status, saved.as_deref(), no_listen, false))
}

/// One request on an open connection, answered as the daemon's payload.
async fn call_on(
    client: &mut crate::client::Client,
    action: &str,
    payload: Option<(&str, serde_json::Value)>,
) -> Result<serde_json::Value, CliError> {
    let mut request =
        openbutler_protocol::DaemonRequest::new(uuid::Uuid::new_v4().to_string(), action);
    if let Some((key, value)) = payload {
        request.payload.insert(key.to_owned(), value);
    }
    match client.call(&request).await? {
        openbutler_protocol::DaemonResponse::Success { result, .. } => Ok(result),
        openbutler_protocol::DaemonResponse::Failure { error, .. } => Err(CliError::Answer(error)),
    }
}

fn decode_status(result: serde_json::Value) -> Result<openbutler_protocol::DaemonStatus, CliError> {
    serde_json::from_value(result).map_err(|source| CliError::Decode {
        action: "status",
        source,
    })
}

/// Saves or clears the listener on an open connection and answers the
/// stored value (`None` when nothing was persisted this run).
async fn persist_listen_on(
    client: &mut crate::client::Client,
    listen: Option<&str>,
    clear: bool,
) -> Result<Option<String>, CliError> {
    if listen.is_none() && !clear {
        return Ok(None);
    }
    let payload = match listen {
        Some(spec) => serde_json::json!(spec),
        None => serde_json::Value::Null,
    };
    let result = call_on(client, "settings.set-listen", Some(("listen", payload))).await?;
    Ok(result
        .get("listen")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned))
}

/// Spawn the child with `daemon.log` as its output, then poll: an early
/// death is diagnosed with the log's own words, a silent child past the
/// deadline is killed so a retry never races a half-started process for
/// the endpoint.
async fn spawn_and_wait(
    data_dir: &Path,
    exe: &Path,
    listen: Option<&str>,
    no_listen: bool,
) -> Result<(), StartupFailure> {
    use std::process::Stdio;

    let log_path = data_dir.join("daemon.log");
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|error| StartupFailure {
            message: format!(
                "Could not open the machine log {}: {error}",
                log_path.display()
            ),
            retryable: false,
        })?;
    let log_offset = log.metadata().map(|meta| meta.len()).unwrap_or(0);
    let stderr_handle = log.try_clone().map_err(|error| StartupFailure {
        message: format!(
            "Could not open the machine log {}: {error}",
            log_path.display()
        ),
        retryable: false,
    })?;

    let mut command = std::process::Command::new(exe);
    command
        .arg("run")
        .current_dir(data_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(stderr_handle));
    // The cold-start preference goes straight to the child: the host's
    // write-only state is the daemon's, so the flag cannot be persisted
    // before the host exists.
    if let Some(spec) = listen {
        command.arg("--listen").arg(spec);
    } else if no_listen {
        command.arg("--no-listen");
    }
    #[cfg(unix)]
    command.process_group(0);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // No console flash from a background host (the source's windowsHide).
        command.creation_flags(0x0800_0000);
    }
    let mut child = command.spawn().map_err(|error| StartupFailure {
        message: format!("Could not start the background host: {error}"),
        retryable: false,
    })?;

    let deadline = tokio::time::Instant::now() + START_TIMEOUT;
    loop {
        if let Some(exit) = child.try_wait().map_err(|error| StartupFailure {
            message: format!("Could not watch the background host: {error}"),
            retryable: false,
        })? {
            let tail = log_tail(&log_path, log_offset);
            return Err(diagnose("host", exit.code(), &tail));
        }
        match status_report(data_dir).await {
            Ok(_) => return Ok(()),
            Err(CliError::Client(ClientError::Unreachable { .. })) => {}
            Err(error) => {
                return Err(StartupFailure {
                    message: error.to_string(),
                    retryable: false,
                });
            }
        }
        if tokio::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            let tail = log_tail(&log_path, log_offset);
            let mut message = format!(
                "The host did not become ready within {} seconds.",
                START_TIMEOUT.as_secs()
            );
            if !tail.is_empty() {
                message.push('\n');
                message.push_str(&tail);
            }
            return Err(StartupFailure {
                message,
                retryable: false,
            });
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// The source's early-death diagnosis: an allocation failure names the
/// machine (and is retried), a fault that reproduces on every spawn is
/// answered at once, everything else retries with backoff.
fn diagnose(kind: &str, code: Option<i32>, tail: &str) -> StartupFailure {
    let exit_label = match code {
        Some(code) => format!("exit code {code}"),
        None => "unknown signal".to_owned(),
    };
    let tail_block = if tail.is_empty() {
        String::new()
    } else {
        format!("\n{tail}")
    };
    let lower = tail.to_ascii_lowercase();
    let out_of_memory = lower.contains("memory allocation of")
        || lower.contains("out of memory")
        || lower.contains("cannot allocate memory")
        || lower.contains("enomem")
        || code == Some(134);
    if out_of_memory {
        return StartupFailure {
            message: format!(
                "The {kind} exited before becoming ready ({exit_label}): the machine ran out of memory. \
                 Close other applications or enlarge the Windows pagefile, then start it again.{tail_block}"
            ),
            retryable: true,
        };
    }
    // Source patterns for a Node child, adapted for this one: module and
    // syntax faults cannot happen, a bind clash or a Rust panic can.
    let deterministic = [
        "address already in use",
        "socket address",
        "already running",
        "panicked at",
        "Usage:",
        "is not recognized",
    ]
    .iter()
    .any(|pattern| tail.contains(pattern));
    let hint = if tail.is_empty() {
        // The source's sentence, and now literally true: `logs` ships
        // in this CLI (it reads the very file the tail came from).
        "\nRead the machine log for the child's own message: openbutler logs --lines 40.".to_owned()
    } else {
        String::new()
    };
    StartupFailure {
        message: format!(
            "The {kind} exited before becoming ready ({exit_label}).{tail_block}{hint}"
        ),
        retryable: !deterministic,
    }
}

/// The source's `readDaemonLogSince`: at most the last 4000 bytes past
/// the spawn offset, trimmed, empty when the file cannot be read.
fn log_tail(path: &Path, offset: u64) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut file) = std::fs::File::open(path) else {
        return String::new();
    };
    let Ok(meta) = file.metadata() else {
        return String::new();
    };
    let size = meta.len();
    let start = offset.max(size.saturating_sub(4_000));
    let length = size - start;
    if length == 0 || file.seek(SeekFrom::Start(start)).is_err() {
        return String::new();
    }
    let mut buffer = Vec::new();
    if file.take(length).read_to_end(&mut buffer).is_err() {
        return String::new();
    }
    String::from_utf8_lossy(&buffer).trim().to_owned()
}

/// The daemon executable: `OPENBUTLER_DAEMON` first (dev and tests),
/// then the install layout where both binaries sit side by side.
fn resolve_daemon_exe() -> Result<std::path::PathBuf, CliError> {
    if let Some(over) = std::env::var_os("OPENBUTLER_DAEMON") {
        let path = std::path::PathBuf::from(over);
        if path.is_file() {
            return Ok(path);
        }
        return Err(CliError::Start(format!(
            "OPENBUTLER_DAEMON points at {}, which is not a file.",
            path.display()
        )));
    }
    if let Ok(current) = std::env::current_exe()
        && let Some(dir) = current.parent()
    {
        let mut name = std::ffi::OsString::from("openbutler-daemon");
        #[cfg(windows)]
        name.push(".exe");
        let sibling = dir.join(name);
        if sibling.is_file() {
            return Ok(sibling);
        }
        return Err(CliError::Start(format!(
            "Could not find the daemon executable next to {} (looked for {}). \
             Set OPENBUTLER_DAEMON to its path.",
            current.display(),
            sibling.display()
        )));
    }
    Err(CliError::Start(
        "Could not locate the daemon executable; set OPENBUTLER_DAEMON to its path.".to_owned(),
    ))
}

/// The start block: the source's lines, plain (this CLI has no color
/// chrome); the `entry` line stays conditional on the daemon sending
/// one (this daemon answers `entry: null`).
fn render_started(
    status: &openbutler_protocol::DaemonStatus,
    listener: Option<&str>,
    cleared: bool,
    was_running: bool,
) -> String {
    let mut text = String::new();
    text.push_str(
        "The machine custodian and its workspace runtime initialize when a TUI or Telegram turn arrives.",
    );
    text.push_str(&format!("\nBackground host running (pid {}).", status.pid));
    text.push_str(&format!(
        "\n{:<14} {}",
        "workspaces:", status.workspace_count
    ));
    if let Some(entry) = &status.entry {
        text.push_str(&format!("\n{:<14} {}", "entry:", entry));
    }
    // The source's positions (L1806-1810): the saved listener, the restart
    // note when the host was already up, then the cleared note.
    if let Some(spec) = listener {
        text.push_str(&format!(
            "\n{}",
            crate::ui::kv("Listener", &format!("{spec} (remote clients attach here)"))
        ));
        if was_running {
            text.push_str(&format!(
                "\n{}",
                crate::ui::info("The saved listener applies on the next `openbutler restart`.")
            ));
        }
    }
    if cleared {
        text.push_str(&format!(
            "\n{}",
            crate::ui::info("Remote listener cleared; this host binds no listener.")
        ));
    }
    text.push_str(
        "\nIt stays alive after this terminal closes. Stop it with `openbutler daemon stop`.",
    );
    text
}

/// The host's status decoded (the render, the stop flow and the start
/// readiness probe all speak through this).
async fn status_report(data_dir: &Path) -> Result<openbutler_protocol::DaemonStatus, CliError> {
    let result = commands::ask(data_dir, "status").await?;
    serde_json::from_value(result).map_err(|source| CliError::Decode {
        action: "status",
        source,
    })
}

/// The host's status reduced to the pid the stop flow tracks.
async fn status_pid(data_dir: &Path) -> Result<u32, CliError> {
    Ok(status_report(data_dir).await?.pid)
}
