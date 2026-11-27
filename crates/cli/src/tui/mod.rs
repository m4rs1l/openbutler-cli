//! `openbutler tui` — the interactive session.
//!
//! Three parts, each with one job: [`model`] decides (pure), [`render`]
//! draws, and this shell owns the terminal and the socket. The screen opens
//! on the saved-session list and never attaches or creates a chat by itself
//! (owner ruling: there is no default session); opening one is a key press.
//! Every value shown comes from the daemon's frames — the shell computes
//! nothing but layout.

pub mod model;
pub mod render;

use std::io::IsTerminal;
use std::path::Path;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use openbutler_protocol::{
    AttachResult, DaemonEvent, DaemonRequest, DaemonResponse, SessionSummary,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use serde_json::json;

use crate::CliError;
use crate::client::{Client, ClientError};
use model::{App, Intent, Key, SessionRow};

/// The refusal when stdout is not a terminal (no source sentence exists;
/// this build's own guard, like `setup`'s).
pub const TTY_REFUSAL: &str = "tui needs an interactive terminal. Run it from a real terminal.";

/// Runs the interactive session until the user quits.
pub async fn run(data_dir: &Path) -> Result<(), CliError> {
    if !std::io::stdout().is_terminal() {
        return Err(CliError::Answer(TTY_REFUSAL.to_owned()));
    }
    let mut client = Client::connect(data_dir).await?;
    let mut app = App::new();
    app.set_rows(fetch_sessions(&mut client).await?);

    enable_raw_mode().map_err(io_fault)?;
    execute!(std::io::stdout(), EnterAlternateScreen).map_err(io_fault)?;
    let outcome = run_loop(&mut app, &mut client).await;
    // The terminal is restored on every path, a failure included.
    let _ = disable_raw_mode();
    let _ = execute!(std::io::stdout(), LeaveAlternateScreen);
    outcome
}

fn io_fault(error: std::io::Error) -> CliError {
    CliError::Client(ClientError::Io(error))
}

async fn run_loop(app: &mut App, client: &mut Client) -> Result<(), CliError> {
    let mut terminal = Terminal::new(CrosstermBackend::new(std::io::stdout())).map_err(io_fault)?;
    let mut dirty = true;
    loop {
        if dirty {
            terminal
                .draw(|frame| render::draw(frame, app))
                .map_err(io_fault)?;
            dirty = false;
        }
        // Daemon frames first, on a short poll so keys stay responsive; the
        // list screen has no live lane to read.
        if app.screen == model::Screen::Chat {
            match tokio::time::timeout(Duration::from_millis(30), client.next_event()).await {
                Ok(Ok(event)) => {
                    app.apply_event(&event);
                    dirty = true;
                    continue;
                }
                Ok(Err(error)) => return Err(CliError::Client(error)),
                Err(_) => {}
            }
        } else {
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
        while event::poll(Duration::ZERO).map_err(io_fault)? {
            let Event::Key(key) = event::read().map_err(io_fault)? else {
                continue;
            };
            if key.kind != KeyEventKind::Press {
                continue;
            }
            let Some(mapped) = map_key(key) else {
                continue;
            };
            let intent = app.handle_key(mapped);
            if handle_intent(client, app, intent).await? {
                return Ok(());
            }
            dirty = true;
        }
    }
}

/// Maps one crossterm key onto the model's vocabulary.
fn map_key(key: event::KeyEvent) -> Option<Key> {
    if key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('C'))
    {
        return Some(Key::CtrlC);
    }
    Some(match key.code {
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        KeyCode::Enter => Key::Enter,
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Esc => Key::Escape,
        KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            Key::Char(character)
        }
        _ => return None,
    })
}

/// Performs one intent; `true` means quit.
async fn handle_intent(
    client: &mut Client,
    app: &mut App,
    intent: Intent,
) -> Result<bool, CliError> {
    match intent {
        Intent::None => {}
        Intent::Quit => return Ok(true),
        Intent::Reload => {
            let rows = fetch_sessions(client).await?;
            app.set_rows(rows);
        }
        Intent::Open(index) => {
            let Some(row) = app.rows.get(index) else {
                return Ok(false);
            };
            let id = row.id.clone();
            attach(client, app, Some(&id)).await?;
        }
        Intent::NewChat => attach(client, app, None).await?,
        Intent::Back => app.back(),
        Intent::Send(text) => send(client, app, text).await?,
        Intent::Approve(allowed) => approve(client, app, allowed).await?,
        Intent::Answer(text) => answer(client, app, text).await?,
    }
    Ok(false)
}

