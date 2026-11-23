//! The first-run wizard (`openbutler setup`): prompts collect, the
//! daemon writes.
//!
//! Source: `cli/setup.ts` (read 2026-11-23). The prompt sentences, the
//! section/kv/bullet render shapes (`cli/ui.ts`), the `parseList` split
//! and the `summarizeProvider` line are ports; the structure is adapted
//! where the backend-owned rule and this build's missing catalog force
//! it. Documented divergences:
//!
//! - The source wrote `settings.yaml` from the CLI (a client write the
//!   backend-owned rule forbids): this wizard ends in one
//!   `settings.setup` call, so a running daemon is required.
//! - No catalog exists in this build, so the provider-source select is
//!   gone and the source's own fallback sentence stands alone:
//!   custom endpoints only. No `npm` prompt either — the sidecar
//!   consumes `url` and `api_key` alone.
//! - Keys are collected fresh each run (the CLI never reads credential
//!   files, so it cannot seed existing keys); the daemon merge keeps
//!   providers this run did not touch.
//! - No local-memory step and no readiness report (both need the
//!   catalog); the closing hint says `run` where the source said `--ask`.
//! - Menus are plain numbered lists instead of the inquirer arrows;
//!   cancel is Ctrl+C/EOF (silent) or the explicit discard choice.

use std::io::{IsTerminal, Write};
use std::path::Path;

use openbutler_protocol::{DaemonRequest, DaemonResponse};
use serde_json::{Value, json};

use crate::CliError;
use crate::client::Client;

/// The refusal when stdin is not a terminal (the source's guard).
pub const TTY_REFUSAL: &str = "setup needs an interactive terminal. Run it from a real terminal, or edit settings.yaml directly.";

/// Runs the wizard on the real terminal and saves through the daemon.
pub async fn run(data_dir: &Path) -> Result<(), CliError> {
    require_terminal(std::io::stdin().is_terminal())?;
    let mut term = StdTerm;
    match walk(&mut term, data_dir) {
        Outcome::Discard => Ok(()),
        Outcome::Save(payload) => {
            let settings_file = save(data_dir, payload).await?;
            term.write(&format!("\u{2714} Saved to {settings_file}."));
            term.write("");
            term.write("\u{b7} Try: openbutler run \"is docker installed?\"");
            Ok(())
        }
    }
}

/// The one guard: an interactive wizard needs a terminal (the source's
/// inquirer prompts cannot run anywhere else).
fn require_terminal(is_tty: bool) -> Result<(), CliError> {
    if is_tty {
        Ok(())
    } else {
        Err(CliError::Answer(TTY_REFUSAL.to_owned()))
    }
}

/// Sends the collected answers as one `settings.setup` call and returns
/// the settings file path the daemon reports.
pub async fn save(data_dir: &Path, payload: Value) -> Result<String, CliError> {
    let mut client = Client::connect(data_dir).await?;
    let mut request = DaemonRequest::new(uuid::Uuid::new_v4().to_string(), "settings.setup");
    if let Value::Object(fields) = payload {
        for (key, value) in fields {
            request.payload.insert(key, value);
        }
    }
    match client.call(&request).await? {
        DaemonResponse::Success { result, .. } => result
            .get("settingsFile")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| {
                CliError::Answer(
                    "the daemon answered settings.setup without a settingsFile".to_owned(),
                )
            }),
        DaemonResponse::Failure { error, .. } => Err(CliError::Answer(error)),
    }
}

/// What the walk decided: save these answers, or write nothing.
enum Outcome {
    Save(Value),
    Discard,
}

/// One provider the wizard collected this run.
struct Collected {
    id: String,
    url: String,
    keys: Vec<String>,
}

/// Walks the wizard against any terminal implementation. `None` inside
/// means EOF (Ctrl+D/pipe closed): cancel silently, the source's
/// Ctrl+C behaviour — nothing was written, nothing is announced.
fn walk<T: Term>(term: &mut T, data_dir: &Path) -> Outcome {
    walk_inner(term, data_dir).unwrap_or(Outcome::Discard)
}

