//! `openbutler logs` — the machine log's tail, read by the host.
//!
//! Source: `cli-main.ts` `runLogs`. The source read the file in the CLI
//! process; this build's daemon writes no structured file yet, so the honest
//! machine log is the `daemon.log` that `daemon start` captures. The read now
//! happens where the file lives (`logs.tail`), and this module prints the
//! lines the host returns.
//!
//! `--file` is resolved against the caller's shell directory here and must
//! reach the host absolute: the host's working directory is not the one the
//! owner typed the command in.

use std::path::Path;

use serde::Deserialize;
use serde_json::json;

use crate::CliError;

/// The host's answer: finished lines, each with the tone it chose.
#[derive(Deserialize)]
struct TailAnswer {
    lines: Vec<crate::ui::ScreenLine>,
}

/// Asks the host for the tail and prints exactly what it answers.
pub async fn run(
    data_dir: &Path,
    lines: usize,
    file: Option<std::path::PathBuf>,
) -> Result<String, CliError> {
    let mut fields: Vec<(&str, serde_json::Value)> = vec![("lines", json!(lines))];
    if let Some(file) = file {
        // The caller's directory is the only correct base for a relative path;
        // a path that cannot be made absolute travels as typed and the host
        // refuses it with its own sentence.
        let absolute = std::path::absolute(&file).unwrap_or(file);
        fields.push(("file", json!(absolute.to_string_lossy())));
    }

    let mut client = crate::client::Client::connect(data_dir).await?;
    let result = crate::commands::daemon::call_fields(&mut client, "logs.tail", fields).await?;
    let answer: TailAnswer = serde_json::from_value(result).map_err(|source| CliError::Decode {
        action: "logs.tail",
        source,
    })?;
    Ok(crate::ui::screen(&answer.lines))
}
