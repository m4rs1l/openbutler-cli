//! The stored backend: which host this client talks to, and the session it
//! signed in with.
//!
//! Two files, deliberately. `backend.json` is the protocol-shaped target (host,
//! port, username, device) and carries no secret, so it can be read, copied and
//! printed. `credential.json` holds the session token and is written private.
//! Splitting them is what lets the target be inspectable without exposing the
//! credential, and leaves room for an OS keychain to hold only the second file.
//!
//! The client owns both: the daemon never reads them, and the token is the
//! client's own identity, not the host's state.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use openbutler_protocol::BackendTarget;
use serde::{Deserialize, Serialize};

/// One stored backend: the target, plus the session that authenticates it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredBackend {
    /// Where this client points and who it signs in as.
    pub target: BackendTarget,
    /// The session the host issued at sign-in.
    pub session: StoredSession,
}

/// The credential file: the session token, and nothing else.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredSession {
    /// The session id `device list` prints.
    pub session_id: String,
    /// The token every connection presents as `auth`.
    pub token: String,
    /// When the host stops accepting it (informational: the host decides).
    pub expires_at: String,
}

/// The target file's name inside the client's data directory.
pub const BACKEND_FILE: &str = "backend.json";
/// The credential file's name inside the client's data directory.
pub const CREDENTIAL_FILE: &str = "credential.json";

fn backend_path(data_dir: &Path) -> PathBuf {
    data_dir.join(BACKEND_FILE)
}

fn credential_path(data_dir: &Path) -> PathBuf {
    data_dir.join(CREDENTIAL_FILE)
}

/// Reads the stored backend.
///
/// `Ok(None)` means this client has never signed in (a local-only install).
/// A file that exists but cannot be used is an error, never a silent fallback:
/// answering from a different host than the owner believes is worse than a
/// clear refusal that names the file.
pub fn load(data_dir: &Path) -> Result<Option<StoredBackend>, String> {
    let target_path = backend_path(data_dir);
    let raw = match fs::read_to_string(&target_path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(cannot_read(&target_path, &error.to_string())),
    };
    let target: BackendTarget = serde_json::from_str(&raw)
        .map_err(|error| cannot_read(&target_path, &error.to_string()))?;

    let credential_path = credential_path(data_dir);
    let credential_raw = match fs::read_to_string(&credential_path) {
        Ok(raw) => raw,
        Err(error) => return Err(cannot_read(&credential_path, &error.to_string())),
    };
    let session: StoredSession = serde_json::from_str(&credential_raw)
        .map_err(|error| cannot_read(&credential_path, &error.to_string()))?;
    Ok(Some(StoredBackend { target, session }))
}

/// Writes both files. The target is world-readable on purpose; the credential
/// is written private (`0600` on unix, the per-user data-directory ACL on
/// Windows).
pub fn save(
    data_dir: &Path,
    target: &BackendTarget,
    session: &StoredSession,
) -> Result<(), String> {
    fs::create_dir_all(data_dir).map_err(|error| error.to_string())?;
    let target_json = serde_json::to_string_pretty(target).map_err(|error| error.to_string())?;
    fs::write(backend_path(data_dir), format!("{target_json}\n"))
        .map_err(|error| format!("cannot write {}: {error}", backend_path(data_dir).display()))?;
    let session_json = serde_json::to_string_pretty(session).map_err(|error| error.to_string())?;
    write_private(
        &credential_path(data_dir),
        format!("{session_json}\n").as_bytes(),
    )
    .map_err(|error| {
        format!(
            "cannot write {}: {error}",
            credential_path(data_dir).display()
        )
    })
}

/// Removes both files. `true` when anything was there, so the caller can say
/// whether it forgot something or there was nothing to forget.
pub fn clear(data_dir: &Path) -> Result<bool, String> {
    let mut removed = false;
    for path in [backend_path(data_dir), credential_path(data_dir)] {
        match fs::remove_file(&path) {
            Ok(()) => removed = true,
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(format!("cannot remove {}: {error}", path.display())),
        }
    }
    Ok(removed)
}

fn cannot_read(path: &Path, reason: &str) -> String {
    format!(
        "The stored backend at {} cannot be read: {reason}. Run `openbutler connect --forget` and sign in again.",
        path.display()
    )
}

/// Writes a credential file with the platform's private mode where it exists.
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;

    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)
}
