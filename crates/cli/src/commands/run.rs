//! `openbutler run "<request>"` — the one-shot agent run (the legacy
//! `--ask` flow, redesigned as a subcommand): attach a fresh chat, send
//! the request, stream the answer to stdout, prompt on a terminal for
//! approvals and questions, and leave on completion or the source's two
//! deadlines.
//!
//! The two deadlines keep their source meaning: the start window never
//! resets (the host must begin reporting progress within it), while the
//! idle window resets on every state change while busy — "progress", not
//! a wall-clock cap on the run itself.

use std::collections::{BTreeMap, HashSet};
use std::io::{IsTerminal, Write};
use std::path::Path;
use std::time::Duration;

use openbutler_protocol::entry::SessionEntry;
use openbutler_protocol::session::run::{PendingQuestion, RunState, ToolCallRequest};
use openbutler_protocol::{
    AttachResult, DaemonEvent, DaemonRequest, DaemonResponse, RuntimeViewData, SessionEntriesPatch,
};
use serde_json::{Value, json};

use crate::CliError;
use crate::client::Client;

/// Knobs of the one-shot flow (testable deadlines, legacy defaults).
pub struct RunOptions {
    /// The request text the agent runs.
    pub request: String,
    /// First progress must arrive within this window (legacy 5 seconds).
    pub start_timeout: Duration,
    /// No state change while busy for this long stops the run
    /// (legacy 10 minutes).
    pub idle_timeout: Duration,
}

impl RunOptions {
    /// One run with the source's deadline defaults.
    pub fn new(request: impl Into<String>) -> Self {
        Self {
            request: request.into(),
            start_timeout: Duration::from_secs(5),
            idle_timeout: Duration::from_secs(600),
        }
    }
}

/// Runs one request to completion, writing the answer to `out` and
/// notices/prompts/failures to `err`.
pub async fn run(
    data_dir: &Path,
    options: RunOptions,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<(), CliError> {
    let mut client = Client::connect(data_dir).await?;

    // A fresh chat: the daemon opens the draft and hands out the binding.
    let attach = DaemonRequest::new(uuid::Uuid::new_v4().to_string(), "attach")
        .with("clientKind", json!("terminal"))
        .with("newChatSession", json!(true));
    let attach_value = ask(&mut client, attach).await?;
    let attach: AttachResult =
        serde_json::from_value(attach_value).map_err(|source| CliError::Decode {
            action: "attach",
            source,
        })?;

    // The header (the source's section/kv/info, in plain text).
    let runtime: &RuntimeViewData = &attach.runtime;
    let destination = runtime
        .provider_name
        .as_deref()
        .unwrap_or(&runtime.provider_id);
    let _ = writeln!(out, "OpenButler agent");
    let _ = writeln!(out, "Model: {} / {}", runtime.model_name, runtime.model_id);
    let _ = writeln!(out, "Provider: {destination}");
    let _ = writeln!(
        out,
        "Your request, tool definitions, and approved tool results will be sent to {destination}."
    );

    let workspace_key = attach.workspace_key.unwrap_or_default();
    let mut render = Render::default();

    // The start window opens before the send (the source starts it at
    // subscribe time) and never resets.
    let start_deadline = tokio::time::Instant::now() + options.start_timeout;
    let mut idle_deadline: Option<tokio::time::Instant> = None;

    // The daemon reads one message object (text and images together), the
    // same shape its parseQueuedMessage expects.
    let send = DaemonRequest::new(uuid::Uuid::new_v4().to_string(), "session.send")
        .with("message", json!({ "text": options.request, "images": [] }))
        .with("workspaceKey", json!(workspace_key));
    let response = tokio::time::timeout_at(
        start_deadline,
        client.call_with_events(&send, |event| {
            render.apply(&event, out, err);
        }),
    )
    .await
    .map_err(|_| start_failure())?
    .map_err(CliError::Client)?;
    // A refusal is the daemon sentence, not a silent start failure.
    if let DaemonResponse::Failure { error, .. } = response {
        return Err(CliError::Answer(error));
    }

    loop {
        let deadline = if !render.run_started {
            start_deadline
        } else {
            idle_deadline.unwrap_or_else(|| tokio::time::Instant::now() + options.idle_timeout)
        };
        let next = tokio::time::timeout_at(deadline, async {
            tokio::select! {
                interrupted = tokio::signal::ctrl_c() => {
                    let _ = interrupted;
                    Ok::<Option<DaemonEvent>, crate::client::ClientError>(None)
                }
                event = client.next_event() => event.map(Some),
            }
        })
        .await;
        match next {
            // Ctrl-C: stop the run and leave cleanly (the source's
            // interrupt handler sets no exit code).
            Ok(Ok(None)) => {
                let stop = DaemonRequest::new(uuid::Uuid::new_v4().to_string(), "session.stop");
                let _ = client.call(&stop).await;
                return Ok(());
            }
            Ok(Ok(Some(event))) => {
                render.apply(&event, out, err);
                if render.run_started && render.busy {
                    idle_deadline = Some(tokio::time::Instant::now() + options.idle_timeout);
                }
            }
            Ok(Err(error)) => return Err(CliError::Client(error)),
            // The window elapsed: start failure or the idle stop.
            Err(_) => {
                let stop = DaemonRequest::new(uuid::Uuid::new_v4().to_string(), "session.stop");
                let _ = client.call(&stop).await;
                return Err(if render.run_started {
                    CliError::Run(
                        "One-shot run stopped: no session progress was reported for 10 minutes."
                            .to_owned(),
                    )
                } else {
                    start_failure()
                });
            }
        }
        if render.finished {
            if render.printed_text {
                let _ = writeln!(out);
            }
            return Ok(());
        }
        // Pending prompts: asked once each, answered over the same
        // socket while further events keep rendering.
        if let Some((request, approval_id)) = render.approval.take()
            && render.prompted_approvals.insert(approval_id.clone())
        {
            let allowed = prompt_approval(&request, err);
            let reply = DaemonRequest::new(uuid::Uuid::new_v4().to_string(), "session.approve")
                .with("approvalId", json!(approval_id))
                .with("decision", json!(if allowed { "allow" } else { "deny" }));
            let response = client
                .call_with_events(&reply, |event| render.apply(&event, out, err))
                .await?;
            if let DaemonResponse::Failure { error, .. } = response {
                return Err(CliError::Answer(error));
            }
        } else if let Some(question) = render.question.take()
            && render.prompted_questions.insert(question.id.clone())
        {
            let answer = prompt_question(&question, err);
            let reply = DaemonRequest::new(uuid::Uuid::new_v4().to_string(), "session.answer")
                .with("questionId", json!(question.id))
                .with(
                    "answer",
                    match answer {
                        Some(text) => Value::String(text),
                        None => Value::Null,
                    },
                );
            let response = client
                .call_with_events(&reply, |event| render.apply(&event, out, err))
                .await?;
            if let DaemonResponse::Failure { error, .. } = response {
                return Err(CliError::Answer(error));
            }
        }
    }
}

fn start_failure() -> CliError {
    CliError::Run("The background host did not start the one-shot run.".to_owned())
}

/// Sends one stateless action and returns its `result`.
async fn ask(client: &mut Client, request: DaemonRequest) -> Result<Value, CliError> {
    match client.call(&request).await? {
        DaemonResponse::Success { result, .. } => Ok(result),
        DaemonResponse::Failure { error, .. } => Err(CliError::Answer(error)),
    }
}

/// Asks for an approval on a terminal; without one the call is denied
/// (a background run never blocks on a human).
fn prompt_approval(request: &ToolCallRequest, err: &mut dyn Write) -> bool {
    if !std::io::stdin().is_terminal() {
        let _ = writeln!(
            err,
            "warning: {} needs approval but no terminal is attached; denying",
            request.name
        );
        return false;
    }
    let _ = writeln!(err, "The tool call needs approval: {}", request.name);
    let _ = write!(err, "Allow? [y/N] ");
    let _ = err.flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return false;
    }
    let answer = line.trim();
    answer.eq_ignore_ascii_case("y") || answer.eq_ignore_ascii_case("yes")
}

