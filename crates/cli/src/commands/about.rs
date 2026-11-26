//! `openbutler about` — version, shell, runtime and the configuration
//! paths, answered by the host (`about.report`).
//!
//! The screen is the source's `runAbout` layout with one added row: this
//! build has two binaries, so `Version` is the CLI's own version and
//! `Host` names the daemon that answered (version + pid). The source ran
//! one Node process and needed neither distinction.

use std::path::Path;

use serde::Deserialize;

use crate::CliError;
use crate::ui;

/// The host's answer. Only display values — no key material.
#[derive(Deserialize)]
struct About {
    version: String,
    pid: u32,
    shell: String,
    runtime: String,
    #[serde(rename = "settingsPath")]
    settings_path: String,
    #[serde(rename = "secretsPath")]
    secrets_path: String,
    packages: String,
}

/// Asks the host and lays the answer out line for line.
pub async fn run(data_dir: &Path) -> Result<String, CliError> {
    let result = super::ask(data_dir, "about.report").await?;
    let about: About = serde_json::from_value(result).map_err(|source| CliError::Decode {
        action: "about",
        source,
    })?;
    let mut lines = Vec::new();
    lines.push(String::new());
    lines.push(ui::section("OpenButler"));
    lines.push(ui::kv("Version", env!("CARGO_PKG_VERSION")));
    lines.push(ui::kv(
        "Host",
        &format!("{} (pid {})", about.version, about.pid),
    ));
    lines.push(ui::kv("Shell", &about.shell));
    lines.push(ui::kv("Runtime", &about.runtime));
    lines.push(ui::kv("Settings", &about.settings_path));
    lines.push(ui::kv("Secrets", &about.secrets_path));
    lines.push(ui::kv("Runtime packages", &about.packages));
    lines.push(String::new());
    Ok(lines.join("\n"))
}
