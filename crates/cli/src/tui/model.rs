//! The interactive session's state machine — pure, so every transition is
//! pinned without a terminal.
//!
//! Owner rulings that shape this file:
//! - **No default session:** the app opens on the session list and never
//!   attaches or creates a chat by itself; opening one is a key press.
//! - **Backend-owned:** every value shown comes from the daemon's frames
//!   (the attach snapshot, the patches, the list). Nothing here computes a
//!   count, a model name or a status the host did not send.

use openbutler_protocol::entry::SessionEntry;
use openbutler_protocol::session::run::{PendingQuestion, RunState, ToolCallRequest};
use openbutler_protocol::{AttachResult, DaemonEvent, SessionEntriesPatch, SessionSummary};

/// What the window shows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Screen {
    /// The saved-session list plus the new-chat entry (the opening screen).
    #[default]
    List,
    /// One attached chat.
    Chat,
}

/// How a transcript line reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    User,
    Assistant,
    Tool,
    Notice,
    Reasoning,
}

/// One transcript line (multi-line entry text arrives pre-split).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub kind: LineKind,
    pub text: String,
}

/// One row of the session list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRow {
    pub id: String,
    pub title: String,
    pub detail: String,
    pub running: bool,
}

impl SessionRow {
    /// The list row for one saved session (the daemon's summary, verbatim).
    pub fn from_summary(summary: &SessionSummary) -> Self {
        Self {
            id: summary.id.clone(),
            title: summary.title.clone(),
            detail: format!("{} / {}", summary.provider_id, summary.model_id),
            running: summary.running.unwrap_or(false),
        }
    }
}

/// What a key press asks the shell (I/O) to do; the model never dials.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Intent {
    None,
    Quit,
    /// Attach this row index.
    Open(usize),
    NewChat,
    Reload,
    /// Back to the list (the run keeps going host-side).
    Back,
    Send(String),
    Approve(bool),
    Answer(Option<String>),
}

/// A key the model understands (the shell maps crossterm onto this).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Up,
    Down,
    Enter,
    Backspace,
    Escape,
    Char(char),
    CtrlC,
}

/// The whole app state.
#[derive(Debug, Default)]
pub struct App {
    pub screen: Screen,
    pub rows: Vec<SessionRow>,
    pub selected: usize,
    pub status: String,
    pub title: String,
    pub lines: Vec<Line>,
    pub busy: bool,
    pub queued: u32,
    pub input: String,
    pub approval: Option<(ToolCallRequest, String)>,
    pub question: Option<PendingQuestion>,
    pub workspace_key: String,
    pub session_id: String,
    /// Absolute entries as the host sent them (patches apply here first).
    entries: Vec<SessionEntry>,
}

impl App {
    /// A fresh app on the list screen, nothing attached.
    pub fn new() -> Self {
        Self::default()
    }

    /// Replaces the list and keeps the selection in range.
    pub fn set_rows(&mut self, rows: Vec<SessionRow>) {
        self.rows = rows;
        if self.selected >= self.rows.len() {
            self.selected = self.rows.len().saturating_sub(1);
        }
    }

    /// Opens a chat from the host's attach answer: the snapshot is the
    /// transcript, the run state the status line, the key the send gate.
    pub fn open(&mut self, attach: &AttachResult) {
        self.screen = Screen::Chat;
        self.session_id = attach.snapshot.session.meta.id.clone();
        self.title = attach.snapshot.session.meta.title.clone();
        self.workspace_key = attach.workspace_key.clone().unwrap_or_default();
        self.entries = attach.snapshot.session.entries.clone();
        self.apply_run(&attach.snapshot.run);
        self.rebuild();
        self.input.clear();
        self.status.clear();
    }

    /// Back to the list (the chat stays open host-side).
    pub fn back(&mut self) {
        self.screen = Screen::List;
        self.entries.clear();
        self.lines.clear();
        self.session_id.clear();
        self.approval = None;
        self.question = None;
        self.status.clear();
    }

