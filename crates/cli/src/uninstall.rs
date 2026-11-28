//! `openbutler uninstall`: remove the program the installer laid down, plus the
//! login entry and the PATH line that came with it.
//!
//! The data directory is the owner's and is kept unless `--purge`; the program
//! directory is only ours when the installer's marker sits at its root, so a
//! build tree or a hand-copied binary is never deleted. Nothing here runs
//! without the owner asking: `--dry-run` reports and stops, and a terminal is
//! required unless `--force` says the caller already decided.

use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use crate::{CliError, ui};

/// The marker both installers write at the prefix root.
const MARKER: &str = ".installed-openbutlerd";
/// The comment the Unix installer appends to the profile line it owns.
const PROFILE_COMMENT: &str = "# openbutler";
/// The Windows key that holds the per-user PATH.
#[cfg(windows)]
const ENVIRONMENT_KEY: &str = r"HKCU\Environment";

/// What the caller asked for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Options {
    pub purge: bool,
    pub dry_run: bool,
    pub force: bool,
}

/// What is on this machine, as far as the program directory is concerned.
pub struct Targets {
    pub install_dir: Option<PathBuf>,
    pub bin_dir: Option<PathBuf>,
    pub autostart: bool,
}

impl Targets {
    /// Nothing worth starting for: no program, no login entry, no running host
    /// and no purge request.
    pub fn nothing_to_remove(&self, host_running: bool, purge: bool) -> bool {
        self.install_dir.is_none() && !self.autostart && !host_running && !purge
    }
}

/// The prefix the running binary came from, when the installer's marker is
/// beside it. `None` means this copy was not laid down by our installer, so its
/// directory is not ours to delete.
pub fn install_dir(exe: &Path) -> Option<PathBuf> {
    let bin = exe.parent()?;
    if bin.file_name()?.to_string_lossy() != "bin" {
        return None;
    }
    let root = bin.parent()?;
    if !root.join(MARKER).is_file() {
        return None;
    }
    Some(root.to_path_buf())
}

/// Which binary the login entry runs: the host when it can be resolved beside
/// this client, the client itself otherwise. Never the client when the host is
/// there - an entry that names the wrong binary is an entry uninstall cannot
/// recognise.
pub fn host_binary(resolved: Option<PathBuf>, exe: &Path) -> PathBuf {
    resolved.unwrap_or_else(|| exe.to_path_buf())
}

/// Whether an uninstall must stop here: a running host that did not stop would
/// leave its data directory under a live writer.
pub fn must_abort(host_running: bool, stopped: bool) -> bool {
    host_running && !stopped
}

/// Removes the installer's PATH line from a profile, leaving every other byte
/// alone. The line is ours only when it names our bin directory or carries our
/// comment.
pub fn strip_profile_line(content: &str, bin_dir: &Path) -> String {
    let bin = bin_dir.display().to_string();
    let kept: Vec<&str> = content
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            if trimmed.contains(PROFILE_COMMENT) {
                return false;
            }
            !(trimmed.starts_with("export PATH=") && trimmed.contains(&bin))
        })
        .collect();
    let mut text = kept.join("\n");
    if !text.is_empty() {
        text.push('\n');
    }
    text
}

/// Removes one entry from a `;`-separated Windows PATH value: case-insensitive,
/// trailing separators ignored, every other entry untouched.
pub fn remove_path_entry(value: &str, bin_dir: &Path) -> String {
    let wanted = normalize(bin_dir);
    value
        .split(';')
        .filter(|entry| !entry.is_empty() && normalize(Path::new(entry)) != wanted)
        .collect::<Vec<_>>()
        .join(";")
}

fn normalize(path: &Path) -> String {
    path.display()
        .to_string()
        .trim()
        .trim_end_matches(['\\', '/'])
        .to_lowercase()
}

/// The `Path` value and its registry type, as `reg query` reports them.
#[cfg(windows)]
pub fn parse_environment_path(stdout: &str) -> Option<(String, String)> {
    stdout.lines().find_map(|line| {
        let mut parts = line.split_whitespace();
        if parts.next()? != "Path" {
            return None;
        }
        let kind = parts.next()?.to_owned();
        if !kind.starts_with("REG_") {
            return None;
        }
        let rest: Vec<&str> = parts.collect();
        if rest.is_empty() {
            return None;
        }
        Some((rest.join(" "), kind))
    })
}

