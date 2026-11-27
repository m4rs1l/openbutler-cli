//! `admin`: the account that signs devices in, created on this machine.
//!
//! The password is prompted twice and compared here, because the confirmation
//! never leaves the terminal — it is input collection, not a host rule. The
//! account itself is created by the local host (`admin.create`), so the client
//! never touches `admin.json`.

use std::path::Path;

use openbutler_protocol::{DaemonRequest, DaemonResponse, ScreenLine};

use crate::client::Client;
use crate::commands::connect::SecretPrompt;
use crate::{CliError, ui};

/// Refusal when there is no terminal to prompt on.
pub const TTY_REFUSAL: &str = "admin create needs an interactive terminal: the password is prompted twice, never passed as an argument.";

/// The two prompts did not agree. Composed here because only the terminal saw
/// the confirmation.
pub const MISMATCH: &str = "The two passwords are not the same.";

/// Creates the admin account and prints the host's own sentence.
pub async fn create(
    data_dir: &Path,
    username: &str,
    prompt: &mut dyn SecretPrompt,
) -> Result<String, CliError> {
    if !prompt.is_interactive() {
        return Err(CliError::Answer(TTY_REFUSAL.to_owned()));
    }
    let password = prompt.ask("Password for the admin account: ")?;
    let repeated = prompt.ask("Repeat the password: ")?;
    if password != repeated {
        return Err(CliError::Answer(MISMATCH.to_owned()));
    }

    // The account is host state, so the local host writes it: a stored remote
    // backend must not make `admin create` configure someone else's machine.
    let mut client = Client::connect_local(data_dir).await?;
    let request = DaemonRequest::new("admin-create", "admin.create")
        .with("username", username)
        .with("password", password);
    let result = match client.call(&request).await? {
        DaemonResponse::Success { result, .. } => result,
        DaemonResponse::Failure { error, .. } => return Err(CliError::Answer(error)),
    };
    let lines: Vec<ScreenLine> = result
        .get("lines")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|source| CliError::Decode {
            action: "admin.create",
            source,
        })?
        .unwrap_or_default();
    Ok(ui::screen(&lines))
}
