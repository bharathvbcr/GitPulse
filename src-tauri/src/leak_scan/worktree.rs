//! Secret scan of the working tree: every file git tracks, and every file it
//! does not track but would not ignore.
//!
//! This is the whole-tree counterpart of [`super::staged`] and
//! [`super::range`], and the primary working-tree secrets scan. It runs one
//! `git ls-files` and then reads the bytes itself, in this process, so its cost
//! is the listing plus the read — no scanner to start, which on a host where a
//! process launch costs a second or more is most of what a scan used to cost.
//!
//! Files git ignores are not read: build output, dependency trees and local
//! env files are the bulk of a checkout's bytes, and what a person wrote and
//! has not ignored is what the next `git add -A` would commit. Kingfisher, the
//! opt-in deep scan, is the one that walks the ignored files too.
//!
//! Every limit is a typed skip ([`SkipReason`]), never a silent omission: a
//! file over the per-file cap, a file the byte budget could not hold, a file
//! that could not be read, and everything past the time limit. A file whose
//! bytes are binary is read like any other, as the commit gate reads a staged
//! binary blob, so a key cannot pass here and fail there.

use super::{note_scan_completed, BlobLines, SkipReason, Tally};
use crate::engine::git_cli::git;
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::Path;
use std::time::{Duration, Instant};

/// The bounds one working-tree scan works within.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorktreeLimits {
    /// Largest single file read. A larger one is a skip.
    pub file_bytes: u64,
    /// Bytes read across the whole scan. A file the rest of the budget cannot
    /// hold is a skip; a smaller one after it is still read.
    pub total_bytes: u64,
    /// Wall time. Files not reached by then are one counted skip.
    pub time: Duration,
}

impl WorktreeLimits {
    /// What the Health panel's scan runs within: the commit gate's streamed
    /// blob budget (about ten seconds of reading at the engine's measured
    /// rate on a loaded host) and a time limit three times that, so a starved
    /// host reports what it did not reach rather than hanging the panel.
    pub const DEFAULT: Self = Self {
        file_bytes: 32 * 1024 * 1024,
        total_bytes: super::staged::STREAM_BUDGET_BYTES,
        time: Duration::from_secs(30),
    };
}

/// Bytes handed to the line reader per `read`, so a large file is never held
/// whole in memory.
const READ_CHUNK: usize = 256 * 1024;

/// A scan that could not reach a verdict. Fixed wording: git's own stderr can
/// quote paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanFailure(pub String);

/// A completed scan of the working tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeScan {
    pub tally: Tally,
    /// Paths git lists as not tracked; every other scanned path is tracked.
    pub untracked: HashSet<String>,
    /// Nested repositories and submodules, repo-relative and ending in `/`.
    /// Their contents belong to another repository's index and are not read.
    pub nested_repos: Vec<String>,
    pub duration_ms: u64,
}

/// One entry of `git ls-files -z -t`.
pub(super) struct Listed {
    pub(super) path: String,
    pub(super) tracked: bool,
}

/// Parse `git ls-files -z -t` output: `<tag> <path>` NUL-separated. `?` marks
/// a file that is not in the index; every other tag is an index entry.
///
/// One entry per path, in first-listed order, tracked winning. An unmerged
/// path is listed once per index stage, and `--deduplicate` does not apply
/// with `-t`, so without this a conflicted file is read and counted up to
/// three times.
pub(super) fn parse_listing(raw: &[u8]) -> Option<Vec<Listed>> {
    let mut out: Vec<Listed> = Vec::new();
    let mut at: HashMap<String, usize> = HashMap::new();
    for entry in raw.split(|b| *b == 0).filter(|e| !e.is_empty()) {
        let (tag, path) = (entry.first()?, entry.get(2..)?);
        if entry.get(1) != Some(&b' ') || path.is_empty() {
            return None;
        }
        let path = String::from_utf8_lossy(path).into_owned();
        let tracked = *tag != b'?';
        match at.get(&path) {
            Some(&index) => out[index].tracked |= tracked,
            None => {
                at.insert(path.clone(), out.len());
                out.push(Listed { path, tracked });
            }
        }
    }
    Some(out)
}