/// Removes the program, the login entry and the PATH line.
pub async fn run(data_dir: &Path, options: Options) -> Result<String, CliError> {
    let exe = std::env::current_exe().map_err(|error| CliError::Answer(error.to_string()))?;
    let install_dir = install_dir(&exe);
    let bin_dir = install_dir.as_ref().map(|root| root.join("bin"));
    let host_program = host_binary(crate::commands::daemon::resolve_daemon_exe().ok(), &exe);
    let autostart = crate::autostart::installed(&host_program);
    let host_running = crate::commands::daemon::status(data_dir).await.is_ok();
    let targets = Targets {
        install_dir: install_dir.clone(),
        bin_dir,
        autostart,
    };

    let mut lines: Vec<String> = Vec::new();
    lines.push(ui::section("Uninstall OpenButler"));
    match (&targets.install_dir, &targets.bin_dir) {
        (Some(root), Some(bin)) => {
            lines.push(ui::kv("Program", &root.display().to_string()));
            lines.push(ui::kv("PATH entry", &bin.display().to_string()));
        }
        _ => lines.push(ui::warn(
            "This OpenButler was not installed by the installer, so there is no program directory to remove.",
        )),
    }
    lines.push(ui::kv(
        "Data",
        &format!(
            "{} {}",
            data_dir.display(),
            if options.purge {
                "(will be removed)"
            } else {
                "(kept)"
            }
        ),
    ));
    if host_running {
        lines.push(ui::kv("Background host", "running (will be stopped)"));
    }
    if autostart {
        lines.push(ui::kv("Login startup", "installed (will be removed)"));
    }

    if targets.nothing_to_remove(host_running, options.purge) {
        lines.push(ui::info("Nothing to remove."));
        return Ok(lines.join("\n"));
    }
    if options.dry_run {
        lines.push(ui::info("Dry run: nothing was changed."));
        return Ok(lines.join("\n"));
    }
    if !options.force {
        confirm(options.purge)?;
    }

    if host_running {
        let stopped = crate::commands::daemon::stop(data_dir).await;
        if must_abort(host_running, stopped.is_ok()) {
            let reason = match &stopped {
                Err(error) => error.to_string(),
                Ok(text) => text.clone(),
            };
            return Err(CliError::Answer(format!(
                "The background host is still running, so nothing was removed: {reason}"
            )));
        }
        lines.push(ui::success("Background host stopped."));
    }
    if autostart {
        lines.push(crate::autostart::uninstall(&host_program)?);
    }
    if let Some(bin) = &targets.bin_dir {
        lines.extend(remove_paths(bin)?);
    }
    if let Some(root) = &targets.install_dir {
        match std::fs::remove_dir_all(root) {
            Ok(()) => lines.push(ui::success("Program files removed.")),
            // The running binary holds its own directory on Windows: a watcher
            // finishes the job once this process is gone.
            Err(_) if exe.starts_with(root) => {
                deferred_removal(root)?;
                lines.push(ui::info(
                    "Program files will be removed as this process exits.",
                ));
            }
            Err(error) => {
                return Err(CliError::Answer(format!(
                    "Could not remove {}: {error}",
                    root.display()
                )));
            }
        }
    }
    if options.purge {
        match std::fs::remove_dir_all(data_dir) {
            Ok(()) => lines.push(ui::success("Data removed.")),
            Err(_) => lines.push(ui::warn(&format!(
                "Could not fully remove {}; delete it manually.",
                data_dir.display()
            ))),
        }
    } else if targets.install_dir.is_some() {
        lines.push(ui::info(&format!(
            "Your data stays at {}. Delete that folder to remove it.",
            data_dir.display()
        )));
    }
    lines.push(String::new());
    lines.push(ui::success(
        "OpenButler uninstalled. Restart your terminal to clear the PATH entry from this session.",
    ));
    Ok(lines.join("\n"))
}

fn confirm(purge: bool) -> Result<(), CliError> {
    if !std::io::stdin().is_terminal() {
        return Err(CliError::Answer(
            "Refusing to uninstall without a terminal. Re-run with --force to skip the prompt."
                .to_owned(),
        ));
    }
    let question = if purge {
        "Remove the program and all data (memories, sessions, settings)? [y/N] "
    } else {
        "Remove the program? Your data is kept. [y/N] "
    };
    eprint!("{question}");
    let mut answer = String::new();
    std::io::stdin()
        .read_line(&mut answer)
        .map_err(|error| CliError::Answer(error.to_string()))?;
    let answer = answer.trim().to_lowercase();
    if answer == "y" || answer == "yes" {
        Ok(())
    } else {
        Err(CliError::Answer(
            "Cancelled. Nothing was removed.".to_owned(),
        ))
    }
}

/// Strips the PATH entry from whatever holds it on this platform.
fn remove_paths(bin_dir: &Path) -> Result<Vec<String>, CliError> {
    #[cfg(unix)]
    {
        let mut lines = Vec::new();
        for profile in profiles() {
            let Ok(content) = std::fs::read_to_string(&profile) else {
                continue;
            };
            let stripped = strip_profile_line(&content, bin_dir);
            if stripped != content {
                std::fs::write(&profile, stripped)
                    .map_err(|error| CliError::Answer(error.to_string()))?;
                lines.push(ui::success(&format!(
                    "Removed the PATH entry from {}.",
                    profile.display()
                )));
            }
        }
        Ok(lines)
    }
    #[cfg(windows)]
    {
        let output = std::process::Command::new("reg")
            .args(["query", ENVIRONMENT_KEY, "/v", "Path"])
            .output()
            .map_err(|error| CliError::Answer(error.to_string()))?;
        if !output.status.success() {
            return Ok(Vec::new());
        }
        let text = String::from_utf8_lossy(&output.stdout).to_string();
        let Some((value, kind)) = parse_environment_path(&text) else {
            return Ok(Vec::new());
        };
        let cleaned = remove_path_entry(&value, bin_dir);
        if cleaned == value {
            return Ok(Vec::new());
        }
        let status = std::process::Command::new("reg")
            .args([
                "add",
                ENVIRONMENT_KEY,
                "/v",
                "Path",
                "/t",
                &kind,
                "/d",
                &cleaned,
                "/f",
            ])
            .status()
            .map_err(|error| CliError::Answer(error.to_string()))?;
        if !status.success() {
            return Err(CliError::Answer(
                "Could not update the user PATH.".to_owned(),
            ));
        }
        Ok(vec![ui::success(
            "Removed the PATH entry from the user environment.",
        )])
    }
}

