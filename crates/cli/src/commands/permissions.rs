//! `openbutler permissions [mode] [--review-timeout N] [--approval-timeout N]
//! [--review-model P/M|none]` — the approval posture.
//!
//! Source: `runPermissions` (cli-main.ts L215-309, read 2026-11-26). The
//! source wrote settings itself and pushed only the mode to a live host;
//! here one `permissions.manage` call carries every provided flag and the
//! daemon answers the four display values this module lays out. The
//! auto-review history section is not ported: no auto-review verdicts are
//! recorded in this build (the runtime that writes them arrives with the
//! tool-approval slice).

use std::path::Path;

use serde_json::Value;

use crate::CliError;

/// The daemon's answer: display-ready values plus what this call changed.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct PermissionsReport {
    mode: String,
    review_timeout: String,
    approval_timeout: String,
    review_model: String,
    saved: Vec<String>,
}

/// Runs the command and renders the screen.
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

    // The source's order (L283-307): the four values, the two explanations,
    // then one success line per flag this call carried.
    let mut lines = vec![
        crate::ui::kv("Current mode", &report.mode),
        crate::ui::kv("Auto-review timeout", &report.review_timeout),
        crate::ui::kv("Approval timeout", &report.approval_timeout),
        crate::ui::kv("Auto-review model", &report.review_model),
        crate::ui::info(
            "Workspace paths run without prompts. Auto-review asks the selected model about actions outside the workspace.",
        ),
        crate::ui::info(
            "Use `openbutler permissions full-access` to disable tool permission prompts.",
        ),
    ];
    if mode.is_some() && report.saved.iter().any(|saved| saved == "mode") {
        lines.push(crate::ui::success(&format!("Saved {} mode.", report.mode)));
    }
    if let Some(seconds) = review_timeout
        && report.saved.iter().any(|saved| saved == "reviewTimeout")
    {
        lines.push(crate::ui::success(&format!(
            "Auto-review timeout set to {seconds} seconds."
        )));
    }
    if let Some(seconds) = approval_timeout
        && report.saved.iter().any(|saved| saved == "approvalTimeout")
    {
        lines.push(crate::ui::success(&format!(
            "Approval timeout set to {seconds} seconds."
        )));
    }
    let cleared_model = review_model
        .as_deref()
        .is_some_and(|value| value == "none" || value.is_empty());
    if cleared_model && report.saved.iter().any(|saved| saved == "reviewModel") {
        lines.push(crate::ui::success("Auto-review will reuse the main model."));
    } else if review_model.is_some() && report.saved.iter().any(|saved| saved == "reviewModel") {
        lines.push(crate::ui::success(&format!(
            "Auto-review model set to {}.",
            report.review_model
        )));
    }
    if review_model.is_some() {
        // The source's sentence named its top-level `openbutler restart`;
        // this surface has only the explicit subcommand (a documented
        // divergence, same as the listener hint).
        lines.push(crate::ui::info(
            "Restart the host for the reviewer model to take effect: openbutler daemon restart",
        ));
    }
    Ok(lines.join("\n"))
}