/// Asks a question on a terminal; an empty line dismisses it (the
/// source's `answer: null`), and no terminal dismisses it too.
fn prompt_question(question: &PendingQuestion, err: &mut dyn Write) -> Option<String> {
    if !std::io::stdin().is_terminal() {
        let _ = writeln!(
            err,
            "warning: the agent asks a question but no terminal is attached; dismissing"
        );
        return None;
    }
    let _ = writeln!(err, "The agent asks: {}", question.question);
    if let Some(options) = &question.options {
        for (index, option) in options.iter().enumerate() {
            let _ = writeln!(err, "  {}. {option}", index + 1);
        }
    }
    let _ = write!(err, "> ");
    let _ = err.flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return None;
    }
    let answer = line.trim().to_owned();
    if answer.is_empty() {
        None
    } else {
        Some(answer)
    }
}

/// Everything one run renders and remembers (the source's one-shot
/// snapshot handler, split per event kind).
#[derive(Debug, Default)]
struct Render {
    /// Entry index → the assistant text already printed (prefix diffs).
    assistant: BTreeMap<i64, String>,
    /// Entry index → announced tool state (`result shown`).
    tools: BTreeMap<i64, (String, bool)>,
    /// Notice ordinals printed so far (the source's visibleNoticeCount).
    notice_printed: usize,
    printed_text: bool,
    /// Entries known (absolute count for append ordering).
    count: i64,
    busy: bool,
    run_started: bool,
    finished: bool,
    approval: Option<(ToolCallRequest, String)>,
    question: Option<PendingQuestion>,
    prompted_approvals: HashSet<String>,
    prompted_questions: HashSet<String>,
}

