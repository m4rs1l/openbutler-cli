//! `openbutler skills` — the global Agent Skills the machine offers, with
//! their roots, composed by the host (`skills.list`).
//!
//! The host answers the table (or the empty note plus every root and the
//! hint) as finished lines; this module prints them through its glyph
//! palette and decides nothing.

use std::path::Path;

use serde::Deserialize;

use crate::CliError;

/// The host's answer: the finished screen lines.
#[derive(Deserialize)]
struct Answer {
    lines: Vec<crate::ui::ScreenLine>,
}

/// Asks the host and prints its screen.
pub async fn run(data_dir: &Path) -> Result<String, CliError> {
    let mut client = crate::client::Client::connect(data_dir).await?;
    let result = super::daemon::call_fields(&mut client, "skills.list", Vec::new()).await?;
    let answer: Answer = serde_json::from_value(result).map_err(|source| CliError::Decode {
        action: "skills",
        source,
    })?;
    Ok(crate::ui::screen(&answer.lines))
}
