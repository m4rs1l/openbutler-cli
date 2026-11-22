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

/// The host's status reduced to the pid the stop flow tracks.
async fn status_pid(data_dir: &Path) -> Result<u32, CliError> {
    let result = commands::ask(data_dir, "status").await?;
    let status: openbutler_protocol::DaemonStatus =
        serde_json::from_value(result).map_err(|source| CliError::Decode {
            action: "status",
            source,
        })?;
    Ok(status.pid)
}
