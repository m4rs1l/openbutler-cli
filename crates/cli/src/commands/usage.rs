//! `openbutler usage` — the daemon's finished report, printed as it
//! arrives.
//!
//! The rows and every number on this screen belong to the host: it reads
//! its own `state.db` read-only and answers with the label, the summary,
//! the day buckets and the rendered lines (`usage.report`). This command
//! forwards the filters and places the answer — it never opens a
//! database, and it never recomputes a number.

use std::path::Path;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::CliError;

/// The daemon's answer. `label`/`summary`/`days` ride through untouched
/// for `--json` (the source envelope); `lines` is what a plain run
/// prints.
#[derive(Deserialize)]
struct Report {
    label: Value,
    summary: Value,
    days: Value,
    lines: Vec<String>,
}

/// Asks the host and prints its answer; `--json` prints the machine
/// envelope, still the host's numbers.
pub async fn run(
    data_dir: &Path,
    days: u32,
    session: Option<String>,
    model: Option<String>,
    json: bool,
) -> Result<String, CliError> {
    let mut fields: Vec<(&str, Value)> = vec![("days", Value::from(days))];
    if let Some(session) = &session {
        fields.push(("session", Value::from(session.clone())));
    }
    if let Some(model) = &model {
        fields.push(("model", Value::from(model.clone())));
    }
    let mut client = crate::client::Client::connect(data_dir).await?;
    let result = super::daemon::call_fields(&mut client, "usage.report", fields).await?;
    let report: Report = serde_json::from_value(result).map_err(|source| CliError::Decode {
        action: "usage report",
        source,
    })?;
    if json {
        let envelope = json!({
            "label": report.label,
            "summary": report.summary,
            "days": report.days,
        });
        return serde_json::to_string_pretty(&envelope).map_err(|source| CliError::Decode {
            action: "usage report",
            source,
        });
    }
    Ok(report.lines.join("\n"))
}
