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

/// `openbutler daemon restart` — recycle the host.
///
/// Source: the one restart flow shared by `openbutler restart` and
/// `openbutler daemon restart` (cli-main.ts L1669-1691, daemon.ts L414),
/// read 2026-11-24. Divergence, deliberate: this build has no browser surface
/// at all - the daemon serves an API, not a UI - so the source's
/// browser-server lines have nothing to report.
pub async fn restart(data_dir: &Path) -> Result<String, CliError> {
    let exe = resolve_daemon_exe()?;
    restart_with(data_dir, &exe).await
}

/// Restart with an explicit executable (the test seam, like [`start_with`]).
pub async fn restart_with(data_dir: &Path, exe: &Path) -> Result<String, CliError> {
    // The source's first line goes out before the stop (cli-main.ts L1675).
    println!("Stopping active runs and saving sessions…");
    stop_for_restart(data_dir).await?;
    // A restart keeps the saved listener and records nothing new: the boot
    // binds the stored preference, exactly like a flagless start.
    let started = start_status(data_dir, exe, None, false).await?;
    Ok(render_restarted(started.status.pid))
}

/// The source's two closing lines (cli-main.ts L1677, L1683).
fn render_restarted(pid: u32) -> String {
    format!("Background host restarted (pid {pid}).\nOpen sessions reconnect automatically.")
}

/// `openbutler daemon run` — the host in the foreground.
///
/// Source: `runDaemonProcess` (daemon.ts L607, read 2026-11-24). In this
/// split-binary stack the host is its own executable: the CLI runs it with
/// inherited stdio and hands back its exit status. The start lock, the
/// listener validation and the serve loop all stay daemon-side, so a
/// second foreground host gets the daemon's own refusal sentence on stderr
/// and this process exits with the daemon's code.
pub async fn run_foreground(
    data_dir: &Path,
    listen: Option<String>,
    no_listen: bool,
) -> Result<i32, CliError> {
    let exe = resolve_daemon_exe()?;
    run_foreground_with(data_dir, &exe, listen, no_listen).await
}

/// Foreground run with an explicit executable (the test seam).
pub async fn run_foreground_with(
    data_dir: &Path,
    exe: &Path,
    listen: Option<String>,
    no_listen: bool,
) -> Result<i32, CliError> {
    use std::process::Stdio;
    let mut command = std::process::Command::new(exe);
    command.arg("run");
    // The child must resolve the same data directory this CLI resolved.
    command.env("OPENBUTLER_DATA_DIR", data_dir);
    if let Some(spec) = listen {
        command.arg("--listen").arg(spec);
    }
    if no_listen {
        command.arg("--no-listen");
    }
    command
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    let status = command.status().map_err(|error| {
        CliError::Start(format!(
            "Could not start the background host {}: {error}",
            exe.display()
        ))
    })?;
    // A signal-terminated child has no code; the failure class is exit 1.
    Ok(status.code().unwrap_or(1))
}

