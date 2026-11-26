//! `openbutler mcp`: the global MCP server manager.
//!
//! Source: `cli-main.ts` (`showMcpServers` L380, `runMcpManager` L398).
//! The screen, the success sentences, the id rule and every field's meaning
//! are the host's (`mcp.manage`); this module only asks the source questions
//! and turns the JSON-shaped answers into the values the wire needs.
//! Documented divergence: numbered menus instead of the inquirer arrows, and
//! cancel is Ctrl+C/EOF (silent), like the rest of this CLI.

use std::io::IsTerminal;
use std::path::Path;

use serde_json::{Value, json};

use crate::CliError;
use crate::client::Client;
use crate::commands::daemon::call_fields;
use crate::commands::setup::{StdTerm, Term, confirm, input, select};
use crate::ui;

/// Runs the manager: `list` always works; `add`/`remove` and the bare menu
/// need an interactive terminal (the source's own split). The list paths
/// return the host's finished text; the interactive paths write to the
/// terminal themselves because prompts and answers interleave.
pub async fn run(data_dir: &Path, action: Option<String>) -> Result<String, CliError> {
    let action = match action.as_deref() {
        Some("list") => "list",
        Some("add") => "add",
        Some("remove") => "remove",
        Some(_) => {
            return Err(CliError::Answer(
                "Usage: openbutler mcp [list|add|remove]".to_owned(),
            ));
        }
        None => "",
    };
    let mut term = StdTerm;
    if action == "list" || !std::io::stdin().is_terminal() {
        let mut text = screen(&list(data_dir).await?);
        if action != "list" {
            text.push('\n');
            text.push_str(
                "Use an interactive terminal with openbutler mcp to add or remove servers.",
            );
        }
        return Ok(text);
    }

    let mut wanted = action.to_owned();
    loop {
        if wanted.is_empty() {
            let Some(picked) = select(
                &mut term,
                "Global MCP servers",
                &[
                    "List configured servers",
                    "Add a server",
                    "Remove a server",
                    "Done",
                ],
            ) else {
                return Ok(String::new());
            };
            wanted = ["list", "add", "remove", "done"][picked].to_owned();
        }
        match wanted.as_str() {
            "done" => return Ok(String::new()),
            "list" => print_lines(&mut term, &list(data_dir).await?),
            "add" => add(data_dir, &mut term).await?,
            "remove" => remove(data_dir, &mut term).await?,
            _ => {}
        }
        wanted.clear();
    }
}

/// One `mcp.manage` list call: the server set and the host's screen.
async fn list(data_dir: &Path) -> Result<Value, CliError> {
    call(data_dir, vec![("command", json!("list"))]).await
}

/// Adds one server from prompted answers. Every answer is collected whole
/// before the host sees it, so a half-configured server can never be saved.
async fn add(data_dir: &Path, term: &mut StdTerm) -> Result<(), CliError> {
    let Some(id) = input(term, "Server id (letters, digits, underscore, dash)") else {
        return Ok(());
    };
    let Some(transport) = select(
        term,
        "Transport",
        &[
            "Local process (stdio)",
            "Streamable HTTP",
            "Server-Sent Events (SSE)",
        ],
    ) else {
        return Ok(());
    };
    let server = if transport == 0 {
        let Some(command) = input(term, "Executable command") else {
            return Ok(());
        };
        let Some(args) = ask_json(
            term,
            "Arguments as a JSON string array",
            "Enter a JSON array of strings, for example [\"--stdio\"].",
            |value| {
                value
                    .as_array()
                    .is_some_and(|items| items.iter().all(Value::is_string))
            },
            json!([]),
        ) else {
            return Ok(());
        };
        let Some(env) = ask_json(
            term,
            "Environment name to host-variable references as JSON (values are never saved), or {}",
            "Enter a JSON object mapping server env names to host environment variable names.",
            Value::is_object,
            json!({}),
        ) else {
            return Ok(());
        };
        let Some(cwd) = input(term, "Working directory (optional)") else {
            return Ok(());
        };
        stdio_server(&command, args, env, &cwd)
    } else {
        let Some(url) = input(term, "HTTP(S) endpoint URL") else {
            return Ok(());
        };
        let Some(headers) = ask_json(
            term,
            "Header name to host-variable references as JSON (values are never saved), or {}",
            "Enter a JSON object mapping header names to host environment variable names.",
            Value::is_object,
            json!({}),
        ) else {
            return Ok(());
        };
        http_server(transport, &url, headers)
    };
    let result = call(
        data_dir,
        vec![
            ("command", json!("add")),
            ("id", json!(id.trim())),
            ("server", server),
        ],
    )
    .await?;
    print_lines(term, &result);
    Ok(())
}

