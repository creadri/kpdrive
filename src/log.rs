//! Plain-text activity log with a retention window.
//!
//! One file per day under `~/.local/share/kpdrive/logs/`, which makes retention
//! a matter of deleting old files and search a matter of reading the days still
//! in range. Lines are `<rfc3339> <LEVEL> <message>`, so they stay greppable
//! outside kpdrive too.
//!
//! The daemon and CLI append to the same file from different processes; each
//! line is one `write` to a file opened for append, which the kernel keeps
//! whole.

use anyhow::{Context, Result};
use std::fmt::Write as _;
use std::fs;
use std::io::Write as _;
use std::path::PathBuf;

use crate::drive::civil_utc;

pub fn dir() -> PathBuf {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").expect("HOME")).join(".local/share"));
    base.join("kpdrive/logs")
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// `2026-09-19`, the name of the file a moment belongs to.
fn day(secs: i64) -> String {
    let (y, m, d, ..) = civil_utc(secs);
    format!("{y:04}-{m:02}-{d:02}")
}

fn stamp(secs: i64) -> String {
    let (y, m, d, hh, mi, ss) = civil_utc(secs);
    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mi:02}:{ss:02}Z")
}

/// Appends one line. Never fails the caller: losing a log line must not take
/// down a sync.
pub fn write(level: &str, message: &str) {
    let secs = now();
    let mut line = String::new();
    let _ = write!(line, "{} {level} {}", stamp(secs), message.replace('\n', " "));
    line.push('\n');
    let path = dir().join(format!("{}.log", day(secs)));
    let appended = fs::create_dir_all(dir()).and_then(|_| {
        fs::OpenOptions::new().create(true).append(true).open(&path).and_then(|mut f| f.write_all(line.as_bytes()))
    });
    if let Err(e) = appended {
        // Once, to stderr: a broken log directory should be visible but not fatal.
        eprintln!("kpdrive: cannot write {}: {e}", path.display());
    }
}

/// Logs and prints: the CLI shows activity, the log keeps it.
pub fn info(message: &str) {
    println!("{message}");
    write("INFO", message);
}

/// Logs and prints to stderr, for the things worth noticing.
pub fn warn(message: &str) {
    eprintln!("{message}");
    write("WARN", message);
}

pub fn error(message: &str) {
    eprintln!("{message}");
    write("ERROR", message);
}

/// Deletes day-files older than `retention_days`. Returns how many went.
pub fn prune(retention_days: u64) -> Result<usize> {
    let cutoff = day(now() - (retention_days.max(1) as i64) * 86_400);
    let Ok(entries) = fs::read_dir(dir()) else { return Ok(0) };
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str().and_then(|n| n.strip_suffix(".log")) else { continue };
        // Day names sort like dates, so a string compare is the date compare.
        if name < cutoff.as_str() {
            fs::remove_file(entry.path()).with_context(|| format!("remove {name}.log"))?;
            removed += 1;
        }
    }
    Ok(removed)
}

/// The most recent `limit` lines, newest last, optionally only those containing
/// `term` (case-insensitive).
pub fn search(term: &str, limit: usize) -> Result<Vec<String>> {
    search_in(&dir(), term, limit)
}

/// The body of [`search`], against any log directory, so it can be tested.
pub fn search_in(dir: &std::path::Path, term: &str, limit: usize) -> Result<Vec<String>> {
    let mut days: Vec<_> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().ends_with(".log"))
        .map(|e| e.path())
        .collect();
    days.sort();

    if limit == 0 {
        return Ok(Vec::new());
    }
    let needle = term.to_lowercase();
    let mut lines: Vec<String> = Vec::new();
    // Newest day first, so a big archive stops as soon as the limit is met.
    for path in days.iter().rev() {
        let Ok(text) = fs::read_to_string(path) else { continue };
        let mut matched: Vec<&str> = text
            .lines()
            .filter(|l| !l.is_empty() && (needle.is_empty() || l.to_lowercase().contains(&needle)))
            .collect();
        matched.reverse();
        for line in matched {
            lines.push(line.to_owned());
            if lines.len() >= limit {
                lines.reverse();
                return Ok(lines);
            }
        }
    }
    lines.reverse();
    Ok(lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn day_names_sort_as_dates() {
        assert_eq!(day(0), "1970-01-01");
        assert_eq!(day(1_700_000_000), "2023-11-14");
        assert!(day(1_700_000_000) < day(1_700_000_000 + 86_400));
        assert_eq!(stamp(1_700_000_000), "2023-11-14T22:13:20Z");
    }

    #[test]
    fn search_filters_and_limits() {
        let dir = std::env::temp_dir().join(format!("kpdrive-log-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("2026-09-18.log"), "A fetched one\nB pushed two\n").unwrap();
        fs::write(dir.join("2026-09-19.log"), "C fetched three\nD pushed four\n").unwrap();
        let read = |term: &str, limit: usize| search_in(&dir, term, limit).unwrap();
        assert_eq!(read("", 10).len(), 4, "all lines");
        assert_eq!(read("", 10)[0], "A fetched one", "oldest first");
        assert_eq!(read("fetched", 10), vec!["A fetched one", "C fetched three"]);
        assert_eq!(read("FETCHED", 10).len(), 2, "case-insensitive");
        assert_eq!(read("", 2), vec!["C fetched three", "D pushed four"], "limit keeps the newest");
        assert!(read("nothing here", 10).is_empty());
        fs::remove_dir_all(&dir).unwrap();
    }
}
