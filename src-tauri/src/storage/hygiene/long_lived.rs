//! Long-lived data the cleaner reports and never touches.
//!
//! A full disk with "the cleaner found nothing" needs an explanation: space is
//! often held by data GitPulse will not clean (model caches, agent
//! configuration and transcripts, toolchains). This module lists where, with a
//! size, so a person can decide. It is report-only by construction: it returns
//! a list of paths and sizes and has no reference to a removal path, a
//! candidate or a scope. Its reads are size-only, bounded by their own budget
//! (separate from the agent scan's and the target walk's), never follow a
//! symlink and never cross onto another filesystem.
//!
//! The default set is fixed, generic and existence-checked: a path that is not
//! there is absent from the list, not a zero row. Paths the cleaner already
//! offers through the shared-cache inventory (the Cargo registry, npm, uv,
//! pnpm, Go) are deliberately not in it: a path GitPulse manages is not "not
//! cleaned by GitPulse". The person can add absolute directories of their own
//! in the cleaner's settings; each is validated when saved and again when
//! reported.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

/// Most user-added paths a policy may carry.
pub(crate) const MAX_USER_PATHS: usize = 16;
/// Entries one path's walk may visit, and the time it may spend.
const PATH_ENTRIES: usize = 300_000;
const PATH_SECONDS: u64 = 8;
/// What the whole report may spend, however many paths there are.
const TOTAL_ENTRIES: usize = 1_000_000;
const TOTAL_SECONDS: u64 = 30;
/// Directory entries between clock and cancellation checks.
const CHECK_STRIDE: usize = 512;

/// One row of the report. `bytes` is logical size; `at_least` says the walk was
/// cut short (entries, time, a directory it could not read, another
/// filesystem), so the figure is a floor. `error` is set instead of `bytes`
/// when the path could not be read at all: that is not zero bytes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LongLivedItem {
    pub label: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub at_least: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// The structural part of a user path, with no filesystem access: absolute,
/// bounded, no control characters, no `.` or `..` component.
pub(crate) fn check_shape(path: &str) -> Result<(), String> {
    if path.len() > 4096 || path.chars().any(char::is_control) {
        return Err("a long-lived data path is too long or holds a control character".into());
    }
    let parsed = Path::new(path);
    if !parsed.is_absolute() {
        return Err(format!("{path} is not an absolute path"));
    }
    // `Path::components` drops an interior `.`, so the text is checked too.
    if parsed
        .components()
        .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
        || path
            .split(['/', '\\'])
            .any(|part| part == "." || part == "..")
    {
        return Err(format!("{path} contains . or .. components"));
    }
    Ok(())
}

/// A user-added path, checked against the filesystem: it exists, is a directory
/// and not a symlink (nor reached through one), is not `/`, the home directory
/// or an ancestor of it, and is not an ancestor of (or the same as) a reclaim
/// root. Returns the path to walk.
pub(crate) fn validate_user_path(
    path: &str,
    roots: &[String],
    home: &Path,
) -> Result<PathBuf, String> {
    check_shape(path)?;
    let given = Path::new(path);
    let meta =
        std::fs::symlink_metadata(given).map_err(|e| format!("{path} cannot be read: {e}"))?;
    if meta.file_type().is_symlink() {
        return Err(format!(
            "{path} is a symlink; GitPulse does not follow symlinks"
        ));
    }
    if !meta.is_dir() {
        return Err(format!("{path} is not a directory"));
    }
    let canonical = crate::engine::process::canonicalize_plain(given)
        .map_err(|e| format!("{path} cannot be resolved: {e}"))?;
    if canonical != given {
        return Err(format!(
            "{path} is reached through a symlink (it resolves to {}); give the real path",
            canonical.display()
        ));
    }
    let home =
        crate::engine::process::canonicalize_plain(home).unwrap_or_else(|_| home.to_path_buf());
    // The root of any volume has no parent: `/`, or `C:\` on Windows.
    if given.parent().is_none() || home.starts_with(given) {
        return Err(format!(
            "{path} is the filesystem root, the home directory or one of its ancestors; list a directory inside it instead"
        ));
    }
    for root in roots {
        let root = Path::new(root);
        let root =
            crate::engine::process::canonicalize_plain(root).unwrap_or_else(|_| root.to_path_buf());
        if root.starts_with(given) {
            return Err(format!(
                "{path} contains the repository root {}, so the cleaner would be listing what it also manages",
                root.display()
            ));
        }
    }
    Ok(canonical)
}

/// The fixed default set, under `home`, for the Claude config directories given.
/// Nothing personal or project-specific: tool data directories by their
/// conventional names.
fn defaults(home: &Path, claude_dirs: &[PathBuf]) -> Vec<(String, PathBuf)> {
    let mut out: Vec<(String, PathBuf)> = vec![
        ("Hugging Face cache".into(), home.join(".cache/huggingface")),
        ("Ollama models".into(), home.join(".ollama")),
        ("LM Studio".into(), home.join(".lmstudio")),
        ("PyTorch hub cache".into(), home.join(".cache/torch")),
        (
            "Claude desktop app data".into(),
            home.join("Library/Application Support/Claude"),
        ),
        (
            "Rust toolchains (rustup)".into(),
            home.join(".rustup/toolchains"),
        ),
    ];
    for dir in claude_dirs {
        out.push(("Claude configuration directory".into(), dir.clone()));
    }
    out
}

