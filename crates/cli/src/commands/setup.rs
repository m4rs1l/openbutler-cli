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

use openbutler_protocol::{DaemonRequest, DaemonResponse, ScreenLine};
use serde_json::{Value, json};

use crate::CliError;
use crate::client::Client;

/// The refusal when stdin is not a terminal (the source's guard).
pub const TTY_REFUSAL: &str = "setup needs an interactive terminal. Run it from a real terminal, or edit settings.yaml directly.";

/// Runs the wizard on the real terminal and saves through the daemon.
pub async fn run(data_dir: &Path) -> Result<(), CliError> {
    require_terminal(std::io::stdin().is_terminal())?;
    let mut term = StdTerm;
    let reviewer = DaemonReviewer {
        data_dir: data_dir.to_path_buf(),
    };
    match walk(&mut term, data_dir, &reviewer).await? {
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

/// One provider row from the host's catalog.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProviderRow {
    /// Catalog id, the value saved as the provider id.
    pub id: String,
    /// Display name for the picker.
    pub name: String,
    /// How many models the catalog lists for it.
    pub models: u64,
    /// The package that speaks it, when the catalog names one.
    pub npm: Option<String>,
}

/// One model row from the host's catalog.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ModelRow {
    /// The model id the catalog keys it under.
    pub id: String,
    /// Display name for the picker.
    pub name: String,
}

/// What the wizard needs from the host: the Review screen's finished
/// lines, the host's own paths, and its catalog. The layout is the host's
/// decision; the client only applies its glyph palette and draws menus.
pub trait Reviewer {
    fn review(
        &self,
        payload: &Value,
    ) -> impl std::future::Future<Output = Result<Vec<ScreenLine>, CliError>>;

    /// The host's paths for the header: data directory and settings file.
    /// A reviewer without a host answers empty and the wizard falls back to
    /// the local path it was given.
    fn paths(&self) -> impl std::future::Future<Output = Result<(String, String), CliError>> {
        async { Ok((String::new(), String::new())) }
    }

    /// The host's catalog providers. Empty means the host has none yet; the
    /// wizard then offers custom endpoints only.
    fn providers(&self) -> impl std::future::Future<Output = Result<Vec<ProviderRow>, CliError>> {
        async { Ok(Vec::new()) }
    }

    /// One provider's models; empty means the id must be typed.
    fn models(
        &self,
        provider: &str,
    ) -> impl std::future::Future<Output = Result<Vec<ModelRow>, CliError>> {
        async move {
            let _ = provider;
            Ok(Vec::new())
        }
    }
}

/// The production reviewer: one `settings.setup.review` call, so the
/// screen can never drift from the set the save would apply.
pub struct DaemonReviewer {
    /// The daemon's data directory, for the endpoint lookup.
    pub data_dir: std::path::PathBuf,
}

impl Reviewer for DaemonReviewer {
    async fn review(&self, payload: &Value) -> Result<Vec<ScreenLine>, CliError> {
        let mut client = Client::connect(&self.data_dir).await?;
        let mut request =
            DaemonRequest::new(uuid::Uuid::new_v4().to_string(), "settings.setup.review");
        if let Value::Object(fields) = payload {
            for (key, value) in fields {
                request.payload.insert(key.clone(), value.clone());
            }
        }
        match client.call(&request).await? {
            DaemonResponse::Success { result, .. } => {
                let lines = result
                    .get("lines")
                    .and_then(Value::as_array)
                    .cloned()
                    .ok_or_else(|| {
                        CliError::Answer(
                            "the daemon answered settings.setup.review without lines".to_owned(),
                        )
                    })?;
                serde_json::from_value(Value::Array(lines)).map_err(|error| {
                    CliError::Answer(format!(
                        "the daemon answered settings.setup.review with unreadable lines: {error}"
                    ))
                })
            }
            DaemonResponse::Failure { error, .. } => Err(CliError::Answer(error)),
        }
    }