    /// Applies one live frame; frames for another chat are stale (a
    /// previous attachment's tail) and are dropped.
    pub fn apply_event(&mut self, event: &DaemonEvent) {
        match event {
            DaemonEvent::Snapshot { snapshot, .. }
                if !self.session_id.is_empty() && snapshot.session.meta.id != self.session_id =>
            {
                return;
            }
            DaemonEvent::Patch { patch, .. }
                if !self.session_id.is_empty() && patch.session.id != self.session_id =>
            {
                return;
            }
            _ => {}
        }
        match event {
            DaemonEvent::Snapshot { snapshot, .. } => {
                self.title = snapshot.session.meta.title.clone();
                self.entries = snapshot.session.entries.clone();
                self.apply_run(&snapshot.run);
                self.rebuild();
            }
            DaemonEvent::Patch { patch, .. } => {
                self.title = patch.session.title.clone();
                if let Some(entries) = &patch.entries_patch {
                    self.apply_entries_patch(entries);
                }
                self.apply_run(&patch.run);
                self.rebuild();
            }
            DaemonEvent::Notification { title, message, .. } => {
                self.status = format!("{title}: {message}");
            }
        }
    }

    fn apply_run(&mut self, run: &RunState) {
        self.busy = run.busy;
        self.queued = run.queued_message_count;
        self.approval = match (&run.pending_approval, &run.pending_approval_id) {
            (Some(request), Some(id)) => Some((request.clone(), id.clone())),
            _ => None,
        };
        self.question = run.pending_question.clone();
    }

    fn apply_entries_patch(&mut self, patch: &SessionEntriesPatch) {
        match patch {
            SessionEntriesPatch::ReplaceAll { entries } => self.entries = entries.clone(),
            SessionEntriesPatch::Delta {
                append,
                replace,
                append_text,
            } => {
                self.entries.extend(append.iter().cloned());
                for replaced in replace {
                    if let Some(slot) = self.entries.get_mut(replaced.index as usize) {
                        *slot = replaced.entry.clone();
                    }
                }
                for delta in append_text {
                    if let Some(SessionEntry::Assistant { text, .. }) =
                        self.entries.get_mut(delta.index as usize)
                    {
                        text.push_str(&delta.delta);
                    }
                }
            }
        }
    }

    /// Rebuilds the visible lines from the entries (the slim view: user,
    /// assistant, tool, notice and reasoning lines).
    pub fn rebuild(&mut self) {
        let mut lines: Vec<Line> = Vec::new();
        for entry in &self.entries {
            match entry {
                SessionEntry::User { text, .. } => {
                    push_text(&mut lines, LineKind::User, text, "> ");
                }
                SessionEntry::Assistant { text, .. } => {
                    push_text(&mut lines, LineKind::Assistant, text, "");
                }
                SessionEntry::Reasoning { text, .. } => {
                    push_text(&mut lines, LineKind::Reasoning, text, "~ ");
                }
                SessionEntry::Notice { text, .. } => {
                    push_text(&mut lines, LineKind::Notice, text, "\u{b7} ");
                }
                SessionEntry::Divider { text, .. } => {
                    let text = text
                        .clone()
                        .unwrap_or_else(|| "\u{2500}\u{2500}".to_owned());
                    lines.push(Line {
                        kind: LineKind::Notice,
                        text,
                    });
                }
                SessionEntry::Tool {
                    tool,
                    running,
                    preparing,
                    command,
                    tool_title,
                    ..
                } => {
                    if *preparing == Some(true) {
                        continue;
                    }
                    let name = tool
                        .clone()
                        .or_else(|| tool_title.clone())
                        .or_else(|| command.clone())
                        .unwrap_or_else(|| "tool".to_owned());
                    let state = if *running == Some(true) {
                        "running"
                    } else {
                        "done"
                    };
                    lines.push(Line {
                        kind: LineKind::Tool,
                        text: format!("{name} ({state})"),
                    });
                }
                SessionEntry::Compaction { status, .. } => {
                    let state = match status {
                        openbutler_protocol::entry::CompactionStatus::Running => "running",
                        openbutler_protocol::entry::CompactionStatus::Done => "done",
                        openbutler_protocol::entry::CompactionStatus::Failed => "failed",
                    };
                    lines.push(Line {
                        kind: LineKind::Notice,
                        text: format!("compaction: {state}"),
                    });
                }
            }
        }
        self.lines = lines;
    }