/// `session.list` — the opening screen's rows.
async fn fetch_sessions(client: &mut Client) -> Result<Vec<SessionRow>, CliError> {
    let value = call(client, DaemonRequest::new(id(), "session.list")).await?;
    let sessions: Vec<SessionSummary> =
        serde_json::from_value(value).map_err(|source| CliError::Decode {
            action: "session.list",
            source,
        })?;
    Ok(sessions.iter().map(SessionRow::from_summary).collect())
}

/// `attach`: an existing chat by id, or a fresh one (`newChatSession`).
async fn attach(client: &mut Client, app: &mut App, chat: Option<&str>) -> Result<(), CliError> {
    let mut request = DaemonRequest::new(id(), "attach").with("clientKind", json!("terminal"));
    request = match chat {
        Some(chat) => request.with("chatSessionId", json!(chat)),
        None => request.with("newChatSession", json!(true)),
    };
    let value = call(client, request).await?;
    let attach: AttachResult =
        serde_json::from_value(value).map_err(|source| CliError::Decode {
            action: "attach",
            source,
        })?;
    app.open(&attach);
    Ok(())
}

/// `session.send` on the open chat; frames that arrive before the answer are
/// applied afterwards, in order.
async fn send(client: &mut Client, app: &mut App, text: String) -> Result<(), CliError> {
    if app.session_id.is_empty() {
        app.status = "no chat is open".to_owned();
        return Ok(());
    }
    let request = DaemonRequest::new(id(), "session.send")
        .with("message", json!({ "text": text, "images": [] }))
        .with("workspaceKey", json!(app.workspace_key));
    apply_with_events(client, app, request).await
}

async fn approve(client: &mut Client, app: &mut App, allowed: bool) -> Result<(), CliError> {
    let Some((_, approval_id)) = app.approval.clone() else {
        return Ok(());
    };
    let request = DaemonRequest::new(id(), "session.approve")
        .with("approvalId", json!(approval_id))
        .with("decision", json!(if allowed { "allow" } else { "deny" }))
        .with("workspaceKey", json!(app.workspace_key));
    apply_with_events(client, app, request).await
}

async fn answer(client: &mut Client, app: &mut App, text: Option<String>) -> Result<(), CliError> {
    let Some(question) = app.question.clone() else {
        return Ok(());
    };
    let request = DaemonRequest::new(id(), "session.answer")
        .with("questionId", json!(question.id))
        .with(
            "answer",
            match text {
                Some(text) => json!(text),
                None => serde_json::Value::Null,
            },
        )
        .with("workspaceKey", json!(app.workspace_key));
    apply_with_events(client, app, request).await
}

/// Sends one request and folds the events that arrive before its answer
/// into the model, then the answer's own outcome into the status line.
async fn apply_with_events(
    client: &mut Client,
    app: &mut App,
    request: DaemonRequest,
) -> Result<(), CliError> {
    let mut arrived: Vec<DaemonEvent> = Vec::new();
    let response = client
        .call_with_events(&request, |event| arrived.push(event))
        .await?;
    for event in &arrived {
        app.apply_event(event);
    }
    match response {
        DaemonResponse::Success { .. } => {}
        DaemonResponse::Failure { error, .. } => app.status = error,
    }
    Ok(())
}

/// One request, its `result` or the daemon's refusal sentence.
async fn call(client: &mut Client, request: DaemonRequest) -> Result<serde_json::Value, CliError> {
    match client.call(&request).await? {
        DaemonResponse::Success { result, .. } => Ok(result),
        DaemonResponse::Failure { error, .. } => Err(CliError::Answer(error)),
    }
}

fn id() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn the_guard_refuses_without_a_terminal() {
        // A test process has no TTY on stdout: the guard fires first, so the
        // command can never half-open a terminal.
        let outcome = futures_block_on(run(std::path::Path::new("/ob/data")));
        match outcome {
            Err(CliError::Answer(message)) => assert_eq!(message, TTY_REFUSAL),
            other => panic!("expected the terminal refusal, got {other:?}"),
        }
    }

    #[test]
    fn keys_map_onto_the_models_vocabulary() {
        use crossterm::event::KeyEvent;
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(Key::CtrlC)
        );
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE)),
            Some(Key::Char('a'))
        );
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            Some(Key::Escape)
        );
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE)),
            None
        );
    }

    /// A tiny blocking executor for the guard test (no runtime needed).
    fn futures_block_on<F: std::future::Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(future)
    }
}