    async fn paths(&self) -> Result<(String, String), CliError> {
        let mut client = Client::connect(&self.data_dir).await?;
        let request = DaemonRequest::new(uuid::Uuid::new_v4().to_string(), "about.report");
        match client.call(&request).await? {
            DaemonResponse::Success { result, .. } => {
                let settings = result
                    .get("settingsPath")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let data = std::path::Path::new(&settings)
                    .parent()
                    .map(|parent| parent.display().to_string())
                    .unwrap_or_default();
                Ok((data, settings))
            }
            DaemonResponse::Failure { error, .. } => Err(CliError::Answer(error)),
        }
    }

    async fn providers(&self) -> Result<Vec<ProviderRow>, CliError> {
        let mut client = Client::connect(&self.data_dir).await?;
        let request = DaemonRequest::new(uuid::Uuid::new_v4().to_string(), "catalog.listProviders");
        let result = match client.call(&request).await? {
            DaemonResponse::Success { result, .. } => result,
            DaemonResponse::Failure { error, .. } => return Err(CliError::Answer(error)),
        };
        let mut rows = Vec::new();
        for row in result
            .get("providers")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            rows.push(ProviderRow {
                id: row
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                name: row
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                models: row.get("models").and_then(Value::as_u64).unwrap_or(0),
                npm: row.get("npm").and_then(Value::as_str).map(str::to_owned),
            });
        }
        Ok(rows)
    }

    async fn models(&self, provider: &str) -> Result<Vec<ModelRow>, CliError> {
        let mut client = Client::connect(&self.data_dir).await?;
        let mut request =
            DaemonRequest::new(uuid::Uuid::new_v4().to_string(), "catalog.listModels");
        request
            .payload
            .insert("provider".to_owned(), json!(provider));
        let result = match client.call(&request).await? {
            DaemonResponse::Success { result, .. } => result,
            DaemonResponse::Failure { error, .. } => return Err(CliError::Answer(error)),
        };
        let mut rows = Vec::new();
        for row in result
            .get("models")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            rows.push(ModelRow {
                id: row
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                name: row
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            });
        }
        Ok(rows)
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
    npm: Option<String>,
    keys: Vec<String>,
}

/// Walks the wizard against any terminal implementation. `None` inside
/// means EOF (Ctrl+D/pipe closed): cancel silently, the source's
/// Ctrl+C behaviour — nothing was written, nothing is announced.
async fn walk<T: Term, R: Reviewer>(
    term: &mut T,
    data_dir: &Path,
    reviewer: &R,
) -> Result<Outcome, CliError> {
    Ok(walk_inner(term, data_dir, reviewer)
        .await?
        .unwrap_or(Outcome::Discard))
}