fn walk_inner<T: Term>(term: &mut T, data_dir: &Path) -> Option<Outcome> {
    term.write("");
    term.write(&section_line("Setup"));
    term.write(&kv_line("Data directory", &data_dir.display().to_string()));
    term.write(&kv_line(
        "Settings file",
        &data_dir.join("settings.yaml").display().to_string(),
    ));
    term.write("\u{b7} Settings are written at the last step. Ctrl+C cancels before saving.");

    let mut collected: Vec<Collected> = Vec::new();
    let mut default_pair: Option<(String, String)> = None;
    let mut first_pass = true;

    loop {
        term.write("");
        term.write(&section_line("LLM providers"));
        // This build has no catalog; the source prints exactly this when
        // the index cannot load (setup.ts L322), and it is permanent here.
        term.write("! Catalog unavailable; only custom endpoints can be configured.");

        // The first pass must configure a provider (the source's machine
        // cannot leave the section otherwise); a later pass — `Back to
        // LLM providers` — only adds when asked.
        if first_pass || confirm(term, "Add another LLM provider?")? {
            collect_until_declined(term, &mut collected)?;
            first_pass = false;
        }

        if !collected.is_empty() {
            let ids: Vec<&str> = collected.iter().map(|entry| entry.id.as_str()).collect();
            let chosen = select(term, "Which provider should openbutler use?", &ids)?;
            let provider = ids[chosen].to_owned();
            term.write(&format!(
                "! No model in the catalog for {provider}; type the model id."
            ));
            let raw = input(term, &format!("Model id for {provider}"))?;
            // The source's catalog-empty branch: the first list item, and
            // an empty answer sets nothing (the pair never half-moves).
            if let Some(model) = parse_list(&raw).into_iter().next() {
                term.write(&format!("\u{2714} Default: {provider} / {model}"));
                default_pair = Some((provider, model));
            }
        }

        term.write("");
        term.write(&section_line("Review"));
        if collected.is_empty() {
            term.write("  \u{b7} no provider configured");
        } else {
            let default_id = default_pair.as_ref().map(|(id, _)| id.as_str());
            for entry in &collected {
                let is_default = default_id == Some(entry.id.as_str());
                term.write(&format!(
                    "  \u{b7} {}",
                    summarize_provider(entry, is_default)
                ));
            }
        }
        term.write("");
        let default_line = match &default_pair {
            Some((provider, model)) => format!("{provider} / {model}"),
            None => "(none)".to_owned(),
        };
        term.write(&kv_line("Default model", &default_line));

        let decision = select(
            term,
            "What next?",
            &[
                "Save these settings",
                "\u{2190} Back to LLM providers",
                "Discard and exit without saving",
            ],
        )?;
        match decision {
            0 => {
                let payload = build_payload(&collected, default_pair.as_ref());
                return Some(Outcome::Save(payload));
            }
            1 => continue,
            _ => {
                term.write("! Nothing was written.");
                return Some(Outcome::Discard);
            }
        }
    }
}

/// Collects providers until the user declines another (the source's
/// inner loop, setup.ts L343).
fn collect_until_declined<T: Term>(term: &mut T, collected: &mut Vec<Collected>) -> Option<()> {
    loop {
        let id = ask_provider_id(term)?;
        let url = ask_url(term)?;
        let keys = ask_keys(term, &id)?;
        collected.push(Collected { id, url, keys });
        if !confirm(term, "Add another LLM provider?")? {
            return Some(());
        }
    }
}

/// The custom-id gate (`/^[a-z0-9][a-z0-9-]*$/i`, ASCII classes — JS
/// without the unicode flag treats non-ASCII letters as no-match).
fn ask_provider_id<T: Term>(term: &mut T) -> Option<String> {
    loop {
        let value = input(term, "Custom provider id (letters, digits, dashes)")?;
        let trimmed = value.trim();
        if is_provider_id(trimmed) {
            return Some(trimmed.to_owned());
        }
        term.write("! Use letters, digits, or dashes.");
    }
}

fn is_provider_id(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_alphanumeric() {
        return false;
    }
    chars.all(|char| char.is_ascii_alphanumeric() || char == '-')
}

/// Custom endpoints always need their URL (setup.ts L200: the source's
/// validate message, byte for byte).
fn ask_url<T: Term>(term: &mut T) -> Option<String> {
    loop {
        let value = input(term, "Endpoint base URL (required)")?;
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_owned());
        }
        term.write("! A custom endpoint needs its URL.");
    }
}