/// The files the Unix installer may have written its line into.
#[cfg(unix)]
fn profiles() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut files = Vec::new();
    if let Some(home) = home {
        for name in [".profile", ".bashrc", ".zshrc"] {
            files.push(home.join(name));
        }
    }
    files
}

/// A watcher that removes the directory once this process is gone: the running
/// binary holds it on Windows, and a plain delete would fail.
fn deferred_removal(directory: &Path) -> Result<(), CliError> {
    let pid = std::process::id();
    let path = directory.display().to_string();
    #[cfg(windows)]
    {
        let script = format!(
            "$p={pid}; while (Get-Process -Id $p -ErrorAction SilentlyContinue) {{ Start-Sleep -Milliseconds 300 }}; Remove-Item -LiteralPath '{}' -Recurse -Force -ErrorAction SilentlyContinue",
            path.replace('\'', "''")
        );
        std::process::Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .spawn()
            .map_err(|error| CliError::Answer(error.to_string()))?;
    }
    #[cfg(unix)]
    {
        let quoted = path.replace('\'', "'\\''");
        let script =
            format!("while kill -0 {pid} 2>/dev/null; do sleep 0.2; done; rm -rf '{quoted}'");
        std::process::Command::new("sh")
            .args(["-c", &script])
            .spawn()
            .map_err(|error| CliError::Answer(error.to_string()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{install_dir, remove_path_entry, strip_profile_line};
    use std::path::Path;

    #[test]
    fn the_login_entry_runs_the_host_when_it_is_there() {
        use super::{host_binary, must_abort};
        let client = Path::new("C:/prefix/bin/openbutler.exe");
        assert_eq!(
            host_binary(
                Some(Path::new("C:/prefix/bin/openbutlerd.exe").to_path_buf()),
                client
            ),
            Path::new("C:/prefix/bin/openbutlerd.exe")
        );
        // No host beside the client: the client path is all there is.
        assert_eq!(host_binary(None, client), client);
        // A host that did not stop blocks the removal; anything else does not.
        assert!(must_abort(true, false));
        assert!(!must_abort(true, true));
        assert!(!must_abort(false, false));
    }

    #[test]
    fn a_build_tree_is_not_an_installed_program() {
        let root = std::env::temp_dir().join(format!("ob-un-{}", std::process::id()));
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).ok();
        // No marker: the directory belongs to whoever built it.
        assert_eq!(install_dir(&bin.join("openbutler.exe")), None);
        std::fs::write(root.join(".installed-openbutlerd"), "version=0.0.1\n").ok();
        assert_eq!(install_dir(&bin.join("openbutler.exe")), Some(root.clone()));
        // A bare binary outside a `bin` directory is not ours either.
        assert_eq!(install_dir(&root.join("openbutler.exe")), None);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn the_profile_line_is_the_only_one_removed() {
        let content = "export EDITOR=vi\nexport PATH=\"/opt/ob/bin:$PATH\"  # openbutler\nexport LANG=en_US.UTF-8\n";
        assert_eq!(
            strip_profile_line(content, Path::new("/opt/ob/bin")),
            "export EDITOR=vi\nexport LANG=en_US.UTF-8\n"
        );
        // A line the installer did not write survives, even when it exports PATH.
        let foreign = "export PATH=\"/opt/other/bin:$PATH\"\n";
        assert_eq!(
            strip_profile_line(foreign, Path::new("/opt/ob/bin")),
            foreign
        );
        // Our line is recognised by the bin directory alone as well.
        let bare = "export PATH=\"/opt/ob/bin:$PATH\"\n";
        assert_eq!(strip_profile_line(bare, Path::new("/opt/ob/bin")), "");
    }

    #[test]
    fn the_windows_path_entry_is_removed_case_insensitively() {
        let value = r"C:\Windows;C:\Users\me\.openbutler\bin\;C:\Tools";
        assert_eq!(
            remove_path_entry(value, Path::new(r"c:\users\me\.openbutler\BIN")),
            r"C:\Windows;C:\Tools"
        );
        // Nothing matches: the value comes back byte for byte.
        assert_eq!(remove_path_entry(value, Path::new(r"C:\Elsewhere")), value);
    }
}
