//! Bounded file claims for one repository.
//!
//! A claim says who intends to edit a path, or who the edit hook just let
//! through. It is not a git change: the file lives under
//! `<git-common-dir>/gitpulse/file-claims.jsonl`, beside the lane bus, and
//! nothing here updates a ref, an index, or a worktree file. There is no MCP
//! write. Declared claims are recorded when a run starts from a card's
//! planned files. Observed claims are recorded by the edit hook.
//!
//! The store is append-only until it crosses its size threshold, then it is
//! rewritten down to the cap with expired rows dropped. A missing file is an
//! empty store. Any other read or write failure is an error, so a caller can
//! refuse to treat it as a clean result.

use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// How many live claims one repository keeps. Older rows are dropped first.
pub const DEFAULT_CAP: usize = 4096;
/// How long a claim still names a holder. Tests pass their own clock.
pub const DEFAULT_TTL_MS: u64 = 30 * 60 * 1000;
/// A store larger than this is unreadable, not silently truncated.
const MAX_STORE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_LINE_BYTES: usize = 2048;
const MAX_FIELD_BYTES: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimSource {
    Declared,
    Observed,
}

/// One claim. `path` is repo-relative. `worktree` is the checkout it belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileClaim {
    pub source: ClaimSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    pub path: String,
    pub worktree: String,
    pub ts_ms: u64,
    /// Absolute cwd the hook reported, when this claim is what attribution
    /// should prefer over the process table.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

pub fn store_path(git_common_dir: &Path) -> PathBuf {
    git_common_dir.join("gitpulse").join("file-claims.jsonl")
}

fn git_common(repo: &Path) -> Result<PathBuf, String> {
    crate::engine::git_cli::resolve_git_common_dir(repo).map_err(|error| error.into_message())
}

/// Repo-relative path, or why it cannot be claimed.
pub fn normalize_rel(path: &str) -> Result<String, String> {
    let path = path.trim().replace('\\', "/");
    if path.is_empty() || path.starts_with('/') || path.contains('\0') {
        return Err("a claim path has to be repo-relative".into());
    }
    let mut parts = Vec::new();
    for part in path.split('/') {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." {
            return Err("a claim path has to stay inside the repository".into());
        }
        parts.push(part);
    }
    if parts.is_empty() {
        return Err("a claim path is empty".into());
    }
    let joined = parts.join("/");
    if joined.len() > MAX_FIELD_BYTES {
        return Err("a claim path is too long".into());
    }
    Ok(joined)
}

fn bounded(value: &str, what: &str) -> Result<(), String> {
    if value.len() > MAX_FIELD_BYTES || value.chars().any(|c| c.is_control()) {
        return Err(format!("{what} is too long or spans lines"));
    }
    Ok(())
}

fn validate(claim: &FileClaim) -> Result<(), String> {
    normalize_rel(&claim.path)?;
    bounded(&claim.worktree, "a claim worktree")?;
    if claim.worktree.is_empty() {
        return Err("a claim needs the worktree it belongs to".into());
    }
    match claim.source {
        ClaimSource::Observed => {
            let session = claim.session_id.as_deref().unwrap_or("");
            if session.is_empty() {
                return Err("an observed claim needs a session_id".into());
            }
            bounded(session, "a claim session_id")?;
        }
        ClaimSource::Declared => {
            let run = claim.run_id.as_deref().unwrap_or("");
            if run.is_empty() {
                return Err("a declared claim needs a run_id".into());
            }
            bounded(run, "a claim run_id")?;
        }
    }
    if let Some(cwd) = &claim.cwd {
        bounded(cwd, "a claim cwd")?;
    }
    Ok(())
}

/// The newest hook cwd recorded for `session_id`, if one is still in `rows`.
pub fn last_claimed_cwd<'a>(rows: &'a [FileClaim], session_id: &str) -> Option<&'a str> {
    if session_id.is_empty() {
        return None;
    }
    rows.iter().rev().find_map(|claim| {
        if claim.session_id.as_deref() != Some(session_id) {
            return None;
        }
        claim.cwd.as_deref().filter(|cwd| !cwd.is_empty())
    })
}

/// Active claims for the repository that contains `worktree`. A missing store
/// is empty. Anything else the store could not do is an error.
pub fn read_for_worktree(worktree: &Path, now_ms: u64) -> Result<Vec<FileClaim>, String> {
    let common = git_common(worktree)?;
    read_store(
        &store_path(&common),
        now_ms,
        DEFAULT_TTL_MS,
        DEFAULT_CAP,
    )
}

pub fn read_store(store: &Path, now_ms: u64, ttl_ms: u64, cap: usize) -> Result<Vec<FileClaim>, String> {
    with_lock(store, false, || {
        let rows = load_lines(store)?;
        Ok(trim(rows, now_ms, ttl_ms, cap))
    })
}

