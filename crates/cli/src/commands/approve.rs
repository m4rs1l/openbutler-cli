//! `openbutler approve [<n>]` — the auto-review denials, and the one-shot
//! retry a user grants from that list.
//!
//! The host composes the screen (`auto_review.approve`): the numbering, the
//! table, the empty note and the answer each approval prints. This module
//! sends the number and prints what came back.

use std::path::Path;

use serde::Deserialize;

use crate::CliError;

/// The host's answer: the finished screen lines.
#[derive(Deserialize)]
struct Answer {
    lines: Vec<crate::ui::ScreenLine>,
}

/// Asks the host and prints its lines.
pub async fn run(data_dir: &Path, index: Option<u32>) -> Result<String, CliError> {
    let mut client = crate::client::Client::connect(data_dir).await?;
    let mut fields: Vec<(&str, serde_json::Value)> = Vec::new();
    if let Some(index) = index {
        fields.push(("index", serde_json::Value::from(index)));
    }
    let result = super::daemon::call_fields(&mut client, "auto_review.approve", fields).await?;
    let answer: Answer = serde_json::from_value(result).map_err(|source| CliError::Decode {
        action: "auto_review.approve",
        source,
    })?;
    Ok(crate::ui::screen(&answer.lines))
}
