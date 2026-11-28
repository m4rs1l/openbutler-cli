//! The login entry: the one thing that makes the host survive a reboot.
//!
//! One entry per machine, written by the platform's own mechanism - the
//! per-user Run key on Windows, a systemd user unit on Linux - and it starts
//! the *host*, never the client: a login that opened a terminal window would
//! be a bug.
//!
//! Every string the platform receives is built by a pure function here, so the
//! command line, the value name and the unit text are pinned by tests without
//! touching a machine.

use std::path::PathBuf;

use crate::CliError;

/// What the entry runs: the host binary plus the flags it was installed with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub program: PathBuf,
    pub arguments: Vec<String>,
}

impl Entry {
    pub fn new(program: PathBuf, web: bool) -> Self {
        Self {
            program,
            arguments: if web {
                vec!["--web".to_owned()]
            } else {
                Vec::new()
            },
        }
    }

    /// The command line the platform records. The program is quoted because a
    /// path with a space must survive the registry and the unit file alike.
    pub fn command_line(&self) -> String {
        let mut line = format!("\"{}\"", self.program.display());
        for argument in &self.arguments {
            line.push(' ');
            line.push_str(argument);
        }
        line
    }
}

/// Windows: the per-user Run key, which is where a login entry belongs - no
/// administrator, no service, visible in Task Manager's startup list.
#[cfg(windows)]
pub mod windows {
    use super::Entry;

    /// The key every per-user login entry lives under.
    pub const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
    /// The value name this product owns; nothing else may write it.
    pub const VALUE: &str = "OpenButler";

    /// The `reg.exe` arguments that write the entry.
    pub fn write_arguments(entry: &Entry) -> Vec<String> {
        vec![
            "add".to_owned(),
            RUN_KEY.to_owned(),
            "/v".to_owned(),
            VALUE.to_owned(),
            "/t".to_owned(),
            "REG_SZ".to_owned(),
            "/d".to_owned(),
            entry.command_line(),
            "/f".to_owned(),
        ]
    }

    /// The `reg.exe` arguments that remove it.
    pub fn delete_arguments() -> Vec<String> {
        vec![
            "delete".to_owned(),
            RUN_KEY.to_owned(),
            "/v".to_owned(),
            VALUE.to_owned(),
            "/f".to_owned(),
        ]
    }

    /// Writes the entry, but never over one that is not ours: a Run value that
    /// already belongs to another command is refused, unchanged.
    pub fn install(entry: &Entry) -> Result<(), String> {
        let foreign = read().filter(|existing| !belongs_to(existing, &entry.program));
        if let Some(existing) = foreign {
            return Err(format!(
                "The Run value \"{VALUE}\" already belongs to another command ({existing}); OpenButler left it unchanged."
            ));
        }
        run(&write_arguments(entry))
    }

    /// The value that is there now, if any.
    pub fn read() -> Option<String> {
        let output = std::process::Command::new("reg")
            .args(["query", RUN_KEY, "/v", VALUE])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        parse_query(&String::from_utf8_lossy(&output.stdout))
    }

    /// The value line of a `reg query` answer: name, type, data.
    pub fn parse_query(stdout: &str) -> Option<String> {
        stdout
            .lines()
            .find(|line| line.trim_start().starts_with(VALUE))
            .and_then(|line| line.split("REG_SZ").nth(1))
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
    }

    /// Whether an existing value already points at this host binary.
    pub fn belongs_to(value: &str, program: &std::path::Path) -> bool {
        value.contains(&program.display().to_string())
    }

    /// Removes the entry, but only one that is ours. Returns whether a value
    /// was there, so the caller can say what happened.
    pub fn remove(program: &std::path::Path) -> Result<bool, String> {
        match read() {
            None => Ok(false),
            Some(existing) if !belongs_to(&existing, program) => Err(format!(
                "The Run value \"{VALUE}\" belongs to another command ({existing}); OpenButler left it unchanged."
            )),
            Some(_) => match run(&delete_arguments()) {
                // A value that vanished between the read and the delete is
                // already gone: uninstall stays idempotent.
                Err(error) if missing_entry(&error) => Ok(true),
                Err(error) => Err(error),
                Ok(()) => Ok(true),
            },
        }
    }

    /// Whether reg.exe said the value was not there.
    pub fn missing_entry(stderr: &str) -> bool {
        stderr.contains("unable to find")
    }

    fn run(arguments: &[String]) -> Result<(), String> {
        let output = std::process::Command::new("reg")
            .args(arguments)
            .output()
            .map_err(|error| error.to_string())?;
        if output.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
        }
    }
}

/// Linux: a systemd user unit. `Restart=on-failure` because a host that dies
/// at login should come back; the unit starts the host, and the host resolves
/// its own data directory.
#[cfg(unix)]
pub mod unix {
    use std::path::{Path, PathBuf};