/// Collects the complete key list for one provider (setup.ts L81): the
/// prompts name the provider, an empty first answer refuses, and the
/// stored line shows only the last four characters.
fn ask_keys<T: Term>(term: &mut T, owner: &str) -> Option<Vec<String>> {
    let mut keys: Vec<String> = Vec::new();
    loop {
        let prompt = if keys.is_empty() {
            format!("Paste the API key for {owner}")
        } else {
            format!("Paste another key for {owner} (empty to finish)")
        };
        let key = term.read_password(&format!("{prompt}: "))?;
        let key = key.trim();
        if key.is_empty() {
            if !keys.is_empty() {
                return Some(keys);
            }
            term.write(&format!("! {owner} needs at least one API key."));
            continue;
        }
        keys.push(key.to_owned());
        term.write(&format!(
            "\u{2714} {owner}: key {} stored (shown later as \u{2026}{}).",
            keys.len(),
            last_four(key)
        ));
        if !confirm(term, &format!("{owner}: add another key?"))? {
            return Some(keys);
        }
    }
}

/// A yes/no line; the source's `confirm` default is false.
fn confirm<T: Term>(term: &mut T, message: &str) -> Option<bool> {
    let answer = term.read_line(&format!("{message} [y/N]: "))?;
    let answer = answer.trim();
    Some(answer.eq_ignore_ascii_case("y") || answer.eq_ignore_ascii_case("yes"))
}

/// A numbered menu (the source's `select`, rendered plain).
fn select<T: Term>(term: &mut T, message: &str, choices: &[&str]) -> Option<usize> {
    loop {
        term.write(message);
        for (index, choice) in choices.iter().enumerate() {
            term.write(&format!("{}) {choice}", index + 1));
        }
        let answer = term.read_line("Choice: ")?;
        if let Ok(picked) = answer.trim().parse::<usize>()
            && (1..=choices.len()).contains(&picked)
        {
            return Some(picked - 1);
        }
    }
}

/// One prompt line, trimmed of its newline.
fn input<T: Term>(term: &mut T, message: &str) -> Option<String> {
    term.read_line(&format!("{message}: "))
}

/// `parseList` (setup.ts L17): split on whitespace, commas and
/// semicolons, trim, drop empties.
fn parse_list(raw: &str) -> Vec<String> {
    raw.split(|char: char| char.is_whitespace() || char == ',' || char == ';')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(str::to_owned)
        .collect()
}

/// The source's `summarizeProvider` line (setup.ts L26): `enabled` is
/// always true in this flow (never collected) and `npm` is never
/// collected, so those variants are unreachable here.
fn summarize_provider(entry: &Collected, is_default: bool) -> String {
    let keys = if entry.keys.len() == 1 {
        "1 key".to_owned()
    } else {
        format!("{} keys", entry.keys.len())
    };
    let marker = if is_default { " *" } else { "" };
    format!("on {}{} \u{2014} {keys}", entry.id, marker)
}

