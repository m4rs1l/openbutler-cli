//! `openbutler schedule`: the host's scheduler surface.
//!
//! Source: `cli-main.ts` (`runSchedule`, L1039). The list and log screens,
//! the empty sentences and every state word are the host's; this module
//! prints the answered lines and, for `capabilities`, the host's payload as
//! the JSON the source printed. Subcommands that need guided answers
//! (`add`/`update`/`open`) are not ported yet — documented divergence.

use std::path::Path;

use serde_json::{Value, json};

use crate::CliError;
use crate::client::Client;
use crate::commands::daemon::call_fields;
use crate::ui;

/// Runs one `schedule` subcommand.
pub async fn run(data_dir: &Path, words: &[String]) -> Result<String, CliError> {
    let action = words.first().map(String::as_str).unwrap_or("list");
    match action {
        "list" => {
            let result = call(data_dir, vec![("action", json!("list"))]).await?;
            Ok(screen(&result))
        }
        "log" => {
            let Some(id) = words.get(1) else {
                return Err(CliError::Answer(
                    "Usage: openbutler schedule log <id>".to_owned(),
                ));
            };
            let result = call(
                data_dir,
                vec![("action", json!("log")), ("scheduleId", json!(id))],
            )
            .await?;
            Ok(screen(&result))
        }
        "capabilities" => {
            let result = call(data_dir, vec![("action", json!("capabilities"))]).await?;
            serde_json::to_string_pretty(&result).map_err(|source| CliError::Decode {
                action: "schedule",
                source,
            })
        }
        other => Err(CliError::Answer(format!(
            "Usage: openbutler schedule [list|log <id>|capabilities] (got {other})"
        ))),
    }
}

/// One `schedule.manage` call with its payload fields.
async fn call(data_dir: &Path, fields: Vec<(&str, Value)>) -> Result<Value, CliError> {
    let mut client = Client::connect(data_dir).await?;
    call_fields(&mut client, "schedule.manage", fields).await
}

/// The host's lines as text, painted through this client's palette.
fn screen(result: &Value) -> String {
    let lines: Vec<ui::ScreenLine> =
        serde_json::from_value(result["lines"].clone()).unwrap_or_default();
    lines
        .iter()
        .map(|line| ui::toned(line.tone, &line.text))
        .collect::<Vec<_>>()
        .join("\n")
}