/// What a walk found.
#[derive(Debug)]
struct Reading {
    bytes: u64,
    floor: bool,
}

/// The shared spend of one report.
struct Spend {
    entries: usize,
    deadline: Instant,
}

/// Sums the logical size of every regular file under `root`.
///
/// Size-only: no per-file map is kept. A symlink is skipped (never followed,
/// not counted); a directory on another filesystem is not entered and makes
/// the figure a floor; a file with more than one hard link is counted once (by
/// device and inode). `Err` when the root itself could not be read.
fn size_of(root: &Path, spend: &mut Spend, cancel: &AtomicBool) -> Result<Reading, String> {
    let meta = std::fs::symlink_metadata(root).map_err(|e| format!("could not be read: {e}"))?;
    if meta.file_type().is_symlink() {
        return Err("is a symlink; GitPulse does not follow symlinks".into());
    }
    if !meta.is_dir() {
        return Err("is not a directory".into());
    }
    #[cfg(unix)]
    let device = meta.dev();
    #[cfg(unix)]
    let mut linked: HashSet<(u64, u64)> = HashSet::new();
    let deadline = spend
        .deadline
        .min(Instant::now() + Duration::from_secs(PATH_SECONDS));
    let mut reading = Reading {
        bytes: 0,
        floor: false,
    };
    let mut visited = 0usize;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if dir == root => return Err(format!("could not be read: {e}")),
            Err(_) => {
                reading.floor = true;
                continue;
            }
        };
        for item in entries {
            visited += 1;
            spend.entries += 1;
            let stop = visited > PATH_ENTRIES
                || spend.entries > TOTAL_ENTRIES
                || (visited.is_multiple_of(CHECK_STRIDE)
                    && (Instant::now() >= deadline || cancel.load(Ordering::Relaxed)));
            if stop {
                reading.floor = true;
                return Ok(reading);
            }
            let Ok(entry) = item else {
                reading.floor = true;
                continue;
            };
            let Ok(meta) = entry.metadata() else {
                reading.floor = true;
                continue;
            };
            let kind = meta.file_type();
            if kind.is_symlink() {
                continue;
            }
            if kind.is_dir() {
                #[cfg(unix)]
                if meta.dev() != device {
                    reading.floor = true;
                    continue;
                }
                stack.push(entry.path());
            } else if kind.is_file() {
                #[cfg(unix)]
                if meta.nlink() > 1 && !linked.insert((meta.dev(), meta.ino())) {
                    continue;
                }
                reading.bytes = reading.bytes.saturating_add(meta.len());
            }
        }
    }
    Ok(reading)
}

/// Lists the long-lived data held under `home` and the paths the policy names,
/// with sizes. Report-only and read-only: nothing here is a candidate or is
/// passed to a removal. Problems reading the policy's own paths are rows with
/// an `error`; nothing here can fail a preview or change what it approves.
pub(crate) fn report(
    user_paths: &[String],
    roots: &[String],
    home: &Path,
    claude_dirs: &[PathBuf],
    cancel: &AtomicBool,
) -> Vec<LongLivedItem> {
    let mut spend = Spend {
        entries: 0,
        deadline: Instant::now() + Duration::from_secs(TOTAL_SECONDS),
    };
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut rows = Vec::new();
    let mut measure = |label: String, path: PathBuf, rows: &mut Vec<LongLivedItem>| {
        if !seen.insert(path.clone()) {
            return;
        }
        let shown = path.to_string_lossy().into_owned();
        let row = |bytes: Option<u64>, at_least: bool, error: Option<String>| LongLivedItem {
            label: label.clone(),
            path: shown.clone(),
            bytes,
            at_least,
            error,
        };
        if spend.entries >= TOTAL_ENTRIES
            || Instant::now() >= spend.deadline
            || cancel.load(Ordering::Relaxed)
        {
            rows.push(row(
                None,
                false,
                Some("not measured: the report's total budget was spent".into()),
            ));
            return;
        }
        rows.push(match size_of(&path, &mut spend, cancel) {
            Ok(reading) => row(Some(reading.bytes), reading.floor, None),
            Err(why) => row(None, false, Some(why)),
        });
    };
    for (label, path) in defaults(home, claude_dirs) {
        // Absent is absent, not zero.
        if std::fs::symlink_metadata(&path).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
        {
            continue;
        }
        measure(label, path, &mut rows);
    }
    for path in user_paths {
        match validate_user_path(path, roots, home) {
            Ok(real) => measure("Added by you".into(), real, &mut rows),
            Err(why) => rows.push(LongLivedItem {
                label: "Added by you".into(),
                path: path.clone(),
                bytes: None,
                at_least: false,
                error: Some(format!("not measured: {why}")),
            }),
        }
    }
    rows
}

#[cfg(test)]
mod tests;
