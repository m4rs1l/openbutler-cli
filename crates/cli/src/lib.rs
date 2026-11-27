//! OpenButler command-line client.
//!
//! The CLI is a thin consumer: it derives the daemon endpoint, sends
//! envelopes from `openbutler-protocol`, and renders the state the daemon
//! computed. No rule, filter, or count is produced here.

pub mod client;
pub mod commands;
pub mod data_dir;
pub mod remote;
pub mod tui;
pub mod ui;

use commands::run;
use std::io::Write;

use clap::Parser;

/// The parsed subcommand tree (`openbutler --help`).
#[derive(Debug, Parser)]
#[command(
    name = "openbutler",
    version = concat!(env!("CARGO_PKG_VERSION"), " (cli)"),
    about = "Command-line client for the OpenButler daemon"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
    /// List models that can call tools. `--filter`, `--limit`, `--provider`.
    #[arg(long = "list-models")]
    pub list_models: bool,
    /// Substring match on `provider/model` (with `--list-models`).
    #[arg(long, value_name = "TERM", requires = "list_models")]
    pub filter: Option<String>,
    /// Rows to show; 200 by default (with `--list-models`).
    #[arg(long, value_name = "N", requires = "list_models")]
    pub limit: Option<u32>,
    /// One provider id (with `--list-models`).
    #[arg(long, value_name = "ID", requires = "list_models")]
    pub provider: Option<String>,
}

/// `openbutler events` subcommands (cli-main.ts L1585-1610).
#[derive(Debug, Clone, clap::Subcommand)]
pub enum EventsCommand {
    /// Move a retryable event back to pending.
    Retry {
        /// The event id `events` printed.
        event_id: String,
    },
    /// Delete settled events older than a window (preview without `--yes`).
    Prune {
        /// A window like 30d, 12h, 90m, or 2w.
        #[arg(long, value_name = "DURATION")]
        older_than: String,
        /// Actually delete (preview otherwise).
        #[arg(long)]
        yes: bool,
    },
}

/// `openbutler sessions` subcommands (cli-main.ts L1465).
#[derive(Debug, Clone, clap::Subcommand)]
pub enum SessionsCommand {
    /// Delete saved sessions older than a window (preview without `--yes`).
    Prune {
        /// A window like 30d, 12h, 90m, or 2w.
        #[arg(long, value_name = "DURATION")]
        older_than: String,
        /// Actually delete (preview otherwise).
        #[arg(long)]
        yes: bool,
    },
}

/// `openbutler actions` subcommands.
#[derive(Debug, Clone, clap::Subcommand)]
pub enum ActionsCommand {
    /// Delete terminal actions older than a window (preview without `--yes`).
    Prune {
        /// A window like 30d, 12h, 90m, or 2w.
        #[arg(long, value_name = "DURATION")]
        older_than: String,
        /// Actually delete (preview otherwise).
        #[arg(long)]
        yes: bool,
    },
}