/// The stop half of a restart: the same wait as [`stop`], asking with the
/// source's `restart` flag (daemon.ts L369) so every other client hears the
/// restart wording. A host that is not running is fine — there is nothing to
/// recycle.
async fn stop_for_restart(data_dir: &Path) -> Result<(), CliError> {
    // One connection carries the probe and the ask, the same shape as
    // \`start_with\`: the host can never leave between the two and turn the
    // ask into a silent no-op.
    let mut client = match crate::client::Client::connect_local(data_dir).await {
        Ok(client) => client,
        Err(ClientError::Unreachable { .. }) => return Ok(()),
        Err(error) => return Err(CliError::Client(error)),
    };
    let pid = decode_status(call_on(&mut client, "status", None).await?)?.pid;
    let ack = call_on(
        &mut client,
        "stop",
        Some(("restart", serde_json::Value::Bool(true))),
    )
    .await;
    drop(client);
    let deadline = tokio::time::Instant::now() + STOP_TIMEOUT;
    loop {
        tokio::time::sleep(POLL).await;
        match status_pid(data_dir).await {
            Err(CliError::Client(ClientError::Unreachable { .. })) => return Ok(()),
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

/// `openbutler daemon start` — spawn the host (or report the one that
/// is already answering) and wait until its endpoint responds.
///
/// Divergences from the source, deliberate and listed: the
/// `defaultModelReady` guard is a settings/key/catalog computation that
/// belongs daemon-side (the owner's backend-owned rule) and this
/// build's status carries no model field yet, so start does not
/// pre-check; the `--listen` flag and its settings write land with their
/// own slices.
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
    let started = start_status(data_dir, exe, listen, no_listen).await?;
    Ok(render_started(
        &started.status,
        started.saved.as_deref(),
        no_listen,
        started.was_running,
    ))
}

/// What a successful start found: the report, the listener this run
/// persisted, and whether the host was already answering.
struct Started {
    status: openbutler_protocol::DaemonStatus,
    saved: Option<String>,
    was_running: bool,
}

async fn start_status(
    data_dir: &Path,
    exe: &Path,
    listen: Option<String>,
    no_listen: bool,
) -> Result<Started, CliError> {
    // One connection carries the whole start: the probe and the write.
    // The source could probe and write without a host (its files were the
    // CLI's); here both are the daemon's, so a single socket does the run.
    let connected = match crate::client::Client::connect_local(data_dir).await {
        Ok(client) => Some(client),
        Err(ClientError::Unreachable { .. }) => None,
        Err(error) => return Err(CliError::Client(error)),
    };
    if let Some(mut client) = connected {
        let status = decode_status(call_on(&mut client, "status", None).await?)?;
        let saved = persist_listen_on(&mut client, listen.as_deref(), no_listen).await?;
        return Ok(Started {
            status,
            saved,
            was_running: true,
        });
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
    let mut client = crate::client::Client::connect_local(data_dir).await?;
    let status = decode_status(call_on(&mut client, "status", None).await?)?;
    let saved = persist_listen_on(&mut client, listen.as_deref(), no_listen).await?;
    Ok(Started {
        status,
        saved,
        was_running: false,
    })
}

/// One request on an open connection, answered as the daemon's payload.
async fn call_on(
    client: &mut crate::client::Client,
    action: &str,
    payload: Option<(&str, serde_json::Value)>,
) -> Result<serde_json::Value, CliError> {
    let fields: Vec<(&str, serde_json::Value)> = payload.into_iter().collect();
    call_fields(client, action, fields).await
}

/// One request with any number of payload fields (the envelope flattens
/// them), answered as the daemon's payload.
pub(crate) async fn call_fields(
    client: &mut crate::client::Client,
    action: &str,
    fields: Vec<(&str, serde_json::Value)>,
) -> Result<serde_json::Value, CliError> {
    require_declared(action)?;
    let mut request =
        openbutler_protocol::DaemonRequest::new(uuid::Uuid::new_v4().to_string(), action);
    for (key, value) in fields {
        request.payload.insert(key.to_owned(), value);
    }
    match client.call(&request).await? {
        openbutler_protocol::DaemonResponse::Success { result, .. } => Ok(result),
        openbutler_protocol::DaemonResponse::Failure { error, .. } => Err(CliError::Answer(error)),
    }
}

/// Refuses a name the protocol's catalog does not declare. The host would
/// answer "unknown action" too, but only after a dial and a frame; a client
/// typo should not cost a round trip, and a name that is not in the contract
/// is a client bug rather than a host condition.
fn require_declared(action: &str) -> Result<(), CliError> {
    if openbutler_protocol::is_known(action) {
        return Ok(());
    }
    Err(CliError::Answer(format!(
        "The client asked for an action the contract does not declare: {action}"
    )))
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

    let mut command = background_command(data_dir, exe, listen, no_listen);
    command
        .current_dir(data_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(stderr_handle));

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Its own process group: a signal aimed at the CLI must not take
        // the host down with it.
        command.process_group(0);
    }
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

/// The background child's command line: the host verb, the cold-start
/// preference, and the data directory this CLI resolved. The child inherits
/// the environment, so the explicit value is what keeps an overridden
/// directory authoritative - the same rule `run_foreground_with` follows.
fn background_command(
    data_dir: &Path,
    exe: &Path,
    listen: Option<&str>,
    no_listen: bool,
) -> std::process::Command {
    let mut command = std::process::Command::new(exe);
    command.arg("run").env("OPENBUTLER_DATA_DIR", data_dir);
    // The cold-start preference goes straight to the child: the host's
    // write-only state is the daemon's, so the flag cannot be persisted
    // before the host exists.
    if let Some(spec) = listen {
        command.arg("--listen").arg(spec);
    } else if no_listen {
        command.arg("--no-listen");
    }
    command
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
pub(crate) fn resolve_daemon_exe() -> Result<std::path::PathBuf, CliError> {
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
        let name = std::ffi::OsString::from("openbutlerd");
        #[cfg(windows)]
        let name = {
            let mut name = name;
            name.push(".exe");
            name
        };
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
    text.push_str("The machine custodian starts when a TUI turn arrives.");
    text.push_str(&format!("\nBackground host running (pid {}).", status.pid));
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
            // The source said `openbutler restart` (cli-main.ts L1808); this
            // surface has only the explicit `daemon restart`, so the hint
            // names a command that exists (a divergence, documented).
            text.push_str(&format!(
                "\n{}",
                crate::ui::info(
                    "The saved listener applies on the next `openbutler daemon restart`."
                )
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

/// Registers the login entry that starts the host at sign-in.
///
/// The entry runs `openbutlerd`, never this client: a login must not open a
/// terminal window, and the host is the thing that outlives the session.
pub fn install() -> Result<String, CliError> {
    let entry = crate::autostart::Entry::new(resolve_daemon_exe()?);
    crate::autostart::install(&entry)
}

/// Removes the login entry. Idempotent: nothing to remove is still success,
/// and an entry that belongs to another command is left alone.
pub fn uninstall() -> Result<String, CliError> {
    crate::autostart::uninstall(&resolve_daemon_exe()?)
}

#[cfg(test)]
mod tests {
    use super::render_restarted;

    /// The background child must resolve the same data directory this CLI
    /// resolved, and its cold-start flags must reach it as arguments.
    #[test]
    fn the_background_child_gets_this_data_directory_and_flags() {
        use std::ffi::OsStr;

        let dir = std::path::Path::new("C:\\fixture\\data");
        let exe = std::path::Path::new("openbutlerd.exe");
        let command = super::background_command(dir, exe, Some("127.0.0.1:7777"), false);
        let envs: Vec<(&OsStr, Option<&OsStr>)> = command.get_envs().collect();
        assert!(
            envs.iter().any(|(key, value)| *key == "OPENBUTLER_DATA_DIR"
                && *value == Some(OsStr::new("C:\\fixture\\data"))),
            "the child must be pointed at this CLI's data directory: {envs:?}"
        );
        let args: Vec<String> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args, vec!["run", "--listen", "127.0.0.1:7777"]);

        let cleared = super::background_command(dir, exe, None, true);
        let args: Vec<String> = cleared
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args, vec!["run", "--no-listen"]);
    }

    /// The two closing lines (cli-main.ts L1677/L1683); reaching them
    /// through the command needs a live host, so the text is pinned here.
    #[test]
    fn the_restart_render_uses_the_source_lines() {
        assert_eq!(
            render_restarted(4242),
            "Background host restarted (pid 4242).\nOpen sessions reconnect automatically."
        );
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod catalog_tests {
    use super::require_declared;

    #[test]
    fn a_declared_action_is_let_through() {
        require_declared("session.search").expect("session.search is in the contract");
        require_declared("status").expect("status is in the contract");
    }

    #[test]
    fn a_typo_is_refused_before_the_dial() {
        let error = require_declared("session.serach").expect_err("a typo must not go out");
        assert!(
            error.to_string().contains("the contract does not declare"),
            "the sentence must name the cause: {error}"
        );
    }
}