async fn walk_inner<T: Term, R: Reviewer>(
    term: &mut T,
    data_dir: &Path,
    reviewer: &R,
) -> Result<Option<Outcome>, CliError> {
    // The header names the host's paths, not the client's: a remote
    // session writes on the host, and a local path here would lie.
    let (host_data, host_settings) = reviewer.paths().await.unwrap_or_default();
    let data_label = if host_data.is_empty() {
        data_dir.display().to_string()
    } else {
        host_data
    };
    let settings_label = if host_settings.is_empty() {
        data_dir.join("settings.yaml").display().to_string()
    } else {
        host_settings
    };
    term.write("");
    term.write(&crate::ui::section("Setup"));
    term.write(&crate::ui::kv("Data directory", &data_label));
    term.write(&crate::ui::kv("Settings file", &settings_label));
    term.write("\u{b7} Settings are written at the last step. Ctrl+C cancels before saving.");

    let mut collected: Vec<Collected> = Vec::new();
    let mut default_pair: Option<(String, String)> = None;
    let mut first_pass = true;

    loop {
        term.write("");
        term.write(&crate::ui::section("LLM providers"));
        // The host's catalog decides: a list means pick one, no list means
        // custom endpoints only, with the source's sentence for that case.
        let catalog = reviewer.providers().await.unwrap_or_default();
        if catalog.is_empty() {
            term.write("! Catalog unavailable; only custom endpoints can be configured.");
        }

        // The first pass must configure a provider (the source's machine
        // cannot leave the section otherwise); a later pass — `Back to
        // LLM providers` — only adds when asked.
        let add = if first_pass {
            true
        } else {
            let Some(answer) = confirm(term, "Add another LLM provider?") else {
                return Ok(None);
            };
            answer
        };
        if add {
            let Some(()) = collect_until_declined(term, &mut collected, &catalog) else {
                return Ok(None);
            };
            first_pass = false;
        }

        if !collected.is_empty() {
            let ids: Vec<&str> = collected.iter().map(|entry| entry.id.as_str()).collect();
            let Some(chosen) = select(term, "Which provider should openbutler use?", &ids) else {
                return Ok(None);
            };
            let provider = ids[chosen].to_owned();
            let models = reviewer.models(&provider).await.unwrap_or_default();
            // A picker stops being a picker past a hundred rows; the id is
            // typed then, and an empty answer sets nothing (the pair never
            // half-moves).
            let chosen_model = if models.is_empty() || models.len() > 100 {
                if models.is_empty() {
                    term.write(&format!(
                        "! No model in the catalog for {provider}; type the model id."
                    ));
                } else {
                    term.write(&format!(
                        "! {provider} lists {} models; type the model id.",
                        models.len()
                    ));
                }
                input(term, &format!("Model id for {provider}"))
                    .and_then(|raw| parse_list(&raw).into_iter().next())
            } else {
                let labels: Vec<String> = models
                    .iter()
                    .map(|row| format!("{} ({})", row.name, row.id))
                    .collect();
                let choices: Vec<&str> = labels.iter().map(String::as_str).collect();
                select(term, "Which model should openbutler use?", &choices)
                    .map(|index| models[index].id.clone())
            };
            if let Some(model) = chosen_model {
                term.write(&format!("\u{2714} Default: {provider} / {model}"));
                default_pair = Some((provider, model));
            }
        }

        // The Review block is the host's screen: the wizard asks which set
        // the choices describe and prints the answered lines verbatim.
        let review = reviewer
            .review(&build_payload(&collected, default_pair.as_ref()))
            .await?;
        for line in &review {
            term.write(&crate::ui::toned(line.tone, &line.text));
        }

        let Some(decision) = select(
            term,
            "What next?",
            &[
                "Save these settings",
                "\u{2190} Back to LLM providers",
                "Discard and exit without saving",
            ],
        ) else {
            return Ok(None);
        };
        match decision {
            0 => {
                let payload = build_payload(&collected, default_pair.as_ref());
                return Ok(Some(Outcome::Save(payload)));
            }
            1 => continue,
            _ => {
                term.write("! Nothing was written.");
                return Ok(Some(Outcome::Discard));
            }
        }
    }
}