    /// One key press, decided here and acted on by the shell.
    pub fn handle_key(&mut self, key: Key) -> Intent {
        if key == Key::CtrlC {
            return Intent::Quit;
        }
        match self.screen {
            Screen::List => match key {
                Key::Up => {
                    self.selected = self.selected.saturating_sub(1);
                    Intent::None
                }
                Key::Down => {
                    if self.selected + 1 < self.rows.len() {
                        self.selected += 1;
                    }
                    Intent::None
                }
                Key::Enter => {
                    if self.rows.is_empty() {
                        Intent::NewChat
                    } else {
                        Intent::Open(self.selected)
                    }
                }
                Key::Char('n') | Key::Char('N') => Intent::NewChat,
                Key::Char('r') | Key::Char('R') => Intent::Reload,
                Key::Char('q') | Key::Char('Q') | Key::Escape => Intent::Quit,
                _ => Intent::None,
            },
            Screen::Chat => match key {
                Key::Escape => Intent::Back,
                Key::Enter => {
                    if self.question.is_some() {
                        let answer = if self.input.trim().is_empty() {
                            None
                        } else {
                            Some(self.input.trim().to_owned())
                        };
                        self.input.clear();
                        return Intent::Answer(answer);
                    }
                    let text = self.input.trim().to_owned();
                    if text.is_empty() {
                        return Intent::None;
                    }
                    self.input.clear();
                    Intent::Send(text)
                }
                Key::Backspace => {
                    self.input.pop();
                    Intent::None
                }
                Key::Char('y') | Key::Char('Y') if self.approval.is_some() => Intent::Approve(true),
                Key::Char('n') | Key::Char('N') if self.approval.is_some() => {
                    Intent::Approve(false)
                }
                Key::Char(character) => {
                    self.input.push(character);
                    Intent::None
                }
                _ => Intent::None,
            },
        }
    }

    /// The status line the window shows under the transcript.
    pub fn status_line(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if self.busy {
            parts.push("running".to_owned());
        }
        if self.queued > 0 {
            parts.push(format!("queued: {}", self.queued));
        }
        if let Some((request, _)) = &self.approval {
            parts.push(format!("needs approval: {} (y/n)", request.name));
        }
        if let Some(question) = &self.question {
            parts.push(format!("asks: {}", question.question));
        }
        if !self.status.is_empty() {
            parts.push(self.status.clone());
        }
        parts.join(" \u{b7} ")
    }
}