/// The S1 command set; the tree grows with each slice.
#[derive(Debug, Clone, clap::Subcommand)]
pub enum Command {
    /// Daemon and workspace status as the host reports it.
    Status,
    /// List saved sessions, or prune the ones older than a window.
    Sessions {
        #[command(subcommand)]
        command: Option<SessionsCommand>,
    },
    /// One-shot request: run it, print the answer, exit.
    Run {
        /// The request text for the agent.
        #[arg(value_name = "REQUEST")]
        request: String,
    },
    /// Background-host lifecycle: start, status, stop, restart, and foreground run.
    Daemon {
        #[command(subcommand)]
        command: Option<DaemonCommand>,
    },
    /// Stop the background host (same as `openbutler daemon stop`).
    Stop,
    /// Restart the background host (same as `openbutler daemon restart`).
    Restart,
    /// List recent auto-review denials; `approve <n>` retries one exactly once.
    Approve {
        /// The number `openbutler approve` printed.
        #[arg(value_name = "N")]
        index: Option<u32>,
    },
    /// Models a tool-using turn can run on (bare `--list-models`).
    #[command(hide = true)]
    ListModels {
        /// One provider id.
        #[arg(long, value_name = "ID")]
        provider: Option<String>,
        /// Substring match on `provider/model`.
        #[arg(long, value_name = "TERM")]
        filter: Option<String>,
        /// Rows to show; 200 by default.
        #[arg(long, value_name = "N")]
        limit: Option<u32>,
    },
    /// Read the machine log's tail.
    Logs {
        /// error | warn | info | debug (host records only).
        #[arg(long)]
        level: Option<String>,
        /// Exact event name, e.g. host.started.
        #[arg(long)]
        event: Option<String>,
        /// Print the raw JSON records instead of the human line.
        #[arg(long = "json")]
        json: bool,
        /// How many tail lines to show.
        #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u32).range(1..=100_000))]
        lines: u32,
        /// Read this file instead of the daemon's captured output.
        #[arg(long)]
        file: Option<std::path::PathBuf>,
    },
    /// Show recorded token use and cost.
    Usage {
        /// Window length in days.
        #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u32).range(1..=3650))]
        days: u32,
        /// Only rows of this session id.
        #[arg(long)]
        session: Option<String>,
        /// Only rows of this provider/model (a bare provider also works).
        #[arg(long)]
        model: Option<String>,
        /// Emit the machine-readable report instead of the render.
        #[arg(long)]
        json: bool,
    },
    /// Read the durable event ledger.
    Events {
        #[command(subcommand)]
        command: Option<EventsCommand>,
    },
    /// Read the durable tool action ledger.
    Actions {
        #[command(subcommand)]
        command: Option<ActionsCommand>,
    },
    /// First-run setup wizard (interactive).
    Setup,
    /// The manager screen: providers, default model, readiness.
    Config,
    /// Version, shell, runtime and the configuration paths.
    About,
    /// Choose the workspace bare launches use.
    Workspace {
        #[command(subcommand)]
        command: WorkspaceCommand,
    },
    /// List the Agent Skills this machine offers.
    Skills {
        /// Scan this workspace instead of the stored default.
        #[arg(long, value_name = "PATH")]
        workspace: Option<String>,
    },
    /// List, log and inspect scheduled wakeups.
    Schedule {
        /// list | log <id> | capabilities (bare: list).
        #[arg(trailing_var_arg = true)]
        words: Vec<String>,
    },
    /// The browser surface this host serves.
    Web,
    /// The global MCP server manager.
    Mcp {
        /// list | add | remove (bare: the interactive menu).
        action: Option<String>,
    },
    /// Manage provider API keys in the secret store.
    Keys {
        #[command(subcommand)]
        command: Option<KeysCommand>,
    },
    /// The approval posture: mode, timeouts, review model.
    Permissions {
        /// workspace | auto-review | full-access (bare: show the current one).
        mode: Option<String>,
        /// Auto-review verdict timeout in seconds (5-600).
        #[arg(long, value_name = "SECONDS")]
        review_timeout: Option<u64>,
        /// Tool-approval timeout in seconds (5-600).
        #[arg(long, value_name = "SECONDS")]
        approval_timeout: Option<u64>,
        /// Reviewer model `<provider>/<model>`, or `none` for the main model.
        #[arg(long, value_name = "PROVIDER/MODEL")]
        review_model: Option<String>,
    },
    /// The interactive session (needs a real terminal).
    Tui,
    /// Sign this client in to a host, or forget the stored backend.
    Connect {
        /// The host and port, e.g. 100.64.0.5:45280.
        target: Option<String>,
        /// The account username.
        username: Option<String>,
        /// Drop the stored backend instead of signing in.
        #[arg(long)]
        forget: bool,
    },
    /// The admin account on this machine.
    Admin {
        #[command(subcommand)]
        command: AdminCommand,
    },
}

/// `openbutler daemon` subcommands; bare `daemon` means `status`, the
/// source's default.
/// `openbutler keys` subcommands; bare `keys` means `list`.
#[derive(Debug, Clone, clap::Subcommand)]
pub enum KeysCommand {
    /// Add keys for a provider (masked prompts; needs a terminal).
    Add {
        /// The provider id.
        provider: String,
    },
    /// List providers and their stored key counts.
    List,
    /// Remove every key of a provider (confirmed; needs a terminal).
    Remove {
        /// The provider id.
        provider: String,
    },
}