/// Collects providers until the user declines another (the source's
/// inner loop, setup.ts L343).
fn collect_until_declined<T: Term>(
    term: &mut T,
    collected: &mut Vec<Collected>,
    catalog: &[ProviderRow],
) -> Option<()> {
    loop {
        let (id, url, npm) = if catalog.is_empty() {
            let id = ask_provider_id(term)?;
            let url = ask_url(term)?;
            (id, url, None)
        } else {
            let labels: Vec<String> = catalog
                .iter()
                .map(|row| format!("{} ({}, {} models)", row.name, row.id, row.models))
                .collect();
            let mut choices: Vec<&str> = labels.iter().map(String::as_str).collect();
            choices.push("Custom endpoint");
            let chosen = select(term, "Which provider do you want to configure?", &choices)?;
            if chosen == catalog.len() {
                let id = ask_provider_id(term)?;
                let url = ask_url(term)?;
                (id, url, None)
            } else {
                let row = &catalog[chosen];
                // A catalog provider ships its package, so the save needs no
                // URL: the sidecar resolves it from npm.
                (row.id.clone(), String::new(), row.npm.clone())
            }
        };
        let keys = ask_keys(term, &id)?;
        collected.push(Collected { id, url, npm, keys });
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
pub fn ask_keys<T: Term + ?Sized>(term: &mut T, owner: &str) -> Option<Vec<String>> {
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
pub fn confirm<T: Term + ?Sized>(term: &mut T, message: &str) -> Option<bool> {
    let answer = term.read_line(&format!("{message} [y/N]: "))?;
    let answer = answer.trim();
    Some(answer.eq_ignore_ascii_case("y") || answer.eq_ignore_ascii_case("yes"))
}

/// A numbered menu (the source's `select`, rendered plain).
pub(crate) fn select<T: Term>(term: &mut T, message: &str, choices: &[&str]) -> Option<usize> {
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
pub(crate) fn input<T: Term>(term: &mut T, message: &str) -> Option<String> {
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

/// The save payload: one entry per provider collected this run and the
/// default pair together or not at all (the source's `setDefaultModel`).
fn build_payload(collected: &[Collected], default_pair: Option<&(String, String)>) -> Value {
    let providers: Vec<Value> = collected
        .iter()
        .map(|entry| {
            let mut row = json!({
                "id": entry.id.as_str(),
                "keys": entry.keys.clone(),
            });
            // An empty URL is absent, not an empty string: the host trims it
            // to nothing anyway, and a catalog provider has none.
            if !entry.url.is_empty() {
                row["url"] = json!(entry.url.as_str());
            }
            if let Some(npm) = &entry.npm {
                row["npm"] = json!(npm);
            }
            row
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
pub struct StdTerm;

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
pub trait Term {
    fn write(&mut self, text: &str);
    fn read_line(&mut self, prompt: &str) -> Option<String>;
    fn read_password(&mut self, prompt: &str) -> Option<String>;
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::cell::RefCell;
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

    /// The host side of the Review block: canned lines (the daemon's own
    /// pins cover the layout) and the payloads the wizard asked about.
    struct FakeReviewer {
        lines: Vec<ScreenLine>,
        seen: RefCell<Vec<Value>>,
    }

    impl FakeReviewer {
        fn new(lines: &[&str]) -> Self {
            Self {
                lines: lines.iter().map(|line| ScreenLine::plain(*line)).collect(),
                seen: RefCell::new(Vec::new()),
            }
        }
    }

    impl Reviewer for FakeReviewer {
        async fn review(&self, payload: &Value) -> Result<Vec<ScreenLine>, CliError> {
            self.seen.borrow_mut().push(payload.clone());
            Ok(self.lines.clone())
        }
    }

    #[tokio::test]
    async fn wizard_collects_builds_the_payload_and_prints_the_hosts_review() {
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
        let reviewer = FakeReviewer::new(&[
            "  \u{b7} on mock * \u{2014} 1 key",
            "  Default model          mock / mock/model-a",
        ]);
        let payload = match walk(&mut term, Path::new("/ob/data"), &reviewer)
            .await
            .unwrap()
        {
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
        // The review question carries exactly the set the save would
        // apply, so the host can never describe a different one.
        assert_eq!(
            reviewer.seen.borrow().as_slice(),
            std::slice::from_ref(&payload)
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
                crate::ui::kv("Settings file", &settings_path.display().to_string())
            ),
            "\u{b7} Settings are written at the last step. Ctrl+C cancels before saving.\n"
                .to_owned(),
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
            // Printed verbatim from the host's lines.
            "  \u{b7} on mock * \u{2014} 1 key\n".to_owned(),
            "  Default model          mock / mock/model-a\n".to_owned(),
            "What next?\n1) Save these settings\n2) \u{2190} Back to LLM providers\n3) Discard and exit without saving\nChoice: 1\n"
                .to_owned(),
        ] {
            assert!(
                out.contains(&expected),
                "transcript is missing {expected:?}\n---\n{out}"
            );
        }
    }

    #[tokio::test]
    async fn invalid_answers_reprompt_with_the_source_sentences() {
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
        let reviewer = FakeReviewer::new(&[
            "  \u{b7} on ok-id \u{2014} 1 key",
            "  Default model          (none)",
        ]);
        let payload = match walk(&mut term, Path::new("/ob/data"), &reviewer)
            .await
            .unwrap()
        {
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

    #[tokio::test]
    async fn discard_announces_and_eof_cancels_silently() {
        let reviewer = FakeReviewer::new(&[
            "  \u{b7} on mock * \u{2014} 1 key",
            "  Default model          mock / m",
        ]);
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
            walk(&mut discard, Path::new("/ob/data"), &reviewer)
                .await
                .unwrap(),
            Outcome::Discard
        ));
        assert!(
            discard.out.contains("! Nothing was written.\n"),
            "the explicit discard announces itself\n---\n{}",
            discard.out
        );

        let mut eof = ScriptTerm::new(&[], &[]);
        assert!(matches!(
            walk(&mut eof, Path::new("/ob/data"), &reviewer)
                .await
                .unwrap(),
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
            crate::ui::section("Setup"),
            format!("\u{2500}\u{2500} Setup {}", "\u{2500}".repeat(48))
        );
        assert_eq!(crate::ui::section("LLM providers").chars().count(), 57);
        assert_eq!(
            crate::ui::kv("Default model", "p / m"),
            "  Default model          p / m"
        );
        assert_eq!(
            parse_list(" alpha,beta ;gamma\ndelta "),
            vec!["alpha", "beta", "gamma", "delta"]
        );
        assert!(parse_list("  ,; ").is_empty());
    }

    /// A host with a catalog: the wizard must offer its providers and
    /// models instead of asking for ids, and save the package, not a URL.
    struct CatalogReviewer {
        providers: Vec<ProviderRow>,
        models: Vec<ModelRow>,
        seen: RefCell<Vec<Value>>,
    }

    impl Reviewer for CatalogReviewer {
        async fn review(&self, payload: &Value) -> Result<Vec<ScreenLine>, CliError> {
            self.seen.borrow_mut().push(payload.clone());
            Ok(Vec::new())
        }

        fn providers(
            &self,
        ) -> impl std::future::Future<Output = Result<Vec<ProviderRow>, CliError>> {
            let rows = self.providers.clone();
            async move { Ok(rows) }
        }

        fn models(
            &self,
            provider: &str,
        ) -> impl std::future::Future<Output = Result<Vec<ModelRow>, CliError>> {
            let rows = if provider == "mock" {
                self.models.clone()
            } else {
                Vec::new()
            };
            async move { Ok(rows) }
        }
    }

    #[tokio::test]
    async fn a_catalog_host_offers_providers_and_models_and_saves_the_package() {
        let mut term = ScriptTerm::new(
            &[
                Some("1"), // provider select: Mock Provider
                Some("n"), // add another key?
                Some("n"), // add another provider?
                Some("1"), // default provider select: mock
                Some("1"), // model select: Mock Large
                Some("1"), // review: save
            ],
            &[Some("sk-catalog-1234")],
        );
        let reviewer = CatalogReviewer {
            providers: vec![ProviderRow {
                id: "mock".to_owned(),
                name: "Mock Provider".to_owned(),
                models: 2,
                npm: Some("@ai-sdk/mock".to_owned()),
            }],
            models: vec![
                ModelRow {
                    id: "mock-large".to_owned(),
                    name: "Mock Large".to_owned(),
                },
                ModelRow {
                    id: "mock-small".to_owned(),
                    name: "Mock Small".to_owned(),
                },
            ],
            seen: RefCell::new(Vec::new()),
        };
        let outcome = walk(&mut term, Path::new("/data"), &reviewer)
            .await
            .expect("the walk returns");
        let Outcome::Save(payload) = outcome else {
            panic!("the scripted answers save");
        };
        assert!(
            term.out.contains("Mock Provider (mock, 2 models)"),
            "{}",
            term.out
        );
        assert!(term.out.contains("Mock Large (mock-large)"), "{}", term.out);
        assert!(
            !term.out.contains("Catalog unavailable"),
            "a catalog host must not claim it has none: {}",
            term.out
        );
        let providers = payload["providers"].as_array().expect("providers");
        assert_eq!(providers[0]["id"], "mock");
        assert_eq!(providers[0]["npm"], "@ai-sdk/mock");
        assert!(
            providers[0].get("url").is_none(),
            "a catalog provider saves no URL: {}",
            providers[0]
        );
        assert_eq!(payload["defaultProvider"], "mock");
        assert_eq!(payload["defaultModel"], "mock-large");
    }

    #[test]
    fn the_tty_guard_refuses_exactly_when_stdin_is_not_a_terminal() {
        assert!(require_terminal(true).is_ok());
        let error = require_terminal(false).unwrap_err();
        assert_eq!(error.to_string(), TTY_REFUSAL);
    }
}