/// Append one observed claim for an edit the hook is letting through.
pub fn record_observed(
    worktree: &Path,
    session_id: &str,
    path: &str,
    cwd: &str,
    now_ms: u64,
) -> Result<(), String> {
    let common = git_common(worktree)?;
    let claim = FileClaim {
        source: ClaimSource::Observed,
        session_id: Some(session_id.to_string()),
        run_id: None,
        path: normalize_rel(path)?,
        worktree: worktree.display().to_string(),
        ts_ms: now_ms,
        cwd: Some(cwd.to_string()),
    };
    append_claim(
        &store_path(&common),
        &claim,
        DEFAULT_CAP,
        DEFAULT_TTL_MS,
        now_ms,
    )
}

/// Declared claims for the paths a run said it would edit. An empty list
/// writes nothing.
pub fn declare(
    git_common_dir: &Path,
    run_id: &str,
    worktree: &str,
    paths: &[String],
    now_ms: u64,
) -> Result<usize, String> {
    if run_id.is_empty() || paths.is_empty() {
        return Ok(0);
    }
    let store = store_path(git_common_dir);
    for path in paths {
        let claim = FileClaim {
            source: ClaimSource::Declared,
            session_id: None,
            run_id: Some(run_id.to_string()),
            path: normalize_rel(path)?,
            worktree: worktree.to_string(),
            ts_ms: now_ms,
            cwd: None,
        };
        append_claim(&store, &claim, DEFAULT_CAP, DEFAULT_TTL_MS, now_ms)?;
    }
    Ok(paths.len())
}

pub fn append_claim(
    store: &Path,
    claim: &FileClaim,
    cap: usize,
    ttl_ms: u64,
    now_ms: u64,
) -> Result<(), String> {
    validate(claim)?;
    let cap = cap.max(1);
    let line = serde_json::to_string(claim).map_err(|error| error.to_string())?;
    if line.len() > MAX_LINE_BYTES {
        return Err("a claim exceeds its size limit".into());
    }
    with_lock(store, true, || {
        let meta_len = fs::metadata(store).map(|meta| meta.len()).unwrap_or(0);
        let threshold = (cap as u64).saturating_mul(96).max(96);
        if meta_len == 0 {
            return write_lines(store, &[line]);
        }
        if meta_len < threshold && meta_len < MAX_STORE_BYTES {
            return append_line(store, &line);
        }
        let mut rows = load_lines(store)?;
        rows.push(claim.clone());
        let kept = trim(rows, now_ms, ttl_ms, cap);
        let lines = kept
            .iter()
            .map(|row| serde_json::to_string(row).map_err(|error| error.to_string()))
            .collect::<Result<Vec<_>, _>>()?;
        write_lines(store, &lines)
    })
}

fn trim(rows: Vec<FileClaim>, now_ms: u64, ttl_ms: u64, cap: usize) -> Vec<FileClaim> {
    let mut indexed: Vec<(usize, FileClaim)> = rows
        .into_iter()
        .enumerate()
        .filter(|(_, claim)| now_ms.saturating_sub(claim.ts_ms) <= ttl_ms)
        .collect();
    indexed.sort_by_key(|(index, claim)| (claim.ts_ms, *index));
    if indexed.len() > cap {
        let skip = indexed.len() - cap;
        indexed.drain(0..skip);
    }
    indexed.into_iter().map(|(_, claim)| claim).collect()
}

fn load_lines(store: &Path) -> Result<Vec<FileClaim>, String> {
    let file = match fs::File::open(store) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(format!("cannot read claims {}: {error}", store.display()));
        }
    };
    let mut buf = Vec::new();
    file.take(MAX_STORE_BYTES + 1)
        .read_to_end(&mut buf)
        .map_err(|error| format!("cannot read claims {}: {error}", store.display()))?;
    if buf.len() as u64 > MAX_STORE_BYTES {
        return Err(format!(
            "claims store {} exceeds {MAX_STORE_BYTES} bytes",
            store.display()
        ));
    }
    let text = String::from_utf8(buf)
        .map_err(|_| format!("claims store {} is not UTF-8", store.display()))?;
    let mut rows = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.is_empty() {
            continue;
        }
        let claim: FileClaim = serde_json::from_str(line).map_err(|error| {
            format!(
                "claims store {} line {} is unreadable ({error})",
                store.display(),
                index + 1
            )
        })?;
        rows.push(claim);
    }
    Ok(rows)
}

fn append_line(store: &Path, line: &str) -> Result<(), String> {
    let mut file = open_store(store, true)?;
    writeln!(file, "{line}")
        .map_err(|error| format!("cannot append claim {}: {error}", store.display()))?;
    file.sync_all()
        .map_err(|error| format!("cannot append claim {}: {error}", store.display()))
}

fn write_lines(store: &Path, lines: &[String]) -> Result<(), String> {
    if let Some(parent) = store.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    }
    let tmp = store.with_extension("jsonl.tmp");
    {
        let mut file = open_at(&tmp, false)?;
        for line in lines {
            writeln!(file, "{line}")
                .map_err(|error| format!("cannot write claims {}: {error}", tmp.display()))?;
        }
        file.sync_all()
            .map_err(|error| format!("cannot write claims {}: {error}", tmp.display()))?;
    }
    fs::rename(&tmp, store).map_err(|error| {
        format!(
            "cannot replace claims {} with {}: {error}",
            store.display(),
            tmp.display()
        )
    })
}

