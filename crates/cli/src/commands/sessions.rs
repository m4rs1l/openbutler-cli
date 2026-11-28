//! `openbutler sessions` — the daemon's session list, one line per row, and
//! `sessions prune`, where the host selects, formats and deletes its own
//! records.
//!
//! Source: `cli-main.ts` `runSessionsCommand` (L1465-1526). The source did
//! the selection, the byte formatting and the deletion in the CLI process;
//! here the host owns the files, so this module only prints the lines the
//! host composed.

use std::path::Path;

use openbutler_protocol::{SessionListAnswer, SessionSearchAnswer};
use serde::Deserialize;
use serde_json::json;

use crate::CliError;
use crate::SessionsCommand;

/// Runs the bare list or the prune subcommand.
pub async fn run(data_dir: &Path, command: Option<SessionsCommand>) -> Result<String, CliError> {
    match command {
        None => list(data_dir).await,
        Some(SessionsCommand::Prune { older_than, yes }) => prune(data_dir, &older_than, yes).await,
        Some(SessionsCommand::Search {
            query,
            limit,
            offset,
            include_outputs,
            json,
        }) => search(data_dir, &query, limit, offset, include_outputs, json).await,
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

/// Asks the host to search the derived index and prints its screen, or the
/// raw answer with `--json`; the CLI decides neither the rows nor the words.
async fn search(
    data_dir: &Path,
    query: &str,
    limit: Option<i64>,
    offset: Option<i64>,
    include_outputs: bool,
    raw: bool,
) -> Result<String, CliError> {
    let mut fields: Vec<(&str, serde_json::Value)> = vec![("query", json!(query))];
    if let Some(value) = limit {
        fields.push(("limit", json!(value)));
    }
    if let Some(value) = offset {
        fields.push(("offset", json!(value)));
    }
    if include_outputs {
        fields.push(("includeOutputs", json!(true)));
    }
    let mut client = crate::client::Client::connect(data_dir).await?;
    let result =
        crate::commands::daemon::call_fields(&mut client, "session.search", fields).await?;
    let answer: SessionSearchAnswer =
        serde_json::from_value(result).map_err(|source| CliError::Decode {
            action: "session.search",
            source,
        })?;
    if raw {
        return serde_json::to_string_pretty(&answer).map_err(|source| CliError::Decode {
            action: "session.search",
            source,
        });
    }
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