impl Render {
    fn apply(&mut self, event: &DaemonEvent, out: &mut dyn Write, err: &mut dyn Write) {
        match event {
            DaemonEvent::Snapshot { snapshot, .. } => {
                self.apply_run(&snapshot.run);
                self.render_full(&snapshot.session.entries, out, err);
            }
            DaemonEvent::Patch { patch, .. } => {
                self.apply_run(&patch.run);
                if let Some(entries) = &patch.entries_patch {
                    self.apply_entries_patch(entries, out, err);
                }
            }
            DaemonEvent::Notification { title, message, .. } => {
                let _ = writeln!(err, "notice: {title}: {message}");
            }
        }
    }

    fn apply_run(&mut self, run: &RunState) {
        self.busy = run.busy;
        if run.busy {
            self.run_started = true;
        }
        self.approval = match (&run.pending_approval, &run.pending_approval_id) {
            (Some(request), Some(id)) => Some((request.clone(), id.clone())),
            _ => None,
        };
        self.question = run.pending_question.clone();
        if self.run_started && !run.busy {
            self.finished = true;
        }
    }

    /// Full-transcript rendering (snapshot / replace-all): the source's
    /// prefix diffs, notice tail, and tool announce-once rules.
    fn render_full(&mut self, entries: &[SessionEntry], out: &mut dyn Write, err: &mut dyn Write) {
        let mut notice_ordinal = 0usize;
        for (index, entry) in entries.iter().enumerate() {
            let index = index as i64;
            if let SessionEntry::Notice { text, .. } = entry {
                if notice_ordinal >= self.notice_printed {
                    let _ = writeln!(err, "notice: {text}");
                }
                notice_ordinal += 1;
                continue;
            }
            self.render_entry(index, entry, out, err);
        }
        self.notice_printed = notice_ordinal;
        self.count = entries.len() as i64;
    }

    fn apply_entries_patch(
        &mut self,
        patch: &SessionEntriesPatch,
        out: &mut dyn Write,
        err: &mut dyn Write,
    ) {
        match patch {
            SessionEntriesPatch::ReplaceAll { entries } => self.render_full(entries, out, err),
            SessionEntriesPatch::Delta {
                append,
                replace,
                append_text,
            } => {
                for (offset, entry) in append.iter().enumerate() {
                    let index = self.count + offset as i64;
                    if let SessionEntry::Notice { text, .. } = entry {
                        let _ = writeln!(err, "notice: {text}");
                        self.notice_printed += 1;
                        continue;
                    }
                    self.render_entry(index, entry, out, err);
                }
                self.count += append.len() as i64;
                for replaced in replace {
                    if let SessionEntry::Notice { .. } = replaced.entry {
                        // The source never reprints notices on rewrite.
                        continue;
                    }
                    self.render_entry(replaced.index, &replaced.entry, out, err);
                }
                for delta in append_text {
                    if delta.delta.is_empty() {
                        continue;
                    }
                    let _ = write!(out, "{}", delta.delta);
                    self.printed_text = true;
                    self.assistant
                        .entry(delta.index)
                        .or_default()
                        .push_str(&delta.delta);
                }
            }
        }
    }

    /// One entry: assistant text by prefix diff, tool announce-once, and
    /// nothing else (the one-shot lane shows notices, answers, tools).
    fn render_entry(
        &mut self,
        index: i64,
        entry: &SessionEntry,
        out: &mut dyn Write,
        _err: &mut dyn Write,
    ) {
        match entry {
            SessionEntry::Assistant { text, .. } => {
                if text.is_empty() {
                    return;
                }
                let previous = self.assistant.get(&index).map(String::as_str).unwrap_or("");
                match text.strip_prefix(previous) {
                    Some(delta) if !delta.is_empty() => {
                        let _ = write!(out, "{delta}");
                        self.printed_text = true;
                    }
                    Some(_) => {}
                    None => {
                        if self.printed_text {
                            let _ = writeln!(out);
                        }
                        let _ = write!(out, "{text}");
                        self.printed_text = true;
                    }
                }
                self.assistant.insert(index, text.clone());
            }
            SessionEntry::Tool {
                tool,
                running,
                preparing,
                result,
                command,
                tool_title,
                ..
            } => {
                if *preparing == Some(true) {
                    return;
                }
                let name = tool
                    .clone()
                    .or_else(|| tool_title.clone())
                    .or_else(|| command.clone())
                    .unwrap_or_else(|| "tool".to_owned());
                let running = *running == Some(true);
                match self.tools.get_mut(&index) {
                    None => {
                        if self.printed_text {
                            let _ = writeln!(out);
                            let _ = writeln!(out);
                        }
                        let _ = writeln!(out, "Tool: {name}");
                        if !running {
                            let _ = writeln!(out, "{result}");
                        }
                        self.tools.insert(index, (name, !running));
                    }
                    Some(seen) => {
                        if !seen.1 && !running {
                            let _ = writeln!(out, "{result}");
                            seen.1 = true;
                        }
                    }
                }
            }
            _ => {}
        }
    }
}
