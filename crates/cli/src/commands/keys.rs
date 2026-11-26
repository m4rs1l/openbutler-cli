//! `openbutler keys [add <provider>|list|remove <provider>]` — the provider
//! key store.
//!
//! Source: `cli/keys.ts` (read 2026-11-26). The store is the daemon's:
//! `list` answers display rows (…tails, never a key), `add`/`remove` are
//! local-only writes. The prompts are masked and answer-driven, so they
//! run where the terminal is — this module only collects what the user
//! typed and names the result.

use std::io::IsTerminal;
use std::path::Path;

use serde::Deserialize;
use serde_json::json;

use crate::CliError;
use crate::commands::setup::{self, Term};
use crate::ui;

/// The non-interactive refusal (the source's sentence).
pub fn guard(action: &str) -> String {
    format!(
        "openbutler keys {action} needs an interactive terminal. Keys are entered through a masked prompt, never as arguments."
    )
}

/// One listing row (`describeProviderKeys`).
#[derive(Deserialize)]
struct Row {
    id: String,
    #[serde(rename = "keyCount")]
    key_count: u64,
}

/// The `keys.list` answer.
#[derive(Deserialize)]
struct Listing {
    providers: Vec<Row>,
    lines: Vec<crate::ui::ScreenLine>,
}

/// The command's shape, shared with the clap tree.
pub enum KeysRequest {
    /// List providers and their stored key counts.
    List,
    /// Add keys through masked prompts.
    Add { provider: String },
    /// Remove every key of a provider (confirmed first).
    Remove { provider: String },
}

/// Runs the command on the real terminal; the write actions demand a TTY.
pub async fn run(data_dir: &Path, request: KeysRequest) -> Result<String, CliError> {
    if let KeysRequest::Add { .. } | KeysRequest::Remove { .. } = &request {
        let action = if matches!(request, KeysRequest::Add { .. }) {
            "add"
        } else {
            "remove"
        };
        if !std::io::stdin().is_terminal() {
            return Err(CliError::Answer(guard(action)));
        }
    }
    run_with(&mut setup::StdTerm, data_dir, request).await
}

/// The testable half: a scripted terminal drives the prompts.
pub async fn run_with(
    term: &mut dyn Term,
    data_dir: &Path,
    request: KeysRequest,
) -> Result<String, CliError> {
    match request {
        KeysRequest::List => list(data_dir).await,
        KeysRequest::Add { provider } => {
            let Some(keys) = setup::ask_keys(term, &provider) else {
                return Ok(ui::info("Nothing added."));
            };
            let mut client = crate::client::Client::connect(data_dir).await?;
            let result = super::daemon::call_fields(
                &mut client,
                "keys.add",
                vec![("provider", json!(provider)), ("keys", json!(keys))],
            )
            .await?;
            let lines: Vec<crate::ui::ScreenLine> = serde_json::from_value(result["lines"].clone())
                .map_err(|source| CliError::Decode {
                    action: "keys add",
                    source,
                })?;
            Ok(ui::screen(&lines))
        }
        KeysRequest::Remove { provider } => {
            // One connection for both the count and the write: the view is
            // consistent and the fake daemon serves one client.
            let mut client = crate::client::Client::connect(data_dir).await?;
            let listing_result =
                super::daemon::call_fields(&mut client, "keys.list", Vec::new()).await?;
            let listing: Listing =
                serde_json::from_value(listing_result).map_err(|source| CliError::Decode {
                    action: "keys list",
                    source,
                })?;
            // The count names what a confirmation would drop; a provider
            // the listing does not carry still goes to the daemon, whose
            // classification owns the unknown and MCP sentences.
            let count = listing
                .providers
                .iter()
                .find(|row| row.id == provider)
                .map(|row| row.key_count);
            if let Some(count) = count {
                let plural = if count == 1 {
                    "1 key".to_owned()
                } else {
                    format!("{count} keys")
                };
                let confirmed = setup::confirm(term, &format!("Remove {plural} for {provider}?"))
                    .unwrap_or(false);
                if !confirmed {
                    return Ok(ui::info("Cancelled. Nothing was written."));
                }
            }
            let result = super::daemon::call_fields(
                &mut client,
                "keys.remove",
                vec![("provider", json!(provider))],
            )
            .await?;
            let lines: Vec<crate::ui::ScreenLine> = serde_json::from_value(result["lines"].clone())
                .map_err(|source| CliError::Decode {
                    action: "keys remove",
                    source,
                })?;
            Ok(ui::screen(&lines))
        }
    }
}

/// The table, or the source's two empty-screen lines.
async fn list(data_dir: &Path) -> Result<String, CliError> {
    let listing = listing(data_dir).await?;
    Ok(ui::screen(&listing.lines))
}

/// One `keys.list` round trip.
async fn listing(data_dir: &Path) -> Result<Listing, CliError> {
    let result = super::ask(data_dir, "keys.list").await?;
    serde_json::from_value::<Listing>(result).map_err(|source| CliError::Decode {
        action: "keys list",
        source,
    })
}
