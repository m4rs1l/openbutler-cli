//! `openbutler sessions` — the daemon's session list, one line per row, and
//! `sessions prune`, where the host selects, formats and deletes its own
//! records.
//!
//! Source: `cli-main.ts` `runSessionsCommand` (L1465-1526). The source did
//! the selection, the byte formatting and the deletion in the CLI process;
//! here the host owns the files, so this module only prints the lines the
//! host composed.

use std::path::Path;

use openbutler_protocol::SessionListAnswer;
use serde::Deserialize;
use serde_json::json;

use crate::CliError;
use crate::SessionsCommand;

/// Runs the bare list or the prune subcommand.
pub async fn run(data_dir: &Path, command: Option<SessionsCommand>) -> Result<String, CliError> {
    match command {
        None => list(data_dir).await,
        Some(SessionsCommand::Prune { older_than, yes }) => prune(data_dir, &older_than, yes).await,
    }
}

/// Fetches `session.list` and prints the host's screen.
async fn list(data_dir: &Path) -> Result<String, CliError> {
    let result = super::ask(data_dir, "session.list").await?;
    let answer: SessionListAnswer =
        serde_json::from_value(result).map_err(|source| CliError::Decode {
            action: "session.list",
            source,
        })?;
    Ok(crate::ui::screen(&answer.lines))
}

/// The host's prune answer: finished lines, each with the tone it chose.
#[derive(Deserialize)]
struct PruneAnswer {
    lines: Vec<crate::ui::ScreenLine>,
}

/// Asks the host to prune and prints exactly what it answers.
async fn prune(data_dir: &Path, older_than: &str, yes: bool) -> Result<String, CliError> {
    let mut client = crate::client::Client::connect(data_dir).await?;
    let result = crate::commands::daemon::call_fields(
        &mut client,
        "sessions.prune",
        vec![("olderThan", json!(older_than)), ("apply", json!(yes))],
    )
    .await?;
    let answer: PruneAnswer =
        serde_json::from_value(result).map_err(|source| CliError::Decode {
            action: "sessions.prune",
            source,
        })?;
    Ok(crate::ui::screen(&answer.lines))
}
