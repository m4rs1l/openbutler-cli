//! `openbutler config` — the manager screen: what is set up, which provider
//! is the default, and what still stands between this machine and a run.
//!
//! Source: `cli-main.ts` `runConfig` (L169-213). Every decision — labels,
//! counts, the default pair, readiness sentences, the table layout, the
//! line order — is made by the host (`config.report`), which answers the
//! finished screen lines. This module prints them and applies its own glyph
//! palette. Divergences are recorded here:
//! - the source awaited the catalog download inside the command and printed
//!   real tool counts on the first run; our host starts the download and
//!   answers at once, so a first run shows `unknown` and the extra line
//!   says why;
//! - the no-default hint names `openbutler run` (owner ruling: the verb is
//!   `run`, not `--ask`);
//! - provider rows follow the stored `providerOrder`; when no order was ever
//!   recorded they list id-sorted, because our settings maps are sorted and
//!   the source's YAML insertion order is not recoverable from them.

use std::path::Path;

use serde::Deserialize;

use crate::CliError;

/// The host's answer: the finished screen lines.
#[derive(Deserialize)]
struct ConfigReport {
    lines: Vec<crate::ui::ScreenLine>,
}

/// Fetches and prints the host's screen.
pub async fn run(data_dir: &Path) -> Result<String, CliError> {
    let result = super::ask(data_dir, "config.report").await?;
    let report: ConfigReport =
        serde_json::from_value(result).map_err(|source| CliError::Decode {
            action: "config.report",
            source,
        })?;
    Ok(crate::ui::screen(&report.lines))
}
