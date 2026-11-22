//! `openbutler logs` — read the machine log's tail without opening a
//! file.
//!
//! The source reads the structured `logs/openbutler.log` and filters
//! parsed records by level/event; this build's daemon writes no
//! structured file yet, so the honest machine log is the `daemon.log`
//! that `daemon start` captures — raw lines, `--lines` tailing and
//! `--file` override from the source, level/event/json filtering
//! deferred with the daemon-side structured logger (documented).

use std::path::{Path, PathBuf};

use crate::CliError;

/// Reads the log tail: the daemon's own captured output by default, or
/// `--file` verbatim.
pub async fn run(data_dir: &Path, lines: usize, file: Option<PathBuf>) -> Result<String, CliError> {
    let path = file.unwrap_or_else(|| data_dir.join("daemon.log"));
    // The source treats an unreadable file as empty records.
    let records = tail_lines(&path, lines).unwrap_or_default();
    if records.is_empty() {
        return Ok(format!("No log lines at {}.", path.display()));
    }
    Ok(records.join("\n"))
}

/// Tail by seeking: blocks are read backwards until the wanted newline
/// count is covered, so a large log costs its tail, not its size (the
/// source read the whole file into memory).
fn tail_lines(path: &Path, lines: usize) -> std::io::Result<Vec<String>> {
    use std::io::{Read, Seek, SeekFrom};

    let mut file = std::fs::File::open(path)?;
    let size = file.metadata()?.len();
    const BLOCK: u64 = 64 * 1024;

    let mut position = size;
    let mut newlines = 0usize;
    let mut chunks: Vec<Vec<u8>> = Vec::new();
    while position > 0 && newlines <= lines {
        let start = position.saturating_sub(BLOCK);
        let mut block = vec![0u8; (position - start) as usize];
        file.seek(SeekFrom::Start(start))?;
        file.read_exact(&mut block)?;
        newlines += block.iter().filter(|byte| **byte == b'\n').count();
        chunks.insert(0, block);
        position = start;
    }

    let mut data = Vec::new();
    for chunk in chunks {
        data.extend(chunk);
    }
    let text = String::from_utf8_lossy(&data);
    let mut records: Vec<String> = text
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line).to_owned())
        .collect();
    if records.last().is_some_and(|line| line.is_empty()) {
        records.pop();
    }
    Ok(records.split_off(records.len().saturating_sub(lines)))
}
