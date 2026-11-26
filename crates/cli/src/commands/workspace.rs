//! `openbutler workspace default [path]` — the workspace bare launches
//! use, stored by the host (`settings.set-default-workspace`).
//!
//! Source: `cli-main.ts` `runWorkspaceCommand` (L340-352): no path shows
//! the stored root (or the "none" sentence), a path canonicalizes on the
//! host and answers the saved root. The path rule lives in the daemon's
//! single normalizer; this command never touches a file.

use std::path::Path;

use serde_json::Value;

use crate::CliError;
use crate::ui;

/// The host's answer: the stored root, or `null`.
const ACTION: &str = "settings.set-default-workspace";

/// Queries or saves the default workspace and renders the source's line.
pub async fn run(data_dir: &Path, path: Option<String>) -> Result<String, CliError> {
    let mut fields: Vec<(&str, Value)> = Vec::new();
    if let Some(path) = &path {
        fields.push(("path", Value::from(path.clone())));
    }
    let mut client = crate::client::Client::connect(data_dir).await?;
    let result = super::daemon::call_fields(&mut client, ACTION, fields).await?;
    let workspace = result.get("workspace").cloned().unwrap_or(Value::Null);
    Ok(match (workspace.as_str(), path.is_some()) {
        (Some(root), true) => ui::success(&format!("Bare OpenButler launches will use {root}.")),
        (Some(root), false) => ui::kv("Default workspace", root),
        (None, _) => ui::kv(
            "Default workspace",
            "(none; bare OpenButler uses the launch directory)",
        ),
    })
}