fn push_text(lines: &mut Vec<Line>, kind: LineKind, text: &str, prefix: &str) {
    if text.is_empty() {
        return;
    }
    for (index, part) in text.split('\n').enumerate() {
        let text = if index == 0 {
            format!("{prefix}{part}")
        } else {
            part.to_owned()
        };
        lines.push(Line { kind, text });
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use serde_json::json;

    fn meta_json() -> serde_json::Value {
        json!({
            "version": 1,
            "id": "sess-1",
            "title": "Old chat",
            "createdAt": "2026-11-23T00:00:00Z",
            "updatedAt": "2026-11-23T00:00:00Z",
            "providerId": "mock",
            "modelId": "m"
        })
    }

    fn attach_json() -> serde_json::Value {
        // `SessionView` flattens the record: the meta fields and `entries`
        // share one object.
        let mut session = meta_json();
        session["entries"] = json!([
            { "kind": "user", "text": "hello" },
            { "kind": "assistant", "text": "hi there" }
        ]);
        json!({
            "workspaceKey": "/ob/ws",
            "snapshot": {
                "session": session,
                "busy": false,
                "runningTurnId": null,
                "pendingApproval": null,
                "pendingApprovalId": null,
                "pendingQuestion": null,
                "queuedMessageCount": 0,
                "firstQueuedMessage": null,
                "queuedMessages": [],
                "sessionSaveError": null,
                "streamingAssistantIndex": null
            },
            "runtime": runtime_json(),
            "savedSessions": []
        })
    }

    fn runtime_json() -> serde_json::Value {
        json!({
            "workspaceRoot": "/ob/ws",
            "modelName": "M",
            "providerName": "Mock",
            "providerId": "mock",
            "modelId": "m",
            "contextWindowTokens": 1000,
            "supportsVision": false,
            "reasoningVariants": [],
            "reasoningMetadataSource": null,
            "reasoningSupport": "available",
            "reasoningDefault": "default",
            "reasoningPreference": "default",
            "showReasoningDefault": true
        })
    }

    #[test]
    fn opening_a_chat_uses_the_attach_snapshot() {
        let attach: AttachResult = serde_json::from_value(attach_json()).unwrap();
        let mut app = App::new();
        app.open(&attach);
        assert_eq!(app.screen, Screen::Chat);
        assert_eq!(app.session_id, "sess-1");
        assert_eq!(app.title, "Old chat");
        assert_eq!(app.workspace_key, "/ob/ws");
        assert_eq!(app.lines.len(), 2);
        assert_eq!(app.lines[0].text, "> hello");
        assert_eq!(app.lines[1].kind, LineKind::Assistant);
        assert!(!app.busy);
    }

    #[test]
    fn a_delta_patch_appends_and_extends_text() {
        let attach: AttachResult = serde_json::from_value(attach_json()).unwrap();
        let mut app = App::new();
        app.open(&attach);

        // One new user entry, then a text continuation of its reply slot.
        let patch = json!({
            "event": "patch",
            "version": 1,
            "patch": {
                "session": meta_json(),
                "entriesPatch": {
                    "kind": "delta",
                    "append": [{ "kind": "assistant", "text": "wor" }],
                    "replace": [],
                    "appendText": [{ "index": 2, "delta": "ld" }]
                },
                "busy": true,
                "runningTurnId": "t1",
                "pendingApproval": null,
                "pendingApprovalId": null,
                "pendingQuestion": null,
                "queuedMessageCount": 0,
                "firstQueuedMessage": null,
                "queuedMessages": [],
                "sessionSaveError": null,
                "streamingAssistantIndex": 2,
                "runtime": runtime_json()
            }
        });
        let event: DaemonEvent = serde_json::from_value(patch).unwrap();
        app.apply_event(&event);
        assert!(app.busy);
        assert_eq!(app.lines.len(), 3);
        assert_eq!(
            app.lines[2].text, "world",
            "the continuation lands in place"
        );
    }

    #[test]
    fn a_pending_approval_turns_y_and_n_into_decisions() {
        let attach: AttachResult = serde_json::from_value(attach_json()).unwrap();
        let mut app = App::new();
        app.open(&attach);
        let patch = json!({
            "event": "patch",
            "version": 1,
            "patch": {
                "session": meta_json(),
                "busy": true,
                "runningTurnId": "t1",
                "pendingApproval": {
                    "requestId": "call_1", "providerId": "mock", "name": "execute_command",
                    "input": { "command": "ls" }, "effect": { "change": "unknown", "data": "unknown", "network": "possible", "reversible": "unknown", "outbound": "model" },
                    "timeoutSeconds": null, "gateReason": null
                },
                "pendingApprovalId": "call_1",
                "pendingQuestion": null,
                "queuedMessageCount": 0,
                "firstQueuedMessage": null,
                "queuedMessages": [],
                "sessionSaveError": null,
                "streamingAssistantIndex": null,
                "runtime": runtime_json()
            }
        });
        let event: DaemonEvent = serde_json::from_value(patch).unwrap();
        app.apply_event(&event);
        assert!(
            app.status_line()
                .contains("needs approval: execute_command (y/n)")
        );
        assert_eq!(app.handle_key(Key::Char('y')), Intent::Approve(true));
        app.approval = None;
        assert_eq!(
            app.handle_key(Key::Char('n')),
            Intent::None,
            "no approval, no decision key"
        );
    }

    #[test]
    fn a_question_takes_the_input_line_as_its_answer() {
        let attach: AttachResult = serde_json::from_value(attach_json()).unwrap();
        let mut app = App::new();
        app.open(&attach);
        let patch = json!({
            "event": "patch",
            "version": 1,
            "patch": {
                "session": meta_json(),
                "busy": true,
                "runningTurnId": "t1",
                "pendingApproval": null,
                "pendingApprovalId": null,
                "pendingQuestion": { "id": "q1", "question": "Which one?", "options": ["a", "b"] },
                "queuedMessageCount": 0,
                "firstQueuedMessage": null,
                "queuedMessages": [],
                "sessionSaveError": null,
                "streamingAssistantIndex": null,
                "runtime": runtime_json()
            }
        });
        let event: DaemonEvent = serde_json::from_value(patch).unwrap();
        app.apply_event(&event);
        for character in "b".chars() {
            assert_eq!(app.handle_key(Key::Char(character)), Intent::None);
        }
        assert_eq!(
            app.handle_key(Key::Enter),
            Intent::Answer(Some("b".to_owned()))
        );
        assert!(app.input.is_empty());

        // An empty answer dismisses (the source's `answer: null`).
        assert_eq!(app.handle_key(Key::Enter), Intent::Answer(None));
    }

    #[test]
    fn the_list_never_opens_a_chat_by_itself() {
        let mut app = App::new();
        assert_eq!(
            app.screen,
            Screen::List,
            "no default session: the list opens"
        );
        assert_eq!(
            app.handle_key(Key::Down),
            Intent::None,
            "no rows, nothing to do"
        );
        assert_eq!(
            app.handle_key(Key::Enter),
            Intent::NewChat,
            "empty list offers a new chat"
        );
        assert_eq!(app.handle_key(Key::Char('n')), Intent::NewChat);
        assert_eq!(app.handle_key(Key::Escape), Intent::Quit);
        assert_eq!(app.handle_key(Key::CtrlC), Intent::Quit);
    }

    #[test]
    fn rows_keep_the_selection_in_range() {
        let mut app = App::new();
        let summary = |id: &str| SessionSummary {
            running: None,
            project: None,
            id: id.to_owned(),
            title: id.to_owned(),
            preview: String::new(),
            updated_at: "2026-11-23T00:00:00Z".to_owned(),
            last_activity_at: None,
            provider_id: "mock".to_owned(),
            model_id: "m".to_owned(),
            workspace_root: None,
        };
        app.set_rows(vec![
            SessionRow::from_summary(&summary("a")),
            SessionRow::from_summary(&summary("b")),
        ]);
        assert_eq!(app.handle_key(Key::Down), Intent::None);
        assert_eq!(app.selected, 1);
        assert_eq!(
            app.handle_key(Key::Down),
            Intent::None,
            "clamped at the end"
        );
        assert_eq!(app.selected, 1);
        assert_eq!(app.handle_key(Key::Enter), Intent::Open(1));
        app.set_rows(vec![SessionRow::from_summary(&summary("a"))]);
        assert_eq!(app.selected, 0, "a shorter list clamps the selection");
    }
}