/// Whether `rel` stays inside the repository when joined to it. Git never
/// prints anything else; the check is for a listing this parser misread.
fn is_inside(rel: &str) -> bool {
    Path::new(rel)
        .components()
        .all(|c| matches!(c, std::path::Component::Normal(_)))
}

/// Scan every tracked and untracked-but-not-ignored file of `repo`.
pub fn scan_worktree(repo: &Path, limits: WorktreeLimits) -> Result<WorktreeScan, ScanFailure> {
    let started = Instant::now();
    let raw = git(
        repo,
        &[
            "ls-files",
            "-z",
            "-t",
            "--cached",
            "--others",
            "--exclude-standard",
        ],
    )
    .map_err(|_| ScanFailure("git could not list the working tree".into()))?;
    let mut listed = parse_listing(&raw).ok_or_else(|| {
        ScanFailure("the working-tree listing was not in the expected form".into())
    })?;
    // Tracked files first, so a scan that runs out of budget or time has read
    // what is already in history before what is only on disk.
    listed.sort_by_key(|l| !l.tracked);

    let mut tally = Tally::default();
    let mut untracked = HashSet::new();
    let mut nested_repos = Vec::new();
    let mut budget_left = limits.total_bytes;
    let deadline = started + limits.time;
    let total = listed.len();
    for (index, file) in listed.iter().enumerate() {
        if Instant::now() >= deadline {
            let left = (total - index) as u64;
            tally.skip_many(&file.path, SkipReason::TimeLimit, None, left);
            break;
        }
        if !file.tracked {
            untracked.insert(file.path.clone());
        }
        if !is_inside(file.path.trim_end_matches('/')) {
            tally.skip(&file.path, SkipReason::Unreadable, None);
            continue;
        }
        // `symlink_metadata`: a link is not followed, so a link out of the
        // tree cannot make the scan read what the repository does not hold.
        let full = repo.join(file.path.trim_end_matches('/'));
        let meta = match std::fs::symlink_metadata(&full) {
            Ok(meta) => meta,
            // Tracked but removed from disk (` D` in `git status`): there are
            // no bytes to be a hole in.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => {
                tally.skip(&file.path, SkipReason::Unreadable, None);
                continue;
            }
        };
        if meta.is_dir() {
            // git lists a nested repository or submodule as one entry.
            let mut name = file.path.trim_end_matches('/').to_string();
            name.push('/');
            nested_repos.push(name);
            continue;
        }
        if !meta.is_file() {
            continue;
        }
        let size = meta.len();
        if size > limits.file_bytes {
            tally.skip(&file.path, SkipReason::Oversized, Some(size));
        } else if size > budget_left {
            tally.skip(&file.path, SkipReason::BudgetExhausted, Some(size));
        } else {
            let read = read_into(&full, &file.path, limits.file_bytes, &mut tally);
            budget_left = budget_left.saturating_sub(read.bytes);
            if let Some(reason) = read.skipped {
                tally.skip(&file.path, reason, Some(read.bytes));
            }
        }
    }
    nested_repos.sort();
    nested_repos.dedup();
    note_scan_completed();
    Ok(WorktreeScan {
        tally,
        untracked,
        nested_repos,
        duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
    })
}

struct Read1 {
    bytes: u64,
    skipped: Option<SkipReason>,
}

/// Stream one file through the line reader. The cap is enforced on the bytes
/// actually read, not the size `stat` reported, so a file that grew since the
/// listing cannot read past it.
fn read_into(full: &Path, rel: &str, cap: u64, tally: &mut Tally) -> Read1 {
    let file = match std::fs::File::open(full) {
        Ok(file) => file,
        Err(_) => {
            return Read1 {
                bytes: 0,
                skipped: Some(SkipReason::Unreadable),
            }
        }
    };
    let mut reader = file.take(cap + 1);
    let mut lines = BlobLines::new(tally, rel);
    let mut buf = vec![0u8; READ_CHUNK];
    let mut bytes = 0u64;
    let mut skipped = None;
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                bytes += n as u64;
                if bytes > cap {
                    skipped = Some(SkipReason::Oversized);
                    break;
                }
                lines.feed(&buf[..n]);
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => {
                skipped = Some(SkipReason::Unreadable);
                break;
            }
        }
    }
    lines.finish();
    Read1 { bytes, skipped }
}