fn open_store(store: &Path, append: bool) -> Result<fs::File, String> {
    if let Some(parent) = store.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    }
    open_at(store, append)
}

fn open_at(path: &Path, append: bool) -> Result<fs::File, String> {
    let mut options = OpenOptions::new();
    options.write(true).create(true).read(true);
    if append {
        options.append(true);
    } else {
        options.truncate(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(path)
        .map_err(|error| format!("cannot open claims {}: {error}", path.display()))
}

fn with_lock<T>(
    store: &Path,
    create: bool,
    body: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    // One store is touched by the hook and by a launch. The mutex covers this
    // process; the lock file covers the others. Both are released on drop.
    static LOCAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _local = LOCAL.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let _file = acquire(store, create)?;
    body()
}

struct HeldLock {
    file: fs::File,
}

impl Drop for HeldLock {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            // SAFETY: `file` is an open descriptor this guard owns.
            unsafe {
                libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
            }
        }
    }
}

fn acquire(store: &Path, create: bool) -> Result<Option<HeldLock>, String> {
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        let lock_path = store.with_extension("lock");
        if create {
            if let Some(parent) = lock_path.parent() {
                fs::create_dir_all(parent)
                    .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
            }
        }
        let mut options = OpenOptions::new();
        options.read(true).write(true);
        if create {
            options.create(true);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = match options.open(&lock_path) {
            Ok(file) => file,
            Err(error) if !create && error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(None);
            }
            Err(error) => {
                return Err(format!(
                    "cannot open claim lock {}: {error}",
                    lock_path.display()
                ));
            }
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(20);
        loop {
            // SAFETY: `file` is an open descriptor this frame owns.
            let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if rc == 0 {
                return Ok(Some(HeldLock { file }));
            }
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            if std::time::Instant::now() >= deadline {
                return Err(format!(
                    "timed out waiting for claim lock {}",
                    lock_path.display()
                ));
            }
            std::thread::sleep(std::time::Duration::from_micros(200));
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (store, create);
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claim(path: &str, ts: u64) -> FileClaim {
        FileClaim {
            source: ClaimSource::Observed,
            session_id: Some(format!("session-{path}")),
            run_id: None,
            path: path.to_string(),
            worktree: "/worktree".into(),
            ts_ms: ts,
            cwd: Some("/worktree".into()),
        }
    }

    #[test]
    fn the_store_drops_expired_rows_without_sleeping_and_evicts_the_oldest_past_the_cap() {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("file-claims.jsonl");
        let ttl = 1_000;
        let cap = 2;
        append_claim(&store, &claim("a.rs", 1_000), cap, ttl, 1_000).unwrap();
        append_claim(&store, &claim("b.rs", 1_100), cap, ttl, 1_100).unwrap();
        append_claim(&store, &claim("c.rs", 1_200), cap, ttl, 1_200).unwrap();
        let live = read_store(&store, 1_500, ttl, cap).unwrap();
        let paths: Vec<_> = live.iter().map(|row| row.path.as_str()).collect();
        assert!(
            paths.len() <= cap && !paths.contains(&"a.rs") && paths.contains(&"c.rs"),
            "{paths:?}"
        );
        // The third append crosses the byte threshold (cap * 96), so the file
        // itself is rewritten. A read-side trim would hide a fast-path that
        // never evicted.
        let on_disk: Vec<String> = fs::read_to_string(&store)
            .unwrap()
            .lines()
            .filter(|line| !line.is_empty())
            .map(|line| {
                serde_json::from_str::<FileClaim>(line)
                    .unwrap()
                    .path
            })
            .collect();
        assert!(
            on_disk.len() <= cap && !on_disk.iter().any(|path| path == "a.rs") && on_disk.iter().any(|path| path == "c.rs"),
            "on disk {on_disk:?}"
        );
        let expired = read_store(&store, 1_200 + ttl + 1, ttl, cap).unwrap();
        assert!(expired.is_empty(), "{expired:?}");
    }

    #[test]
    fn a_missing_store_is_empty_and_an_unwritable_store_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope").join("file-claims.jsonl");
        assert!(read_store(&missing, 1, DEFAULT_TTL_MS, 4).unwrap().is_empty());
        let blocker = dir.path().join("not-a-directory");
        fs::write(&blocker, b"x").unwrap();
        let store = blocker.join("file-claims.jsonl");
        let error = append_claim(&store, &claim("a.rs", 1), 4, DEFAULT_TTL_MS, 1).unwrap_err();
        assert!(!error.is_empty(), "{error}");
    }

    #[test]
    fn one_append_on_an_empty_store_stays_well_under_the_hook_budget() {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("gitpulse").join("file-claims.jsonl");
        let started = std::time::Instant::now();
        append_claim(&store, &claim("a.rs", 1), DEFAULT_CAP, DEFAULT_TTL_MS, 1).unwrap();
        let elapsed = started.elapsed();
        assert!(
            elapsed.as_millis() < 50,
            "append took {elapsed:?}; the hook budget is 8s and the target is under 1ms"
        );
    }
}