    use super::Entry;

    /// The unit name this product owns.
    pub const UNIT: &str = "openbutler.service";

    /// The unit text.
    pub fn unit(entry: &Entry) -> String {
        format!(
            "[Unit]\nDescription=OpenButler host\n\n[Service]\nType=simple\nExecStart={}\nRestart=on-failure\n\n[Install]\nWantedBy=default.target\n",
            entry.command_line()
        )
    }

    /// Where the unit lives under a config directory (`$XDG_CONFIG_HOME` or
    /// `~/.config`).
    pub fn unit_path(config_dir: &Path) -> PathBuf {
        config_dir.join("systemd").join("user").join(UNIT)
    }

    /// Writes the unit file and nothing else: enabling it is a separate step,
    /// so a test can pin the text without a running user session.
    pub fn write_unit(config_dir: &Path, entry: &Entry) -> Result<PathBuf, String> {
        let path = unit_path(config_dir);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        std::fs::write(&path, unit(entry)).map_err(|error| error.to_string())?;
        Ok(path)
    }

    pub fn install(config_dir: &Path, entry: &Entry) -> Result<(), String> {
        write_unit(config_dir, entry)?;
        systemctl(&["daemon-reload"])?;
        systemctl(&["enable", "--now", UNIT])
    }

    pub fn remove(config_dir: &Path) -> Result<bool, String> {
        // A host that is not enabled is not an error: uninstall must be
        // idempotent.
        let _ = systemctl(&["disable", "--now", UNIT]);
        let removed = match std::fs::remove_file(unit_path(config_dir)) {
            Ok(()) => true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => return Err(error.to_string()),
        };
        let _ = systemctl(&["daemon-reload"]);
        Ok(removed)
    }

    fn systemctl(arguments: &[&str]) -> Result<(), String> {
        let output = std::process::Command::new("systemctl")
            .arg("--user")
            .args(arguments)
            .output()
            .map_err(|error| error.to_string())?;
        if output.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
        }
    }
}

/// Registers the login entry and returns the sentence the client prints.
pub fn install(entry: &Entry) -> Result<String, CliError> {
    #[cfg(windows)]
    windows::install(entry).map_err(CliError::Answer)?;
    #[cfg(unix)]
    {
        let config_dir = config_dir()?;
        unix::install(&config_dir, entry).map_err(CliError::Answer)?;
    }
    Ok(installed_sentence())
}

/// What a client reads after a successful install: pure, so the words are
/// pinned without writing a key or a unit file.
pub fn installed_sentence() -> String {
    #[cfg(windows)]
    {
        format!(
            "Login entry written: {} starts the host at every sign-in. Undo it with `openbutler daemon uninstall`.",
            windows::VALUE
        )
    }
    #[cfg(unix)]
    {
        format!(
            "User unit enabled: {} starts the host at every login. Undo it with `openbutler daemon uninstall`.",
            unix::UNIT
        )
    }
}

/// What a client reads after a successful removal.
pub fn removed_sentence() -> String {
    #[cfg(windows)]
    {
        "Login entry removed.".to_owned()
    }
    #[cfg(unix)]
    {
        "User unit removed.".to_owned()
    }
}

/// Whether a login entry this product owns is present.
pub fn installed(program: &std::path::Path) -> bool {
    #[cfg(windows)]
    {
        windows::read()
            .map(|value| windows::belongs_to(&value, program))
            .unwrap_or(false)
    }
    #[cfg(unix)]
    {
        let _ = program;
        config_dir()
            .map(|dir| unix::unit_path(&dir).is_file())
            .unwrap_or(false)
    }
}

/// Removes the login entry, whether or not it was there.
pub fn uninstall(program: &std::path::Path) -> Result<String, CliError> {
    #[cfg(windows)]
    windows::remove(program).map_err(CliError::Answer)?;
    #[cfg(unix)]
    {
        let _ = program;
        let config_dir = config_dir()?;
        unix::remove(&config_dir).map_err(CliError::Answer)?;
    }
    Ok(removed_sentence())
}

/// `$XDG_CONFIG_HOME`, else `~/.config`: the directory systemd reads user
/// units from on both platforms' Unix side.
#[cfg(unix)]
fn config_dir() -> Result<PathBuf, CliError> {
    if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(dir));
    }
    let home = std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            CliError::Answer("HOME is not set; cannot place the user unit.".to_owned())
        })?;
    Ok(PathBuf::from(home).join(".config"))
}

#[cfg(test)]
mod tests {
    use super::Entry;
    use std::path::PathBuf;

