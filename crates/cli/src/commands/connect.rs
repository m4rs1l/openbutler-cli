//! `connect`: sign this client in to a host, and forget it again.
//!
//! The password is never an argument — argv is visible in the process list and
//! lands in shell history. It is prompted on the terminal, masked, exactly like
//! `keys add` prompts for API keys. Production passes [`TerminalPrompt`]; tests
//! pass a scripted one.
//!
//! What the client stores is its own: the target (no secret) and the session
//! (private). The two sentences this command composes are about those files,
//! never about the host: everything else printed here is the host's own screen.

use std::io::IsTerminal;
use std::path::Path;

use openbutler_protocol::auth::LoginSuccess;
use openbutler_protocol::{BackendTarget, DaemonRequest, DaemonResponse, ScreenLine};

use crate::client::{Client, parse_address};
use crate::remote::{self, StoredSession};
use crate::{CliError, ui};

/// Refusal when there is no terminal to prompt on.
pub const TTY_REFUSAL: &str =
    "connect needs an interactive terminal: the password is prompted, never passed as an argument.";

/// Where the password comes from, and whether a terminal is there at all.
/// Production passes [`TerminalPrompt`]; tests pass a scripted one, so the
/// guard and the prompts stay exercised without a real tty.
pub trait SecretPrompt {
    /// Whether a human terminal is attached (the guard runs before any dial).
    fn is_interactive(&self) -> bool;
    /// Asks for one secret, masking what is typed.
    fn ask(&mut self, label: &str) -> Result<String, CliError>;
}

/// The real terminal: masked, no echo.
pub struct TerminalPrompt;

impl SecretPrompt for TerminalPrompt {
    fn is_interactive(&self) -> bool {
        std::io::stdin().is_terminal()
    }

    fn ask(&mut self, label: &str) -> Result<String, CliError> {
        rpassword::prompt_password(label)
            .map_err(|error| CliError::Answer(format!("cannot read the password: {error}")))
    }
}

/// Runs `connect`: sign in, store the backend, and print the host's own status
/// screen — plus one line about the file this client just wrote.
pub async fn run(
    data_dir: &Path,
    target: Option<String>,
    username: Option<String>,
    forget: bool,
    prompt: &mut dyn SecretPrompt,
) -> Result<String, CliError> {
    if forget {
        let removed = remote::clear(data_dir).map_err(CliError::Backend)?;
        let text = if removed {
            "This client no longer has a stored backend; commands use the local daemon."
        } else {
            "This client had no stored backend."
        };
        return Ok(ui::info(text));
    }

    let Some(target) = target else {
        return Err(CliError::Answer(
            "connect needs a host and port, e.g. openbutler connect 100.64.0.5:45280 <username>"
                .to_owned(),
        ));
    };
    let Some(username) = username else {
        return Err(CliError::Answer(
            "connect needs the account username: openbutler connect <host:port> <username>"
                .to_owned(),
        ));
    };
    if !prompt.is_interactive() {
        return Err(CliError::Answer(TTY_REFUSAL.to_owned()));
    }

    let (host, port) = parse_address(&target)?;
    let password = prompt.ask("Password: ")?;
    let device = device_label();
    let address = format!("{host}:{port}");

    let mut client = Client::dial_remote(&address, data_dir).await?;
    let issued: LoginSuccess = client.login(&username, &password, &device).await?;

    let target = BackendTarget::new(host, port, username.clone(), device, stamp());
    let session = StoredSession {
        session_id: issued.device_id,
        token: issued.token,
        expires_at: issued.expires_at,
    };
    remote::save(data_dir, &target, &session).map_err(CliError::Backend)?;

    // The host's own screen proves the connection with nothing composed here.
    let request = DaemonRequest::new("status-after-connect", "status");
    let lines = match client.call(&request).await? {
        DaemonResponse::Success { result, .. } => result
            .get("lines")
            .cloned()
            .map(serde_json::from_value::<Vec<ScreenLine>>)
            .transpose()
            .map_err(|source| CliError::Decode {
                action: "status",
                source,
            })?
            .unwrap_or_default(),
        DaemonResponse::Failure { error, .. } => return Err(CliError::Answer(error)),
    };

    let mut text = ui::screen(&lines);
    if !text.is_empty() {
        text.push('\n');
    }
    text.push_str(&ui::info(&format!(
        "This client now uses {address} as {username} (undo: openbutler connect --forget)."
    )));
    Ok(text)
}

/// The client's own name for itself, sent as the `device` label. It is a
/// display name for `device list`, never a credential.
fn device_label() -> String {
    for name in ["COMPUTERNAME", "HOSTNAME"] {
        if let Ok(value) = std::env::var(name) {
            let trimmed = value.trim();
            if !trimmed.is_empty() && trimmed.chars().count() <= 64 {
                return trimmed.to_owned();
            }
        }
    }
    "unknown".to_owned()
}

/// The client's own stamp for `savedAt` (informational, never trusted by the
/// host).
fn stamp() -> String {
    chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string()
}