/// `openbutler workspace` subcommands.
#[derive(Debug, Clone, clap::Subcommand)]
pub enum WorkspaceCommand {
    /// Choose the workspace bare launches use (omit the path to show it).
    Default {
        /// The directory to use.
        path: Option<String>,
    },
}

#[derive(Debug, Clone, clap::Subcommand)]
pub enum AdminCommand {
    /// Create the admin account (masked prompts; needs a terminal).
    Create {
        /// The account username.
        username: String,
    },
}

#[derive(Debug, Clone, clap::Subcommand)]
pub enum DaemonCommand {
    /// Show the running host and its report.
    Status,
    /// Stop the background host gracefully.
    Stop,
    /// Recycle the host: stop it, wait for the endpoint, start it again.
    Restart,
    /// Run the host in the foreground (entry points use this).
    Run {
        /// Remote listener for this run (`host:port`); not saved.
        #[arg(long, value_name = "HOST:PORT", conflicts_with = "no_listen")]
        listen: Option<String>,
        /// Refuse the saved listener for this run.
        #[arg(long)]
        no_listen: bool,
    },
    /// Start the background host (or report the running one).
    Start {
        /// Save the remote listener every later boot binds (`host:port`).
        #[arg(long, value_name = "HOST:PORT", conflicts_with = "no_listen")]
        listen: Option<String>,
        /// Refuse the saved listener for this run and clear it.
        #[arg(long)]
        no_listen: bool,
        /// Also serve the browser UI (the pair URL lands in `openbutler web`).
        #[arg(long)]
        web: bool,
    },
}

