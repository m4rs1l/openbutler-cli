//! `openbutler skills` — the Agent Skills this machine offers.
//!
//! The daemon scans the source's roots (`skills.list`) and answers the
//! metadata rows plus the roots themselves; this command only lays them
//! out — the table when something was found, the source's empty screen
//! (roots named, the how-to sentence) when nothing was.

use std::path::Path;

use serde::Deserialize;
use serde_json::Value;

use crate::CliError;
use crate::ui;

/// One discovered skill, as the host reported it.
#[derive(Deserialize)]
struct Skill {
    name: String,
    scope: String,
    description: String,
    #[serde(rename = "filePath")]
    file_path: String,
}

/// One root the host scanned.
#[derive(Deserialize)]
struct Root {
    scope: String,
    path: String,
}

/// The host's answer.
#[derive(Deserialize)]
struct Answer {
    skills: Vec<Skill>,
    roots: Vec<Root>,
}

/// Asks the host and renders the table or the empty screen.
pub async fn run(data_dir: &Path, workspace: Option<String>) -> Result<String, CliError> {
    let mut fields: Vec<(&str, Value)> = Vec::new();
    if let Some(workspace) = &workspace {
        fields.push(("workspace", Value::from(workspace.clone())));
    }
    let mut client = crate::client::Client::connect(data_dir).await?;
    let result = super::daemon::call_fields(&mut client, "skills.list", fields).await?;
    let answer: Answer = serde_json::from_value(result).map_err(|source| CliError::Decode {
        action: "skills",
        source,
    })?;
    if answer.skills.is_empty() {
        let mut lines = vec![ui::info("No Agent Skills found.")];
        for root in &answer.roots {
            lines.push(ui::kv(&root.scope, &root.path));
        }
        lines.push(ui::info(
            "Add a skill-name/SKILL.md with Agent Skills frontmatter.",
        ));
        return Ok(lines.join("\n"));
    }
    let rows: Vec<Vec<String>> = answer
        .skills
        .iter()
        .map(|skill| {
            vec![
                skill.name.clone(),
                skill.scope.clone(),
                skill.description.clone(),
                skill.file_path.clone(),
            ]
        })
        .collect();
    Ok(ui::table(&["NAME", "SCOPE", "DESCRIPTION", "SKILL FILE"], &rows).join("\n"))
}
