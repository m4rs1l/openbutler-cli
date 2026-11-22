//! OpenButler command-line client.
//!
//! The CLI is a thin consumer: it derives the daemon endpoint, sends
//! envelopes from `openbutler-protocol`, and renders the state the daemon
//! computed. No rule, filter, or count is produced here.

pub mod client;
pub mod commands;
pub mod data_dir;

use std::io::Write;

use clap::Parser;

/// The parsed subcommand tree (`openbutler --help`).
#[derive(Debug, Parser)]
#[command(
    name = "openbutler",
    version,
    about = "Command-line client for the OpenButler daemon"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

/// The S1 command set; the tree grows with each slice.
#[derive(Debug, Clone, clap::Subcommand)]
pub enum Command {
    /// Daemon and workspace status as the host reports it.
    Status,
    /// List saved sessions across workspaces.
    Sessions,
}

/// Runs one command and returns its rendered stdout text.
///
/// The data directory is a parameter (not a hidden env read) so tests can
/// point the same code at a fixture endpoint; `main` passes the real one.
pub async fn execute(command: Command, data_dir: &std::path::Path) -> Result<String, CliError> {
    match command {
        Command::Status => commands::status::run(data_dir).await,
        Command::Sessions => commands::sessions::run(data_dir).await,
    }
}

/// A command failure: the message printed to stderr and the exit class.
#[derive(Debug, thiserror::Error)]
pub enum CliError {
    /// The daemon did not answer (not running, wrong data dir, or a wire
    /// fault on the way).
    #[error("{0}")]
    Client(#[from] client::ClientError),
    /// The daemon refused or the payload did not fit its contract.
    #[error("{0}")]
    Answer(String),
    /// The payload did not decode into the contract type.
    #[error("{action} payload does not match the wire contract: {source}")]
    Decode {
        action: &'static str,
        #[source]
        source: serde_json::Error,
    },
}

/// Parses real arguments, runs one command, prints, and picks the exit code:
/// 0 success, 1 command/daemon failure, 2 usage (clap's own exit).
pub fn main_entry() -> std::process::ExitCode {
    let cli = Cli::parse();
    let Some(command) = cli.command else {
        // Unknown or missing verbs never become a prompt: usage only.
        let mut help = Vec::new();
        let mut command = <Cli as clap::CommandFactory>::command();
        let _ = command.write_long_help(&mut help);
        let _ = std::io::stdout().write_all(&help);
        let _ = std::io::stdout().write_all(b"\n");
        return std::process::ExitCode::from(2);
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("openbutler: cannot start the async runtime: {error}");
            return std::process::ExitCode::from(1);
        }
    };
    match runtime.block_on(execute(command, &data_dir::data_dir())) {
        Ok(text) => {
            println!("{text}");
            std::process::ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("openbutler: {error}");
            std::process::ExitCode::from(1)
        }
    }
}
