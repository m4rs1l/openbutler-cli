//! `openbutler --list-models` — the models a tool-using turn can run on.
//!
//! The host composes the screen (`catalog.listToolModels`): the rows, the
//! count in the header, the slice and the narrowing sentence. This module
//! carries the three flags and prints what came back, so a client can never
//! disagree with the host about which model can call a tool.

use std::path::Path;

use serde::Deserialize;

use crate::CliError;

/// The host's answer: the finished screen plus the counts it printed.
#[derive(Deserialize)]
struct ModelList {
    lines: Vec<crate::ui::ScreenLine>,
}

/// Asks the host and prints its lines.
pub async fn list(
    data_dir: &Path,
    provider: Option<String>,
    filter: Option<String>,
    limit: Option<u32>,
) -> Result<String, CliError> {
    let mut fields: Vec<(&str, serde_json::Value)> = Vec::new();
    if let Some(provider) = provider {
        fields.push(("provider", serde_json::Value::String(provider)));
    }
    if let Some(filter) = filter {
        fields.push(("filter", serde_json::Value::String(filter)));
    }
    if let Some(limit) = limit {
        fields.push(("limit", serde_json::Value::from(limit)));
    }
    let mut client = crate::client::Client::connect(data_dir).await?;
    let result = super::daemon::call_fields(&mut client, "catalog.listToolModels", fields).await?;
    let list: ModelList = serde_json::from_value(result).map_err(|source| CliError::Decode {
        action: "catalog.listToolModels",
        source,
    })?;
    Ok(crate::ui::screen(&list.lines))
}