    fn entry(web: bool) -> Entry {
        Entry::new(
            PathBuf::from("C:/Program Files/OpenButler/bin/openbutlerd.exe"),
            web,
        )
    }

    #[test]
    fn the_command_line_quotes_the_program_and_keeps_its_flags() {
        assert_eq!(
            entry(false).command_line(),
            "\"C:/Program Files/OpenButler/bin/openbutlerd.exe\""
        );
        assert_eq!(
            entry(true).command_line(),
            "\"C:/Program Files/OpenButler/bin/openbutlerd.exe\" --web"
        );
    }

    #[cfg(windows)]
    #[test]
    fn the_run_key_entry_names_the_value_and_carries_the_command() {
        let arguments = super::windows::write_arguments(&entry(true));
        assert_eq!(arguments[0], "add");
        assert_eq!(arguments[1], super::windows::RUN_KEY);
        assert_eq!(arguments[2], "/v");
        assert_eq!(arguments[3], "OpenButler");
        // The data value is the quoted command line, flags included.
        assert_eq!(arguments[6], "/d");
        assert_eq!(arguments[7], entry(true).command_line());
        // Overwrite in place: installing twice must not fail.
        assert_eq!(arguments.last().map(String::as_str), Some("/f"));
    }

    #[cfg(windows)]
    #[test]
    fn a_run_value_that_is_not_ours_is_never_overwritten() {
        use std::path::Path;
        let foreign = "wscript.exe \"C:\\Users\\me\\openbutler\\autostart\\openbutler-logon.vbs\"";
        assert!(!super::windows::belongs_to(
            foreign,
            Path::new("C:/build/bin/openbutlerd.exe")
        ));
        assert!(super::windows::belongs_to(
            "\"C:\\build\\bin\\openbutlerd.exe\" --web",
            Path::new(r"C:\build\bin\openbutlerd.exe")
        ));
    }

    #[cfg(windows)]
    #[test]
    fn the_query_parser_reads_the_value_line() {
        let stdout = "\r\nHKEY_CURRENT_USER\\Software\\Microsoft\\Windows\\CurrentVersion\\Run\r\n    OpenButler    REG_SZ    \"C:\\bin\\openbutlerd.exe\" --web\r\n\r\n";
        assert_eq!(
            super::windows::parse_query(stdout).as_deref(),
            Some("\"C:\\bin\\openbutlerd.exe\" --web")
        );
        assert_eq!(super::windows::parse_query("no value here"), None);
    }

    #[cfg(windows)]
    #[test]
    fn a_missing_value_is_read_as_already_removed() {
        assert!(super::windows::missing_entry(
            "ERROR: The system was unable to find the specified registry key or value."
        ));
        assert!(!super::windows::missing_entry("ERROR: Access is denied."));
    }

    #[cfg(windows)]
    #[test]
    fn removing_the_entry_deletes_the_one_value() {
        let arguments = super::windows::delete_arguments();
        assert_eq!(arguments[0], "delete");
        assert_eq!(arguments[1], super::windows::RUN_KEY);
        assert_eq!(arguments[3], "OpenButler");
        assert!(!arguments.iter().any(|argument| argument == "/d"));
    }

    #[cfg(unix)]
    #[test]
    fn the_unit_runs_the_host_with_its_flags_and_restarts_on_failure() {
        let text = super::unix::unit(&entry(true));
        assert!(
            text.contains("ExecStart=\"C:/Program Files/OpenButler/bin/openbutlerd.exe\" --web"),
            "{text}"
        );
        assert!(text.contains("Restart=on-failure"), "{text}");
        assert!(text.contains("WantedBy=default.target"), "{text}");
    }

    #[test]
    fn the_install_sentence_says_how_to_undo_it() {
        let sentence = super::installed_sentence();
        assert!(
            sentence.contains("openbutler daemon uninstall"),
            "{sentence}"
        );
    }

    #[test]
    fn the_removal_sentence_is_short_and_final() {
        let sentence = super::removed_sentence();
        assert!(sentence.ends_with('.'), "{sentence}");
        assert!(sentence.len() < 40, "{sentence}");
    }

    #[cfg(unix)]
    #[test]
    fn the_unit_lands_under_the_config_directory() {
        let dir = std::env::temp_dir().join(format!("ob-unit-{}", std::process::id()));
        let path = match super::unix::write_unit(&dir, &entry(false)) {
            Ok(path) => path,
            Err(error) => panic!("unit written: {error}"),
        };
        assert_eq!(
            path,
            dir.join("systemd").join("user").join("openbutler.service")
        );
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) => panic!("read: {error}"),
        };
        assert!(text.contains("ExecStart="), "{text}");
        assert!(!text.contains("--web"), "no flag was asked for: {text}");
        std::fs::remove_dir_all(&dir).ok();
    }
}
