//! `openbutler permissions [mode] [--review-timeout N] [--approval-timeout N]
//! [--review-model P/M|none]` — the approval posture.
//!
//! Source: `runPermissions` (cli-main.ts L215-309). The source wrote settings
//! itself, pushed only the mode to a live host and composed the screen; here
//! one `permissions.manage` call carries every provided flag and the host
//! answers the finished screen lines (values, explanations, the success line
//! per flag this call carried, the restart hint). This module prints them and
//! applies its own glyph palette — it makes no decision.
//!
//! The auto-review history section is not ported: no auto-review verdicts are
//! recorded in this build (the runtime that writes them arrives with the
//! tool-approval slice).

use std::path::Path;

use serde::Deserialize;
use serde_json::Value;

use crate::CliError;

/// The host's answer: the finished lines.
#[derive(Deserialize)]
struct PermissionsReport {
    lines: Vec<crate::ui::ScreenLine>,
}

/// Runs the command and prints the host's screen.
pub async fn run(
    data_dir: &Path,
    mode: Option<String>,
    review_timeout: Option<u64>,
    approval_timeout: Option<u64>,
    review_model: Option<String>,
) -> Result<String, CliError> {
    let mut fields: Vec<(&str, Value)> = Vec::new();
    if let Some(mode) = &mode {
        fields.push(("mode", Value::String(mode.clone())));
    }
    if let Some(seconds) = review_timeout {
        fields.push(("reviewTimeoutSeconds", Value::from(seconds)));
    }
    if let Some(seconds) = approval_timeout {
        fields.push(("approvalTimeoutSeconds", Value::from(seconds)));
    }
    if let Some(model) = &review_model {
        fields.push(("reviewModel", Value::String(model.clone())));
    }

    let mut client = crate::client::Client::connect(data_dir).await?;
    let result =
        crate::commands::daemon::call_fields(&mut client, "permissions.manage", fields).await?;
    let report: PermissionsReport =
        serde_json::from_value(result).map_err(|source| CliError::Decode {
            action: "permissions.manage",
            source,
        })?;
    Ok(crate::ui::screen(&report.lines))
}
