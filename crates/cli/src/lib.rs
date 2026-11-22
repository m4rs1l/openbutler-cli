//! OpenButler command-line client.
//!
//! The CLI is a thin consumer: it derives the daemon endpoint, sends
//! envelopes from `openbutler-protocol`, and renders the state the daemon
//! computed. No rule, filter, or count is produced here.

pub mod client;
pub mod commands;
pub mod data_dir;

use commands::run;
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
    /// One-shot request: run it, print the answer, exit.
    Run {
        /// The request text for the agent.
        #[arg(value_name = "REQUEST")]
        request: String,
    },
    /// Background-host lifecycle: status and stop today, more to come.
    Daemon {
        #[command(subcommand)]
        command: Option<DaemonCommand>,
    },
}

/// `openbutler daemon` subcommands; bare `daemon` means `status`, the
/// source's default.
#[derive(Debug, Clone, clap::Subcommand)]
pub enum DaemonCommand {
    /// Show the running host and its report.
    Status,
    /// Stop the background host gracefully.
    Stop,
}

/// Runs one command and returns its rendered stdout text.
///
/// The data directory is a parameter (not a hidden env read) so tests can
/// point the same code at a fixture endpoint; `main` passes the real one.
pub async fn execute(command: Command, data_dir: &std::path::Path) -> Result<String, CliError> {
    match command {
        Command::Status => commands::status::run(data_dir).await,
        Command::Sessions => commands::sessions::run(data_dir).await,
        Command::Daemon { command } => match command.unwrap_or(DaemonCommand::Status) {
            DaemonCommand::Status => commands::daemon::status(data_dir).await,
            DaemonCommand::Stop => commands::daemon::stop(data_dir).await,
        },
        // The buffered path (tests and callers without a terminal); the
        // binary streams a run straight to the real stdio.
        Command::Run { request } => {
            let mut out = Vec::new();
            let mut err = Vec::new();
            run::run(data_dir, run::RunOptions::new(request), &mut out, &mut err).await?;
            let mut text = String::from_utf8_lossy(&out).into_owned();
            if !err.is_empty() {
                text.push_str(&String::from_utf8_lossy(&err));
            }
            Ok(text)
        }
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
    /// A run stopped before it produced anything (the two source
    /// deadline sentences, verbatim).
    #[error("{0}")]
    Run(String),
    /// The host stayed on the endpoint past the stop window; this build
    /// keeps no pid record, so its identity can never be verified and
    /// it is never terminated blindly (source refusal, verbatim).
    #[error("{0}")]
    Stop(String),
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
    let outcome: Result<(), CliError> = runtime.block_on(async {
        match command {
            // A run streams live: stdout and stderr go straight through.
            Command::Run { request } => {
                let mut out = std::io::stdout();
                let mut err = std::io::stderr();
                run::run(
                    &data_dir::data_dir(),
                    run::RunOptions::new(request),
                    &mut out,
                    &mut err,
                )
                .await
            }
            other => {
                let text = execute(other, &data_dir::data_dir()).await?;
                println!("{text}");
                Ok(())
            }
        }
    });
    match outcome {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("openbutler: {error}");
            std::process::ExitCode::from(1)
        }
    }
}
