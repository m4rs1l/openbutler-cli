//! `openbutler status` — the daemon's own report, composed by the host.
//!
//! The host answers `DaemonStatus` with the finished screen (`lines`): the
//! pid, the start time, the workspace and client counts, the schedule and
//! telegram rows when the host reports them, the unresponsive note, the
//! entry path, and the build trio. This module prints those lines through
//! its glyph palette and decides nothing.

use std::path::Path;

use openbutler_protocol::DaemonStatus;

use crate::CliError;

/// Fetches `status` and prints the host's screen.
pub async fn run(data_dir: &Path) -> Result<String, CliError> {
    let result = super::ask(data_dir, "status").await?;
    let status: DaemonStatus =
        serde_json::from_value(result).map_err(|source| CliError::Decode {
            action: "status",
            source,
        })?;
    if status.lines.is_empty() {
        // A host that predates the screen: nothing to print is honest, but a
        // silent status would look like a bug, so say what happened.
        return Ok("This host does not send a status screen; update it and try again.".to_owned());
    }
    Ok(crate::ui::screen(&status.lines))
}
