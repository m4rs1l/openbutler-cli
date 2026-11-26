//! `openbutler web`: the browser surface's address, composed by the host.
//!
//! Source: `cli/web.ts` (`runWebCommand`). The pair URL and the bind belong
//! to the host (`web.report`); this module prints the answered lines.

use std::path::Path;

use crate::CliError;
use crate::client::Client;
use crate::ui;

/// One `web.report` call, printed through the glyph palette.
pub async fn run(data_dir: &Path) -> Result<String, CliError> {
    let mut client = Client::connect(data_dir).await?;
    let result =
        crate::commands::daemon::call_fields(&mut client, "web.report", Vec::new()).await?;
    let lines: Vec<ui::ScreenLine> =
        serde_json::from_value(result["lines"].clone()).unwrap_or_default();
    Ok(lines
        .iter()
        .map(|line| ui::toned(line.tone, &line.text))
        .collect::<Vec<_>>()
        .join("\n"))
}
