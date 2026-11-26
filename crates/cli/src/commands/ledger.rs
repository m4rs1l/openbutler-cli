//! `openbutler events` / `openbutler actions` — the durable activity ledger.
//!
//! Source: `runActivityLedger` / `runLedgerPrune` (cli-main.ts L1556-1610,
//! read 2026-11-26). The list and retry paths print the daemon's rows as
//! pretty JSON, exactly where the source printed its own query output;
//! `prune` asks the daemon (`ledger.prune`) and lays out the counts it
//! answers. The source refused to prune while the host was running because
//! it wrote the database itself; the host is the writer here, so the prune
//! runs inside it.

use std::path::Path;

use serde_json::Value;

use crate::{ActionsCommand, CliError, EventsCommand};

/// `events`: list, `retry <id>`, or `prune --older-than <d> [--yes]`.
pub async fn events(data_dir: &Path, command: Option<EventsCommand>) -> Result<String, CliError> {
    match command {
        None => pretty(&crate::commands::ask(data_dir, "ledger.events").await?),
        Some(EventsCommand::Retry { event_id }) => {
            let result = crate::commands::ask_with(
                data_dir,
                "events.retry",
                ("eventId", Value::String(event_id)),
            )
            .await?;
            pretty(&result)
        }
        Some(EventsCommand::Prune { older_than, yes }) => {
            prune(data_dir, "events", &older_than, yes).await
        }
    }
}

/// `actions`: list or `prune --older-than <d> [--yes]`.
pub async fn actions(data_dir: &Path, command: Option<ActionsCommand>) -> Result<String, CliError> {
    match command {
        None => pretty(&crate::commands::ask(data_dir, "ledger.actions").await?),
        Some(ActionsCommand::Prune { older_than, yes }) => {
            prune(data_dir, "actions", &older_than, yes).await
        }
    }
}

/// `JSON.stringify(rows, null, 2)` — the source's own dump shape.
fn pretty(value: &Value) -> Result<String, CliError> {
    serde_json::to_string_pretty(value).map_err(|error| CliError::Answer(error.to_string()))
}

/// The prune half: one `ledger.prune` call, then the source's three lines.
async fn prune(
    data_dir: &Path,
    kind: &'static str,
    older_than: &str,
    yes: bool,
) -> Result<String, CliError> {
    let mut client = crate::client::Client::connect(data_dir).await?;
    let result = crate::commands::daemon::call_fields(
        &mut client,
        "ledger.prune",
        vec![
            ("kind", Value::String(kind.to_owned())),
            ("olderThan", Value::String(older_than.to_owned())),
            ("apply", Value::Bool(yes)),
        ],
    )
    .await?;
    let events = result.get("events").and_then(Value::as_u64).unwrap_or(0);
    let actions = result.get("actions").and_then(Value::as_u64).unwrap_or(0);
    let transitions = result
        .get("transitions")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let total = events + actions + transitions;
    if total == 0 {
        return Ok(crate::ui::info(&format!(
            "No {kind} records are older than {older_than}; nothing to prune."
        )));
    }
    let mut lines = vec![format!(
        "  {events} event(s), {actions} action(s), {transitions} transition(s)"
    )];
    if yes {
        lines.push(crate::ui::success(&format!("Pruned {total} record(s).")));
    } else {
        lines.push(crate::ui::info(&format!(
            "Nothing was deleted; re-run with --yes to delete these {total} record(s)."
        )));
    }
    Ok(lines.join("\n"))
}