/// The last four characters (the source's `slice(-4)`; API keys are
/// ASCII in practice, so character and UTF-16 slices agree).
fn last_four(key: &str) -> String {
    key.chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

/// `ui.section` (ui.ts L11): `── Title ─────`, a uniform 57-column line
/// while the title fits (`max(3, 56 - len - 3)`).
fn section_line(title: &str) -> String {
    let fill = (56usize.saturating_sub(title.len() + 3)).max(3);
    format!("\u{2500}\u{2500} {title} {}", "\u{2500}".repeat(fill))
}

/// `ui.kv` (ui.ts L31): two spaces, key padded to 22, one space, value.
fn kv_line(key: &str, value: &str) -> String {
    format!("  {key:<22} {value}")
}

/// The save payload: one entry per provider collected this run and the
/// default pair together or not at all (the source's `setDefaultModel`).
fn build_payload(collected: &[Collected], default_pair: Option<&(String, String)>) -> Value {
    let providers: Vec<Value> = collected
        .iter()
        .map(|entry| {
            json!({
                "id": entry.id.as_str(),
                "keys": entry.keys.clone(),
                "url": entry.url.as_str(),
            })
        })
        .collect();
    let mut payload = json!({ "providers": providers });
    if let Some((provider, model)) = default_pair {
        payload["defaultProvider"] = json!(provider);
        payload["defaultModel"] = json!(model);
    }
    payload
}

/// The wizard's terminal: writes go straight to stdout, prompts are
/// flushed before the read.
struct StdTerm;

impl Term for StdTerm {
    fn write(&mut self, text: &str) {
        println!("{text}");
    }

    fn read_line(&mut self, prompt: &str) -> Option<String> {
        print!("{prompt}");
        let _ = std::io::stdout().flush();
        let mut buffer = String::new();
        match std::io::stdin().read_line(&mut buffer) {
            Ok(0) | Err(_) => None,
            Ok(_) => Some(buffer.trim_end_matches(['\r', '\n']).to_owned()),
        }
    }

    fn read_password(&mut self, prompt: &str) -> Option<String> {
        rpassword::prompt_password(prompt).ok()
    }
}

/// What the wizard needs from a terminal. The real one is `StdTerm`;
/// tests script the answers and capture the transcript.
trait Term {
    fn write(&mut self, text: &str);
    fn read_line(&mut self, prompt: &str) -> Option<String>;
    fn read_password(&mut self, prompt: &str) -> Option<String>;
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::collections::VecDeque;

    use super::*;

    /// A scripted terminal: queued answers (None = EOF), captured
    /// transcript. Passwords are never echoed into the transcript — the
    /// real terminal masks them too.
    struct ScriptTerm {
        lines: VecDeque<Option<String>>,
        passwords: VecDeque<Option<String>>,
        out: String,
    }

    impl ScriptTerm {
        fn new(lines: &[Option<&str>], passwords: &[Option<&str>]) -> Self {
            Self {
                lines: lines.iter().map(|line| line.map(str::to_owned)).collect(),
                passwords: passwords
                    .iter()
                    .map(|password| password.map(str::to_owned))
                    .collect(),
                out: String::new(),
            }
        }
    }

    impl Term for ScriptTerm {
        fn write(&mut self, text: &str) {
            self.out.push_str(text);
            self.out.push('\n');
        }

        fn read_line(&mut self, prompt: &str) -> Option<String> {
            self.out.push_str(prompt);
            let answer = self.lines.pop_front().flatten();
            match &answer {
                Some(text) => {
                    self.out.push_str(text);
                    self.out.push('\n');
                }
                None => self.out.push_str("<eof>\n"),
            }
            answer
        }

        fn read_password(&mut self, prompt: &str) -> Option<String> {
            self.out.push_str(prompt);
            self.out.push('\n');
            self.passwords.pop_front().flatten()
        }
    }

    #[test]
    fn wizard_collects_and_builds_the_payload() {
        let mut term = ScriptTerm::new(
            &[
                Some("mock"),
                Some("http://127.0.0.1:9999/v1"),
                Some("n"), // add another key?
                Some("n"), // add another provider?
                Some("1"), // provider select
                Some("mock/model-a"),
                Some("1"), // review: save
            ],
            &[Some("sk-test-1234")],
        );
        let payload = match walk(&mut term, Path::new("/ob/data")) {
            Outcome::Save(payload) => payload,
            Outcome::Discard => panic!("the happy path must save"),
        };
        assert_eq!(
            payload,
            json!({
                "providers": [{
                    "id": "mock",
                    "keys": ["sk-test-1234"],
                    "url": "http://127.0.0.1:9999/v1",
                }],
                "defaultProvider": "mock",
                "defaultModel": "mock/model-a",
            })
        );

        let settings_path = Path::new("/ob/data").join("settings.yaml");
        let out = &term.out;
        for expected in [
            format!("\n\u{2500}\u{2500} Setup {}\n", "\u{2500}".repeat(48)),
            "  Data directory         /ob/data\n".to_owned(),
            // The value is a platform path (`join` picks the separator), so
            // this row pins through the kv rule; the rule's exact rendering
            // has its own literal pin in `render_helpers_match_the_source_shapes`.
            format!(
                "{}\n",
                kv_line(
                    "Settings file",
                    &settings_path.display().to_string()
                )
            ),
            "\u{b7} Settings are written at the last step. Ctrl+C cancels before saving.\n".to_owned(),
            "! Catalog unavailable; only custom endpoints can be configured.\n".to_owned(),
            "Custom provider id (letters, digits, dashes): mock\n".to_owned(),
            "Endpoint base URL (required): http://127.0.0.1:9999/v1\n".to_owned(),
            "Paste the API key for mock: \n".to_owned(),
            "\u{2714} mock: key 1 stored (shown later as \u{2026}1234).\n".to_owned(),
            "mock: add another key? [y/N]: n\n".to_owned(),
            "Add another LLM provider? [y/N]: n\n".to_owned(),
            "Which provider should openbutler use?\n1) mock\nChoice: 1\n".to_owned(),
            "! No model in the catalog for mock; type the model id.\n".to_owned(),
            "Model id for mock: mock/model-a\n".to_owned(),
            "\u{2714} Default: mock / mock/model-a\n".to_owned(),
            "  \u{b7} on mock * \u{2014} 1 key\n".to_owned(),
            "  Default model          mock / mock/model-a\n".to_owned(),
            "What next?\n1) Save these settings\n2) \u{2190} Back to LLM providers\n3) Discard and exit without saving\nChoice: 1\n".to_owned(),
        ] {
            assert!(out.contains(&expected), "transcript is missing {expected:?}\n---\n{out}");
        }
    }

    #[test]
    fn invalid_answers_reprompt_with_the_source_sentences() {
        let mut term = ScriptTerm::new(
            &[
                Some("bad id!"), // rejected → re-ask
                Some("ok-id"),
                Some("   "), // empty url → re-ask
                Some("http://h"),
                Some("n"),  // add another key?
                Some("n"),  // add another provider?
                Some("1"),  // provider select
                Some("  "), // blank model → nothing set
                Some("1"),  // review: save
            ],
            &[Some(""), Some("abcdef")],
        );
        let payload = match walk(&mut term, Path::new("/ob/data")) {
            Outcome::Save(payload) => payload,
            Outcome::Discard => panic!("validation recovers into a save"),
        };
        assert_eq!(
            payload,
            json!({ "providers": [{
                "id": "ok-id",
                "keys": ["abcdef"],
                "url": "http://h",
            }] }),
            "a blank model sets neither default (the pair never half-moves)"
        );

        let out = &term.out;
        for expected in [
            "Custom provider id (letters, digits, dashes): bad id!\n",
            "! Use letters, digits, or dashes.\n",
            "Endpoint base URL (required):    \n",
            "! A custom endpoint needs its URL.\n",
            "Paste the API key for ok-id: \n",
            "! ok-id needs at least one API key.\n",
            "Paste the API key for ok-id: \n",
            "\u{2714} ok-id: key 1 stored (shown later as \u{2026}cdef).\n",
            "  Default model          (none)\n",
        ] {
            assert!(
                out.contains(expected),
                "transcript is missing {expected:?}\n---\n{out}"
            );
        }
    }

    #[test]
    fn discard_announces_and_eof_cancels_silently() {
        let mut discard = ScriptTerm::new(
            &[
                Some("mock"),
                Some("http://h"),
                Some("k"),
                Some("n"),
                Some("n"),
                Some("1"),
                Some("m"),
                Some("3"), // review: discard
            ],
            &[Some("k")],
        );
        assert!(matches!(
            walk(&mut discard, Path::new("/ob/data")),
            Outcome::Discard
        ));
        assert!(
            discard.out.contains("! Nothing was written.\n"),
            "the explicit discard announces itself\n---\n{}",
            discard.out
        );

        let mut eof = ScriptTerm::new(&[], &[]);
        assert!(matches!(
            walk(&mut eof, Path::new("/ob/data")),
            Outcome::Discard
        ));
        assert!(
            !eof.out.contains("Nothing was written") && !eof.out.contains("Saved"),
            "EOF cancels like Ctrl+C: silent\n---\n{}",
            eof.out
        );
    }

    #[test]
    fn render_helpers_match_the_source_shapes() {
        assert_eq!(
            section_line("Setup"),
            format!("\u{2500}\u{2500} Setup {}", "\u{2500}".repeat(48))
        );
        assert_eq!(section_line("LLM providers").chars().count(), 57);
        assert_eq!(
            kv_line("Default model", "p / m"),
            "  Default model          p / m"
        );

        let one = Collected {
            id: "mock".to_owned(),
            url: "http://h".to_owned(),
            keys: vec!["k".to_owned()],
        };
        assert_eq!(summarize_provider(&one, true), "on mock * \u{2014} 1 key");
        let three = Collected {
            keys: vec!["a".to_owned(), "b".to_owned(), "c".to_owned()],
            ..one
        };
        assert_eq!(summarize_provider(&three, false), "on mock \u{2014} 3 keys");

        assert_eq!(
            parse_list(" alpha,beta ;gamma\ndelta "),
            vec!["alpha", "beta", "gamma", "delta"]
        );
        assert!(parse_list("  ,; ").is_empty());
    }

    #[test]
    fn the_tty_guard_refuses_exactly_when_stdin_is_not_a_terminal() {
        assert!(require_terminal(true).is_ok());
        let error = require_terminal(false).unwrap_err();
        assert_eq!(error.to_string(), TTY_REFUSAL);
    }
}
