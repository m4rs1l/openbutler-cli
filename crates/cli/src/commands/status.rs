//! `openbutler status` — the daemon's own report, rendered line by line.

use std::path::Path;

use openbutler_protocol::DaemonStatus;
use openbutler_protocol::status::TelegramState;

use crate::CliError;

/// Fetches `status` and renders it.
pub async fn run(data_dir: &Path) -> Result<String, CliError> {
    let result = super::ask(data_dir, "status").await?;
    let status: DaemonStatus =
        serde_json::from_value(result).map_err(|source| CliError::Decode {
            action: "status",
            source,
        })?;
    Ok(render(&status))
}

/// Renders the status the host computed; no field is re-derived here.
fn render(status: &DaemonStatus) -> String {
    let mut lines = vec![
        line("pid", status.pid),
        line("started", &status.started_at),
        line("workspaces", status.workspace_count),
        line("clients", status.connected_clients),
    ];
    if let Some(schedules) = status.schedules {
        lines.push(line("schedules", schedules));
    }
    if let Some(telegram) = &status.telegram {
        let mut value = match telegram.state {
            TelegramState::Disabled => "disabled",
            TelegramState::Connecting => "connecting",
            TelegramState::Polling => "polling",
            TelegramState::Error => "error",
            TelegramState::Stopped => "stopped",
        }
        .to_owned();
        if let Some(username) = &telegram.username {
            value.push_str(&format!(" (@{username})"));
        }
        if let Some(error) = &telegram.error {
            value.push_str(&format!(" — {error}"));
        }
        lines.push(format!("{:<14} {value}", "telegram:"));
    }
    if status.unresponsive == Some(true) {
        lines.push(format!(
            "{:<14} yes — the daemon is alive but IPC is dead; counts above are unknown",
            "unresponsive:"
        ));
    }
    if let Some(entry) = &status.entry {
        lines.push(format!("{:<14} {entry}", "entry:"));
    }
    // The build-id trio: absent = an older host (no line), [null] = a
    // source run, ["id"] = a built vintage.
    match &status.build_id {
        None => {}
        Some(None) => lines.push(format!("{:<14} source", "build:")),
        Some(Some(build_id)) => lines.push(format!("{:<14} {build_id}", "build:")),
    }
    lines.join("\n")
}

fn line(label: &str, value: impl std::fmt::Display) -> String {
    let label = format!("{label}:");
    format!("{label:<14} {value}")
}
