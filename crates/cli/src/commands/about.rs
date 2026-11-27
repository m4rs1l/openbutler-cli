//! `openbutler about` — version, shell, runtime and the configuration
//! paths, answered and composed by the host (`about.report`).
//!
//! The screen is the source's `runAbout` layout with one added row: this
//! build has two binaries, so `Host` names the daemon that answered
//! (version + pid). The two binaries version independently: the screen
//! describes the host, while `openbutler --version` describes the client.
//! Compatibility is decided by the protocol version on the wire, never by
//! comparing the two product versions.

use std::path::Path;

use serde::Deserialize;

use crate::CliError;

/// The host's answer: the finished screen lines.
#[derive(Deserialize)]
struct About {
    lines: Vec<crate::ui::ScreenLine>,
}

/// Asks the host and prints its lines.
pub async fn run(data_dir: &Path) -> Result<String, CliError> {
    let result = super::ask(data_dir, "about.report").await?;
    let about: About = serde_json::from_value(result).map_err(|source| CliError::Decode {
        action: "about",
        source,
    })?;
    Ok(crate::ui::screen(&about.lines))
}
