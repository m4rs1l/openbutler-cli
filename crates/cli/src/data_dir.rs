//! Data directory resolution — the CLI's copy of the daemon's two-rung
//! rule (`crate::config::data_dir` in openbutler-daemon): the environment
//! first, the platform convention second, nothing else consulted.
//!
//! It lives here because cross-repo code sharing stops at the contract:
//! `openbutler-protocol` stays pure path math and takes the directory as
//! input. The pin tests keep this copy honest against the recorded
//! platform vectors.

use std::path::PathBuf;

/// Resolves the data directory: `OPENBUTLER_DATA_DIR` when set, otherwise
/// the platform default.
pub fn data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("OPENBUTLER_DATA_DIR").filter(|dir| !dir.is_empty()) {
        return PathBuf::from(dir);
    }
    default_data_dir()
}

fn default_data_dir() -> PathBuf {
    #[cfg(windows)]
    {
        let base = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| home_dir().join("AppData").join("Local"));
        base.join("openbutler-daemon")
    }
    #[cfg(target_os = "macos")]
    {
        home_dir()
            .join("Library")
            .join("Application Support")
            .join("openbutler-daemon")
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let base = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home_dir().join(".local").join("share"));
        base.join("openbutler-daemon")
    }
}

fn home_dir() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}
