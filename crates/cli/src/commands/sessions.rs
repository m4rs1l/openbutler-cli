//! `openbutler sessions` — the daemon's session list, one line per row.

use std::path::Path;

use openbutler_protocol::SessionSummary;

use crate::CliError;

/// Fetches `session.list` and renders every row.
pub async fn run(data_dir: &Path) -> Result<String, CliError> {
    let result = super::ask(data_dir, "session.list").await?;
    let sessions: Vec<SessionSummary> =
        serde_json::from_value(result).map_err(|source| CliError::Decode {
            action: "session.list",
            source,
        })?;
    Ok(render(&sessions))
}

/// Renders the list the daemon computed: full id first (scripts select by
/// it), then activity, model, title, and the running marker when the host
/// reports one.
fn render(sessions: &[SessionSummary]) -> String {
    if sessions.is_empty() {
        return "No sessions.".to_owned();
    }
    sessions
        .iter()
        .map(|session| {
            let running = match session.running {
                Some(true) => " [running]",
                _ => "",
            };
            format!(
                "{}  {}  {}/{}{running}  {}",
                session.id,
                session.updated_at,
                session.provider_id,
                session.model_id,
                session.title
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}
