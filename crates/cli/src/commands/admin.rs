//! `admin`: the account that signs devices in, created on this machine.
//!
//! The password is collected here, because the confirmation never leaves the
//! terminal - it is input collection, not a host rule. A terminal prompts twice
//! and compares; `--password-stdin` reads one line so an installer or a script
//! can provision the first account without a tty. The account itself is created
//! by the local host (`admin.create`), so the client never touches `admin.json`.

use std::io::BufRead;
use std::path::Path;

use openbutler_protocol::{DaemonRequest, DaemonResponse, ScreenLine};

use crate::client::Client;
use crate::commands::connect::SecretPrompt;
use crate::{CliError, ui};

/// Refusal when there is no terminal to prompt on and no password was piped.
pub const TTY_REFUSAL: &str = "admin create needs an interactive terminal: the password is prompted twice, never passed as an argument. Pipe it with --password-stdin instead.";

/// The two prompts did not agree. Composed here because only the terminal saw
/// the confirmation.
pub const MISMATCH: &str = "The two passwords are not the same.";

/// Nothing arrived on standard input.
pub const NO_STDIN_PASSWORD: &str = "No password arrived on standard input.";

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
    submit(data_dir, username, &password).await
}

/// Creates the admin account from the first line of `source`, so a caller
/// without a terminal can provision it. Only the line ending is trimmed: a
/// password may contain spaces.
pub async fn create_from_stdin(
    data_dir: &Path,
    username: &str,
    source: &mut dyn BufRead,
) -> Result<String, CliError> {
    let mut line = String::new();
    source
        .read_line(&mut line)
        .map_err(|error| CliError::Answer(format!("Reading the password failed: {error}")))?;
    let password = line.trim_end_matches(['\r', '\n']);
    if password.is_empty() {
        return Err(CliError::Answer(NO_STDIN_PASSWORD.to_owned()));
    }
    submit(data_dir, username, password).await
}

/// Sends the account to the local host, which owns the file and the rules.
async fn submit(data_dir: &Path, username: &str, password: &str) -> Result<String, CliError> {
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