/// Removes one server after a confirmation; the empty set answers the
/// source's own sentence instead of an empty menu.
async fn remove(data_dir: &Path, term: &mut StdTerm) -> Result<(), CliError> {
    let listed = list(data_dir).await?;
    let ids: Vec<String> = listed["servers"]
        .as_array()
        .map(|servers| {
            servers
                .iter()
                .filter_map(|server| server["id"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    if ids.is_empty() {
        term.write("No MCP servers are configured.");
        return Ok(());
    }
    let labels: Vec<&str> = ids.iter().map(String::as_str).collect();
    let Some(picked) = select(term, "Choose a server to remove", &labels) else {
        return Ok(());
    };
    let Some(confirmed) = confirm(term, &format!("Remove global MCP server {}?", ids[picked]))
    else {
        return Ok(());
    };
    if !confirmed {
        return Ok(());
    }
    let result = call(
        data_dir,
        vec![("command", json!("remove")), ("id", json!(ids[picked]))],
    )
    .await?;
    print_lines(term, &result);
    Ok(())
}

/// The stdio body the source saves: the command trimmed, the parsed args and
/// env references, and `cwd` only when one was typed (an empty answer is
/// absent, not `""`).
fn stdio_server(command: &str, args: Value, env: Value, cwd: &str) -> Value {
    let mut server = json!({
        "transport": "stdio",
        "command": command.trim(),
        "args": args,
        "env": env,
    });
    if !cwd.trim().is_empty() {
        server["cwd"] = json!(cwd.trim());
    }
    server
}

/// The HTTP-family body: the chosen menu slot names the transport
/// (`http`/`sse`), exactly like the source's select values.
fn http_server(slot: usize, url: &str, headers: Value) -> Value {
    json!({
        "transport": if slot == 1 { "http" } else { "sse" },
        "url": url.trim(),
        "headers": headers,
    })
}

/// One `mcp.manage` call with its payload fields.
async fn call(data_dir: &Path, fields: Vec<(&str, Value)>) -> Result<Value, CliError> {
    let mut client = Client::connect(data_dir).await?;
    call_fields(&mut client, "mcp.manage", fields).await
}

/// The host's lines as text, painted through this client's palette.
fn screen(result: &Value) -> String {
    let lines: Vec<ui::ScreenLine> =
        serde_json::from_value(result["lines"].clone()).unwrap_or_default();
    lines
        .iter()
        .map(|line| ui::toned(line.tone, &line.text))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Prints the host's lines through the glyph palette.
fn print_lines(term: &mut StdTerm, result: &Value) {
    let lines: Vec<ui::ScreenLine> =
        serde_json::from_value(result["lines"].clone()).unwrap_or_default();
    for line in &lines {
        term.write(&ui::toned(line.tone, &line.text));
    }
}

/// Asks until the answer parses as the expected JSON shape, re-prompting
/// with the source's own validation sentence (parsing is collection, not a
/// decision: the host still validates every field's meaning). An empty
/// answer takes the source's default.
fn ask_json(
    term: &mut StdTerm,
    message: &str,
    refusal: &str,
    accepts: impl Fn(&Value) -> bool,
    default: Value,
) -> Option<Value> {
    loop {
        let answer = input(term, message)?;
        let text = answer.trim();
        if text.is_empty() {
            return Some(default.clone());
        }
        if let Ok(parsed) = serde_json::from_str::<Value>(text)
            && accepts(&parsed)
        {
            return Some(parsed);
        }
        term.write(refusal);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn stdio_body_omits_a_blank_cwd() {
        let server = stdio_server("  npx ", json!(["-y"]), json!({"T": "HOST"}), "   ");
        assert_eq!(
            server,
            json!({"transport": "stdio", "command": "npx", "args": ["-y"], "env": {"T": "HOST"}})
        );
        let with_cwd = stdio_server("npx", json!([]), json!({}), " /tmp/work ");
        assert_eq!(with_cwd["cwd"], "/tmp/work");
    }

    #[test]
    fn the_http_slot_names_the_transport() {
        assert_eq!(
            http_server(1, " https://h/mcp ", json!({}))["transport"],
            "http"
        );
        assert_eq!(
            http_server(2, "https://h/sse", json!({}))["transport"],
            "sse"
        );
        assert_eq!(
            http_server(1, " https://h/mcp ", json!({}))["url"],
            "https://h/mcp"
        );
    }

    #[test]
    fn screen_paints_the_hosts_lines() {
        let result = json!({ "lines": [
            { "tone": "plain", "text": "  SERVER  TRANSPORT" },
            { "tone": "info", "text": "Manage global servers with openbutler mcp." }
        ]});
        // The palette is the ui module's: a plain line verbatim, an info line
        // prefixed with the source's middle dot.
        assert_eq!(
            screen(&result),
            format!(
                "  SERVER  TRANSPORT\n{}Manage global servers with openbutler mcp.",
                crate::ui::info("")
            )
        );
    }
}
