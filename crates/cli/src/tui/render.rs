//! Drawing: functions over the model only, so a `TestBackend` can pin every
//! screen without a terminal.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line as TextLine, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use super::model::{App, LineKind, Screen};

/// The whole frame for the current screen.
pub fn draw(frame: &mut Frame, app: &App) {
    match app.screen {
        Screen::List => draw_list(frame, app),
        Screen::Chat => draw_chat(frame, app),
    }
}

fn border(title: &str) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .title(title.to_owned())
}

/// The opening screen: the saved sessions, the new-chat entry, the keys.
fn draw_list(frame: &mut Frame, app: &App) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(1)])
        .split(frame.area());

    let mut lines: Vec<TextLine> = Vec::new();
    if app.rows.is_empty() {
        lines.push(TextLine::from(Span::styled(
            "  No sessions. Press n to start one.",
            Style::default().fg(Color::DarkGray),
        )));
    }
    for (index, row) in app.rows.iter().enumerate() {
        let selected = index == app.selected;
        let marker = if selected { ">" } else { " " };
        let running = if row.running { " [running]" } else { "" };
        let style = if selected {
            Style::default().add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        lines.push(TextLine::from(Span::styled(
            format!("{marker} {}{running}", row.title),
            style,
        )));
        lines.push(TextLine::from(Span::styled(
            format!("    {}", row.detail),
            Style::default().fg(Color::DarkGray),
        )));
    }

    let block = border("OpenButler - sessions");
    frame.render_widget(Paragraph::new(lines).block(block), layout[0]);
    frame.render_widget(
        Paragraph::new(Span::styled(
            " Up/Down select   Enter open   n new chat   r reload   q quit",
            Style::default().fg(Color::DarkGray),
        )),
        layout[1],
    );
}

/// One chat: the transcript tail, the state line, the input line, the keys.
fn draw_chat(frame: &mut Frame, app: &App) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),
            Constraint::Length(1),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .split(frame.area());

    let visible = layout[0].height.saturating_sub(2) as usize;
    let start = app.lines.len().saturating_sub(visible);
    let lines: Vec<TextLine> = app.lines[start..]
        .iter()
        .map(|line| {
            let (prefix, style) = match line.kind {
                LineKind::User => ("", Style::default().fg(Color::Cyan)),
                LineKind::Assistant => ("", Style::default()),
                LineKind::Tool => ("", Style::default().fg(Color::Yellow)),
                LineKind::Notice => ("", Style::default().fg(Color::DarkGray)),
                LineKind::Reasoning => ("", Style::default().fg(Color::DarkGray)),
            };
            TextLine::from(Span::styled(format!("{prefix}{}", line.text), style))
        })
        .collect();
    frame.render_widget(
        Paragraph::new(lines)
            .block(border(&app.title))
            .wrap(Wrap { trim: false }),
        layout[0],
    );

    frame.render_widget(
        Paragraph::new(Span::styled(
            app.status_line(),
            Style::default().fg(Color::Magenta),
        )),
        layout[1],
    );
    frame.render_widget(
        Paragraph::new(format!("> {}", app.input)).block(border("message")),
        layout[2],
    );
    let prompt = if app.approval.is_some() {
        " y allow   n deny   Ctrl+C quit"
    } else if app.question.is_some() {
        " type the answer, Enter to send   Esc back   Ctrl+C quit"
    } else {
        " Enter send   Esc back   Ctrl+C quit"
    };
    frame.render_widget(
        Paragraph::new(Span::styled(prompt, Style::default().fg(Color::DarkGray))),
        layout[3],
    );
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::tui::model::{Line, SessionRow};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn buffer_text(width: u16, height: u16, app: &App) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, app)).unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    #[test]
    fn the_list_shows_titles_markers_and_the_keys() {
        let mut app = App::new();
        app.set_rows(vec![
            SessionRow {
                id: "a".to_owned(),
                title: "Alpha chat".to_owned(),
                detail: "mock / m".to_owned(),
                running: true,
            },
            SessionRow {
                id: "b".to_owned(),
                title: "Beta chat".to_owned(),
                detail: "openai / gpt".to_owned(),
                running: false,
            },
        ]);
        app.selected = 1;
        let text = buffer_text(60, 10, &app);
        assert!(text.contains("Alpha chat [running]"), "{text}");
        assert!(text.contains("> Beta chat"), "the selection marker: {text}");
        assert!(text.contains("mock / m"), "{text}");
        assert!(text.contains("n new chat"), "the key hints: {text}");
    }

    #[test]
    fn an_empty_list_offers_a_new_chat() {
        let app = App::new();
        let text = buffer_text(60, 8, &app);
        assert!(text.contains("No sessions"), "{text}");
        assert!(text.contains("Enter open"), "{text}");
    }

    #[test]
    fn the_chat_shows_the_transcript_tail_the_state_and_the_input() {
        let mut app = App::new();
        app.screen = Screen::Chat;
        app.title = "Old chat".to_owned();
        app.lines = vec![
            Line {
                kind: LineKind::User,
                text: "> hello".to_owned(),
            },
            Line {
                kind: LineKind::Assistant,
                text: "hi there".to_owned(),
            },
        ];
        app.busy = true;
        app.input = "next".to_owned();
        let text = buffer_text(60, 14, &app);
        assert!(text.contains("hello"), "{text}");
        assert!(text.contains("hi there"), "{text}");
        assert!(text.contains("running"), "the state line: {text}");
        assert!(text.contains("> next"), "the input line: {text}");
    }

    #[test]
    fn an_approval_changes_the_prompt_line() {
        let mut app = App::new();
        app.screen = Screen::Chat;
        app.approval = Some((
            serde_json::from_value(serde_json::json!({
                "requestId": "c1", "providerId": "mock", "name": "execute_command",
                "input": {}, "effect": { "change": "unknown", "data": "unknown", "network": "possible", "reversible": "unknown", "outbound": "model" }, "timeoutSeconds": null, "gateReason": null
            }))
            .unwrap(),
            "c1".to_owned(),
        ));
        let text = buffer_text(70, 12, &app);
        assert!(text.contains("needs approval: execute_command"), "{text}");
        assert!(text.contains("y allow"), "{text}");
    }
}
