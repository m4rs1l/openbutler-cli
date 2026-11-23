//! `openbutler config` — the manager screen: what is set up, which provider
//! is the default, and what still stands between this machine and a run.
//!
//! Source: `cli-main.ts` `runConfig` (L169-213, read 2026-11-24). Every
//! value comes from the daemon (`config.report`): labels, counts, the
//! default pair and the readiness sentences are decisions made there, and
//! this module only lays them out. Divergences, both recorded in
//! `docs/surface.md`:
//! - the source awaited the catalog download inside the command and printed
//!   real tool counts on the first run; our daemon starts the download and
//!   answers at once, so a first run shows `unknown` and the extra line
//!   below says why;
//! - the no-default hint names `openbutler run` (owner ruling: the verb is
//!   `run`, not `--ask`);
//! - provider rows follow the stored `providerOrder`; when no order was ever
//!   recorded they list id-sorted, because our settings maps are sorted and
//!   the source's YAML insertion order is not recoverable from them.

use std::path::Path;

use serde::Deserialize;

use crate::CliError;
use crate::ui;

/// The daemon's report, decoded field for field (nothing is recomputed).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConfigReport {
    mcp_servers: Vec<McpRow>,
    default_workspace: Option<String>,
    tool_access: String,
    has_providers: bool,
    catalog_loaded: bool,
    catalog_loading: bool,
    providers: Vec<ProviderRow>,
    default: Option<DefaultRow>,
    reasoning: String,
    show_reasoning: bool,
    readiness: Readiness,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct McpRow {
    id: String,
    transport: String,
    endpoint: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProviderRow {
    id: String,
    enabled: bool,
    key_count: u64,
    tool_capable_count: Option<u64>,
    is_default: bool,
    custom: bool,
    default_model: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DefaultRow {
    provider_id: String,
    model_id: String,
    source: String,
}

#[derive(Debug, Deserialize)]
struct Readiness {
    ready: bool,
    problems: Vec<String>,
    hints: Vec<String>,
}

/// Fetches and renders the report.
pub async fn run(data_dir: &Path) -> Result<String, CliError> {
    let result = super::ask(data_dir, "config.report").await?;
    let report: ConfigReport =
        serde_json::from_value(result).map_err(|source| CliError::Decode {
            action: "config.report",
            source,
        })?;
    Ok(render(&report))
}

/// The source's line order, exactly (see the module docs for the two
/// deliberate additions).
fn render(report: &ConfigReport) -> String {
    let mut lines: Vec<String> = Vec::new();

    // `showMcpServers` (cli-main.ts L380): the section, the rows, the hint.
    lines.push(String::new());
    lines.push(ui::section("MCP servers"));
    if report.mcp_servers.is_empty() {
        lines.push(ui::info("No MCP servers configured."));
    } else {
        let rows: Vec<Vec<String>> = report
            .mcp_servers
            .iter()
            .map(|row| vec![row.id.clone(), row.transport.clone(), row.endpoint.clone()])
            .collect();
        lines.extend(ui::table(&["SERVER", "TRANSPORT", "ENDPOINT"], &rows));
    }
    lines.push(ui::info(
        "Manage global servers with openbutler mcp. Credentials are environment-variable references, not saved values.",
    ));
    lines.push(String::new());

    lines.push(ui::kv(
        "Default workspace",
        report
            .default_workspace
            .as_deref()
            .unwrap_or("(choose when opening OpenButler)"),
    ));
    lines.push(ui::kv("Tool access", &report.tool_access));

    // The source stops here on an empty machine (L174-180): no catalog
    // load, no default lines, no readiness — just how to fix it.
    if !report.has_providers {
        lines.push(String::new());
        lines.push(ui::section("Providers"));
        lines.push(String::new());
        lines.push(ui::warn("No provider is configured."));
        lines.push(ui::info("Set one up: openbutler setup"));
        return lines.join("\n");
    }

    lines.push(String::new());
    lines.push(ui::section("Providers"));
    let rows: Vec<Vec<String>> = report
        .providers
        .iter()
        .map(|row| {
            vec![
                if row.is_default {
                    format!("{} *", row.id)
                } else {
                    row.id.clone()
                },
                if row.enabled { "on" } else { "off" }.to_owned(),
                row.key_count.to_string(),
                match row.tool_capable_count {
                    Some(count) => count.to_string(),
                    None => "unknown".to_owned(),
                },
                row.default_model.clone(),
                if row.custom { "custom" } else { "catalog" }.to_owned(),
            ]
        })
        .collect();
    lines.extend(ui::table(
        &["PROVIDER", "STATE", "KEYS", "TOOLS", "DEFAULT", "KIND"],
        &rows,
    ));
    lines.push(String::new());

    let default_line = match &report.default {
        Some(choice) => format!(
            "{} / {}{}",
            choice.provider_id,
            choice.model_id,
            if choice.source == "usage" {
                " (from usage)"
            } else {
                ""
            }
        ),
        None => "(none)".to_owned(),
    };
    lines.push(ui::kv("Default", &default_line));
    lines.push(ui::kv("Reasoning", &report.reasoning));
    lines.push(ui::kv(
        "Show reasoning",
        if report.show_reasoning { "on" } else { "off" },
    ));
    lines.push(String::new());

    if report.readiness.ready {
        lines.push(ui::success("Ready to run."));
    } else {
        for problem in &report.readiness.problems {
            lines.push(ui::failure(problem));
        }
    }
    for hint in &report.readiness.hints {
        lines.push(ui::info(hint));
    }
    if report.catalog_loading && !report.catalog_loaded {
        lines.push(ui::info(
            "The catalog is downloading in the background; run this again when it finishes.",
        ));
    }
    if report.default.is_none() {
        lines.push(ui::info("Change the default model: openbutler setup"));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use serde_json::json;

    fn decoded(value: serde_json::Value) -> ConfigReport {
        serde_json::from_value(value).expect("the fixture decodes")
    }

    /// The full screen: MCP table, kv lines, provider table, default pair,
    /// reasoning, readiness — every line, byte for byte.
    #[test]
    fn the_screen_lays_out_every_line_like_the_source() {
        let report = decoded(json!({
            "mcpServers": [
                { "id": "local", "transport": "stdio", "endpoint": "npx -y server-files" }
            ],
            "defaultWorkspace": "C:\\work\\proj",
            "toolAccess": "Workspace (default)",
            "hasProviders": true,
            "catalogLoaded": true,
            "catalogLoading": false,
            "providers": [
                {
                    "id": "mock", "enabled": true, "keyCount": 2,
                    "toolCapableCount": 3, "isDefault": true, "custom": true,
                    "url": "http://127.0.0.1:9/v1", "defaultModel": "test-model"
                },
                {
                    "id": "other", "enabled": false, "keyCount": 0,
                    "toolCapableCount": null, "isDefault": false, "custom": false,
                    "url": "", "defaultModel": ""
                }
            ],
            "default": { "providerId": "mock", "modelId": "test-model", "source": "settings" },
            "reasoning": "default",
            "showReasoning": true,
            "readiness": { "ready": true, "problems": [], "hints": [] }
        }));
        let text = render(&report);
        // Content is pinned byte for byte; the trailing padding the source's
        // `padEnd` emits is pinned separately by line length (see `ui`).
        let lines: Vec<&str> = text.lines().map(str::trim_end).collect();
        let mcp_bar = ui::section("MCP servers");
        let providers_bar = ui::section("Providers");
        assert_eq!(
            lines,
            vec![
                "",
                mcp_bar.as_str(),
                "  SERVER  TRANSPORT  ENDPOINT",
                "  \u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}  \u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}  \u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}",
                "  local   stdio      npx -y server-files",
                "\u{b7} Manage global servers with openbutler mcp. Credentials are environment-variable references, not saved values.",
                "",
                "  Default workspace      C:\\work\\proj",
                "  Tool access            Workspace (default)",
                "",
                providers_bar.as_str(),
                "  PROVIDER  STATE  KEYS  TOOLS    DEFAULT     KIND",
                "  \u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}  \u{2500}\u{2500}\u{2500}\u{2500}\u{2500}  \u{2500}\u{2500}\u{2500}\u{2500}  \u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}  \u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}  \u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}",
                "  mock *    on     2     3        test-model  custom",
                "  other     off    0     unknown              catalog",
                "",
                "  Default                mock / test-model",
                "  Reasoning              default",
                "  Show reasoning         on",
                "",
                "\u{2714} Ready to run.",
            ]
        );
        // The provider row pads its last column (`padEnd`), so its length is
        // fixed even though the trailing spaces are invisible.
        let row = text
            .lines()
            .find(|line| line.starts_with("  mock *"))
            .expect("the default row renders");
        assert_eq!(
            row.chars().count(),
            2 + 8 + 2 + 5 + 2 + 4 + 2 + 7 + 2 + 10 + 2 + 7
        );
    }

    #[test]
    fn an_empty_machine_stops_at_the_setup_hint() {
        let report = decoded(json!({
            "mcpServers": [],
            "defaultWorkspace": null,
            "toolAccess": "Full access (no permission prompts)",
            "hasProviders": false,
            "catalogLoaded": false,
            "catalogLoading": false,
            "providers": [],
            "default": null,
            "reasoning": "default",
            "showReasoning": true,
            "readiness": { "ready": false, "problems": [], "hints": [] }
        }));
        let text = render(&report);
        assert!(text.contains("\u{b7} No MCP servers configured."), "{text}");
        assert!(
            text.contains("  Default workspace      (choose when opening OpenButler)"),
            "{text}"
        );
        assert!(text.contains("\n! No provider is configured.\n"), "{text}");
        assert!(
            text.contains("\u{b7} Set one up: openbutler setup"),
            "{text}"
        );
        assert!(
            !text.contains("Reasoning") && !text.contains("Ready to run"),
            "an empty machine returns before the default block: {text}"
        );
    }

    #[test]
    fn a_usage_default_and_the_download_line_render() {
        let report = decoded(json!({
            "mcpServers": [],
            "defaultWorkspace": null,
            "toolAccess": "Approve for me (Auto-review outside workspace)",
            "hasProviders": true,
            "catalogLoaded": false,
            "catalogLoading": true,
            "providers": [
                {
                    "id": "mock", "enabled": true, "keyCount": 1,
                    "toolCapableCount": null, "isDefault": false, "custom": true,
                    "url": "http://h", "defaultModel": ""
                }
            ],
            "default": { "providerId": "mock", "modelId": "favoured", "source": "usage" },
            "reasoning": "high (this model: default)",
            "showReasoning": false,
            "readiness": {
                "ready": false,
                "problems": ["No LLM provider is enabled with an API key."],
                "hints": []
            }
        }));
        let text = render(&report);
        assert!(
            text.contains("  Default                mock / favoured (from usage)"),
            "{text}"
        );
        assert!(text.contains("  Show reasoning         off"), "{text}");
        assert!(
            text.contains("\u{2716} No LLM provider is enabled with an API key."),
            "{text}"
        );
        assert!(
            text.contains(
                "The catalog is downloading in the background; run this again when it finishes."
            ),
            "{text}"
        );
        assert!(
            !text.contains("Change the default model"),
            "a usage default is a default: {text}"
        );
    }
}