/// Runs one command and returns its rendered stdout text.
///
/// The data directory is a parameter (not a hidden env read) so tests can
/// point the same code at a fixture endpoint; `main` passes the real one.
pub async fn execute(command: Command, data_dir: &std::path::Path) -> Result<String, CliError> {
    match command {
        Command::Status => commands::status::run(data_dir).await,
        Command::Sessions { command } => commands::sessions::run(data_dir, command).await,
        Command::Events { command } => commands::ledger::events(data_dir, command).await,
        Command::Actions { command } => commands::ledger::actions(data_dir, command).await,
        Command::Daemon { command } => match command.unwrap_or(DaemonCommand::Status) {
            DaemonCommand::Status => commands::daemon::status(data_dir).await,
            DaemonCommand::Stop => commands::daemon::stop(data_dir).await,
            DaemonCommand::Restart => commands::daemon::restart(data_dir).await,
            DaemonCommand::Run { listen, no_listen } => {
                commands::daemon::run_foreground(data_dir, listen, no_listen)
                    .await
                    .map(|_| String::new())
            }
            DaemonCommand::Start {
                listen,
                no_listen,
                web,
            } => commands::daemon::start(data_dir, listen, no_listen, web).await,
        },
        Command::Stop => commands::daemon::stop(data_dir).await,
        Command::Approve { index } => commands::approve::run(data_dir, index).await,
        Command::ListModels {
            provider,
            filter,
            limit,
        } => commands::models::list(data_dir, provider, filter, limit).await,
        Command::Restart => commands::daemon::restart(data_dir).await,
        Command::Logs {
            lines,
            file,
            level,
            event,
            json,
        } => commands::logs::run(data_dir, lines as usize, file, level, event, json).await,
        Command::Usage {
            days,
            session,
            model,
            json,
        } => commands::usage::run(data_dir, days, session, model, json).await,
        // The wizard writes to the real terminal as it goes; through this
        // buffered path the guard fires first, so a non-terminal caller
        // gets the refusal sentence and no prompts.
        Command::Setup => commands::setup::run(data_dir).await.map(|()| String::new()),
        Command::Schedule { words } => commands::schedule::run(data_dir, &words).await,
        Command::Web => commands::web::run(data_dir).await,
        Command::Mcp { action } => commands::mcp::run(data_dir, action).await,
        Command::Config => commands::config::run(data_dir).await,
        Command::About => commands::about::run(data_dir).await,
        Command::Workspace {
            command: WorkspaceCommand::Default { path },
        } => commands::workspace::run(data_dir, path).await,
        Command::Skills { workspace } => commands::skills::run(data_dir, workspace).await,
        Command::Keys { command } => {
            let request = match command {
                None | Some(KeysCommand::List) => commands::keys::KeysRequest::List,
                Some(KeysCommand::Add { provider }) => {
                    commands::keys::KeysRequest::Add { provider }
                }
                Some(KeysCommand::Remove { provider }) => {
                    commands::keys::KeysRequest::Remove { provider }
                }
            };
            commands::keys::run(data_dir, request).await
        }
        Command::Permissions {
            mode,
            review_timeout,
            approval_timeout,
            review_model,
        } => {
            commands::permissions::run(
                data_dir,
                mode,
                review_timeout,
                approval_timeout,
                review_model,
            )
            .await
        }
        // The TUI owns the terminal; through this buffered path the guard
        // fires first, so a non-terminal caller gets the refusal sentence.
        Command::Tui => crate::tui::run(data_dir).await.map(|()| String::new()),
        Command::Connect {
            target,
            username,
            forget,
        } => {
            commands::connect::run(
                data_dir,
                target,
                username,
                forget,
                &mut commands::connect::TerminalPrompt,
            )
            .await
        }
        Command::Admin {
            command: AdminCommand::Create { username },
        } => {
            commands::admin::create(data_dir, &username, &mut commands::connect::TerminalPrompt)
                .await
        }
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
    /// The host could not be spawned or died during boot (diagnosed
    /// from the machine log's own words, source-style).
    #[error("{0}")]
    Start(String),
    /// The stored backend cannot be used (the message names the file and the
    /// remedy).
    #[error("{0}")]
    Backend(String),
}

/// Parses real arguments, runs one command, prints, and picks the exit code:
/// 0 success, 1 command/daemon failure, 2 usage (clap's own exit).
pub fn main_entry() -> std::process::ExitCode {
    let cli = Cli::parse();
    let command = if cli.list_models {
        if cli.command.is_some() {
            eprintln!("openbutler: --list-models does not combine with a command.");
            return std::process::ExitCode::from(2);
        }
        Some(Command::ListModels {
            provider: cli.provider,
            filter: cli.filter,
            limit: cli.limit,
        })
    } else {
        cli.command
    };
    let Some(command) = command else {
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
    let outcome: Result<i32, CliError> = runtime.block_on(async {
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
                .map(|()| 0)
            }
            // The foreground host owns this process's exit status.
            Command::Daemon {
                command: Some(DaemonCommand::Run { listen, no_listen }),
            } => commands::daemon::run_foreground(&data_dir::data_dir(), listen, no_listen).await,
            // The wizard prompts on the real stdio itself; it must not be
            // wrapped in the buffered `println!` path.
            Command::Setup => commands::setup::run(&data_dir::data_dir())
                .await
                .map(|()| 0),
            // The manager prompts on the real stdio itself.
            Command::Mcp { action } => {
                commands::mcp::run(&data_dir::data_dir(), action)
                    .await
                    .map(|text| {
                        if !text.is_empty() {
                            println!("{text}");
                        }
                        0
                    })
            }
            // The TUI draws on the real terminal itself.
            Command::Tui => crate::tui::run(&data_dir::data_dir()).await.map(|()| 0),
            other => {
                let text = execute(other, &data_dir::data_dir()).await?;
                println!("{text}");
                Ok(0)
            }
        }
    });
    match outcome {
        Ok(0) => std::process::ExitCode::SUCCESS,
        Ok(code) => std::process::ExitCode::from(u8::try_from(code).unwrap_or(1)),
        Err(error) => {
            eprintln!("openbutler: {error}");
            std::process::ExitCode::from(1)
        }
    }
}
