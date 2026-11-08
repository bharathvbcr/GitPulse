//! Bounded file claims for one repository.
//!
//! A claim says who intends to edit a path, or who the edit hook just let
//! through. It is not a git change: the file lives under
//! `<git-common-dir>/gitpulse/file-claims.jsonl`, beside the lane bus, and
//! nothing here updates a ref, an index, or a worktree file. There is no MCP
//! write. Declared claims are recorded when a run starts from a card's
//! planned files. Observed claims are recorded by the edit hook.
//!
//! # Store discipline
//!
//! * Every row is one `write(2)` of a buffer that already ends in `\n`, so a
//!   kill between "the row" and "its newline" cannot happen. If an earlier
//!   writer was cut off anyway (a full disk, a power loss), the next append
//!   first closes the torn line so it cannot swallow the new row.
//! * Readers take no lock. They skip a line that does not parse or is not
//!   terminated, count it, and report the count as degraded: one bad line is
//!   never a whole-store failure, and never reads as clean.
//! * Appends take a shared lock, so concurrent hooks do not queue behind one
//!   another. Only compaction takes the exclusive lock. It rewrites the file
//!   (dropping expired, duplicate and unreadable rows) through a temp file and
//!   an atomic rename, so a reader sees the old file or the new one.
//! * Compaction is rare by design. It starts when the file passes
//!   [`COMPACT_BYTES_PER_ROW`] bytes per row of cap, and keeps half the cap,
//!   so the next one is thousands of appends away.
//! * Nothing is fsynced. A claim is advisory and expires in minutes; a lost
//!   tail after a crash is the same as the claim never having been made, and
//!   a sync on every hook write is what put it over its latency budget.
//!
//! A missing file is an empty store. Any other read or write failure is an
//! error, so a caller can refuse to treat it as a clean result. Claims are
//! recorded and read only for a repository GitPulse trusts.

use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// How many live claims one repository keeps. Older rows are dropped first.
pub const DEFAULT_CAP: usize = 4096;
/// How long a claim still names a holder. Tests pass their own clock.
pub const DEFAULT_TTL_MS: u64 = 30 * 60 * 1000;
/// A store larger than this is read from its tail and compacted at the next
/// append. It is never read whole.
const MAX_STORE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_LINE_BYTES: usize = 2048;
const MAX_FIELD_BYTES: usize = 4096;
/// A real row is about 310 bytes. The file is compacted when it passes this
/// many bytes per row of cap, which leaves room for roughly 1.6 caps of rows
/// before a rewrite.
const COMPACT_BYTES_PER_ROW: u64 = 512;
/// How long a shared or exclusive claim lock may be waited for. The hook's
/// whole budget is eight seconds; this is deliberately not widened.
const LOCK_WAIT_MS: u64 = 20;

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

/// What a read found: the live rows, and how many lines it could not use.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StoreRead {
    pub rows: Vec<FileClaim>,
    /// Lines that were unparsable, unterminated or oversized, plus the cut
    /// head of a store read from its tail. Non-zero is degraded, not clean.
    pub skipped: usize,
    /// The repository is not trusted, so nothing was read and nothing may be
    /// said about claims there. Distinct from an empty store.
    pub untrusted: bool,
}

impl StoreRead {
    /// The degraded reading of this read, or `None` when every line was used.
    pub fn degraded(&self) -> Option<String> {
        (self.skipped > 0).then(|| {
            format!(
                "{} unreadable claim line{} skipped",
                self.skipped,
                if self.skipped == 1 { "" } else { "s" }
            )
        })
    }
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

/// Whether claims may be read or written for `worktree`.
///
/// `Ok(false)` is an untrusted repository: the hook must not create files
/// under its `.git`, and nothing is said about claims there. Any other trust
/// failure is an error, so it is never read as "untrusted, nothing to report".
pub fn admitted(worktree: &Path) -> Result<bool, String> {
    match crate::repository_trust::require(worktree) {
        Ok(()) => Ok(true),
        Err(error) if crate::repository_trust::refused(error.message()) => Ok(false),
        Err(error) => Err(error.into_message()),
    }
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
/// is empty, and so is an untrusted repository's. Anything else the store
/// could not do is an error.
pub fn read_for_worktree(worktree: &Path, now_ms: u64) -> Result<StoreRead, String> {
    if !admitted(worktree)? {
        return Ok(StoreRead {
            untrusted: true,
            ..StoreRead::default()
        });
    }
    let common = git_common(worktree)?;
    read_store(&store_path(&common), now_ms, DEFAULT_TTL_MS, DEFAULT_CAP)
}

/// Reads the store without taking a lock. A torn or foreign line is skipped
/// and counted; it never fails the read.
pub fn read_store(store: &Path, now_ms: u64, ttl_ms: u64, cap: usize) -> Result<StoreRead, String> {
    let loaded = load_lines(store)?;
    Ok(StoreRead {
        rows: trim(loaded.rows, now_ms, ttl_ms, cap),
        skipped: loaded.skipped,
        untrusted: false,
    })
}

/// Append one observed claim for an edit the hook is letting through. An
/// untrusted repository records nothing.
pub fn record_observed(
    worktree: &Path,
    session_id: &str,
    path: &str,
    cwd: &str,
    now_ms: u64,
) -> Result<(), String> {
    if !admitted(worktree)? {
        return Ok(());
    }
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

/// Declared claims for the paths a run said it would edit, in one locked
/// append. An empty list writes nothing.
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
    let mut batch = Vec::with_capacity(paths.len());
    for path in paths {
        batch.push(FileClaim {
            source: ClaimSource::Declared,
            session_id: None,
            run_id: Some(run_id.to_string()),
            path: normalize_rel(path)?,
            worktree: worktree.to_string(),
            ts_ms: now_ms,
            cwd: None,
        });
    }
    append_claims(
        &store_path(git_common_dir),
        &batch,
        DEFAULT_CAP,
        DEFAULT_TTL_MS,
        now_ms,
    )?;
    Ok(paths.len())
}

pub fn append_claim(
    store: &Path,
    claim: &FileClaim,
    cap: usize,
    ttl_ms: u64,
    now_ms: u64,
) -> Result<(), String> {
    append_claims(store, std::slice::from_ref(claim), cap, ttl_ms, now_ms)
}

/// Appends every claim in `claims` under one lock acquisition.
pub fn append_claims(
    store: &Path,
    claims: &[FileClaim],
    cap: usize,
    ttl_ms: u64,
    now_ms: u64,
) -> Result<(), String> {
    if claims.is_empty() {
        return Ok(());
    }
    let cap = cap.max(1);
    let mut buffer = String::new();
    for claim in claims {
        validate(claim)?;
        let line = serde_json::to_string(claim).map_err(|error| error.to_string())?;
        if line.len() > MAX_LINE_BYTES {
            return Err("a claim exceeds its size limit".into());
        }
        buffer.push_str(&line);
        buffer.push('\n');
    }
    let len = fs::metadata(store).map(|meta| meta.len()).unwrap_or(0);
    let threshold = (cap as u64).saturating_mul(COMPACT_BYTES_PER_ROW);
    if len < threshold && len < MAX_STORE_BYTES {
        return with_lock(store, Mode::Shared, || append_buffer(store, &buffer));
    }
    // Reading, parsing, trimming and rendering is most of a compaction, so it
    // is done before the exclusive lock is asked for. Appends made meanwhile
    // are the bytes past `loaded.end`; they are copied onto the new file under
    // the lock, which is then held only for a read of that tail, one write and
    // a rename. When several hooks cross the threshold together, the first to
    // get the lock compacts and the rest find a store that is already small:
    // they append to it instead of compacting again, so none of them holds the
    // lock for a second rewrite.
    let keep = (cap / 2).max(claims.len()).max(1);
    let prepared = prepare_compaction(store, claims, ttl_ms, now_ms, keep)?;
    with_lock(store, Mode::Exclusive, || {
        let len = fs::metadata(store).map(|meta| meta.len()).unwrap_or(0);
        let same_file = identity(store) == prepared.loaded.id && len >= prepared.loaded.end;
        if !same_file || len < threshold {
            return append_buffer(store, &buffer);
        }
        let mut out = prepared.text.into_bytes();
        out.extend(read_from(store, prepared.loaded.end)?);
        replace_store(store, &out)
    })
}

struct Prepared {
    loaded: Loaded,
    text: String,
}

/// The compacted store as it would be written now. Keeps `keep` rows, half the
/// cap: the next rewrite is then thousands of appends away instead of the very
/// next one.
fn prepare_compaction(
    store: &Path,
    claims: &[FileClaim],
    ttl_ms: u64,
    now_ms: u64,
    keep: usize,
) -> Result<Prepared, String> {
    let mut loaded = load_lines(store)?;
    let mut rows = std::mem::take(&mut loaded.rows);
    rows.extend(claims.iter().cloned());
    let mut text = String::new();
    for row in trim(rows, now_ms, ttl_ms, keep) {
        text.push_str(&serde_json::to_string(&row).map_err(|error| error.to_string())?);
        text.push('\n');
    }
    Ok(Prepared { loaded, text })
}

/// The bytes of the store from `offset` on: what was appended since a read
/// that stopped there.
fn read_from(store: &Path, offset: u64) -> Result<Vec<u8>, String> {
    let read_error =
        |error: std::io::Error| format!("cannot read claims {}: {error}", store.display());
    let mut file = fs::File::open(store).map_err(read_error)?;
    file.seek(SeekFrom::Start(offset)).map_err(read_error)?;
    let mut tail = Vec::new();
    file.take(MAX_STORE_BYTES)
        .read_to_end(&mut tail)
        .map_err(read_error)?;
    Ok(tail)
}

/// Which file is at `path`, so a compaction can tell its read was of the file
/// it is about to replace. `None` where the platform cannot say.
fn identity(path: &Path) -> Option<(u64, u64)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        fs::metadata(path).ok().map(|meta| (meta.dev(), meta.ino()))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

/// Live rows, oldest first: expired rows dropped, one row per holder and path
/// (the newest), then the newest `cap` kept.
fn trim(rows: Vec<FileClaim>, now_ms: u64, ttl_ms: u64, cap: usize) -> Vec<FileClaim> {
    let mut indexed: Vec<(usize, FileClaim)> = rows
        .into_iter()
        .enumerate()
        .filter(|(_, claim)| now_ms.saturating_sub(claim.ts_ms) <= ttl_ms)
        .collect();
    indexed.sort_by_key(|(index, claim)| (claim.ts_ms, *index));
    let mut seen = std::collections::HashSet::new();
    let mut newest_first: Vec<FileClaim> = Vec::new();
    for (_, claim) in indexed.into_iter().rev() {
        let holder = match claim.source {
            ClaimSource::Observed => claim.session_id.clone(),
            ClaimSource::Declared => claim.run_id.clone(),
        };
        let key = (
            claim.source as u8,
            holder.unwrap_or_default(),
            claim.worktree.clone(),
            claim.path.clone(),
        );
        if seen.insert(key) {
            newest_first.push(claim);
        }
    }
    newest_first.truncate(cap);
    newest_first.reverse();
    newest_first
}

/// Every parsable, terminated line of the store, and how many were skipped.
///
/// A store past [`MAX_STORE_BYTES`] is read from its tail; its cut head counts
/// as one skipped line.
fn load_lines(store: &Path) -> Result<Loaded, String> {
    let mut file = match fs::File::open(store) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Loaded::default());
        }
        Err(error) => {
            return Err(format!("cannot read claims {}: {error}", store.display()));
        }
    };
    let read_error =
        |error: std::io::Error| format!("cannot read claims {}: {error}", store.display());
    let len = file.metadata().map_err(read_error)?.len();
    let cut_head = len > MAX_STORE_BYTES;
    let start = if cut_head { len - MAX_STORE_BYTES } else { 0 };
    if cut_head {
        file.seek(SeekFrom::Start(start)).map_err(read_error)?;
    }
    let mut buf = Vec::new();
    file.take(MAX_STORE_BYTES)
        .read_to_end(&mut buf)
        .map_err(read_error)?;
    let (rows, skipped) = parse_lines(&buf, cut_head);
    Ok(Loaded {
        rows,
        skipped,
        end: start + buf.len() as u64,
        id: identity(store),
    })
}

/// What one read of the store found, and where it stopped.
#[derive(Default)]
struct Loaded {
    rows: Vec<FileClaim>,
    skipped: usize,
    /// The byte offset the read ended at.
    end: u64,
    /// The file that was read, when the platform can say which.
    id: Option<(u64, u64)>,
}

fn parse_lines(buf: &[u8], cut_head: bool) -> (Vec<FileClaim>, usize) {
    let terminated = buf.last().is_none_or(|last| *last == b'\n');
    let segments: Vec<&[u8]> = buf.split(|byte| *byte == b'\n').collect();
    let last = segments.len().saturating_sub(1);
    let mut rows = Vec::new();
    let mut skipped = 0usize;
    for (index, segment) in segments.into_iter().enumerate() {
        if segment.is_empty() {
            continue;
        }
        let torn_head = cut_head && index == 0;
        let unterminated = !terminated && index == last;
        if torn_head || unterminated || segment.len() > MAX_LINE_BYTES {
            skipped += 1;
            continue;
        }
        match serde_json::from_slice::<FileClaim>(segment) {
            Ok(claim) => rows.push(claim),
            Err(_) => skipped += 1,
        }
    }
    (rows, skipped)
}

/// Appends `buffer` (whole rows, each ending in `\n`) in one write. When the
/// file's last byte is not a newline, an earlier writer was cut off; the
/// buffer then starts with one so the torn line stands alone.
fn append_buffer(store: &Path, buffer: &str) -> Result<(), String> {
    let mut file = open_store(store)?;
    let append_error =
        |error: std::io::Error| format!("cannot append claim {}: {error}", store.display());
    let len = file.metadata().map_err(append_error)?.len();
    let mut torn = false;
    if len > 0 {
        let mut last = [0u8; 1];
        file.seek(SeekFrom::Start(len - 1)).map_err(append_error)?;
        file.read_exact(&mut last).map_err(append_error)?;
        torn = last[0] != b'\n';
    }
    let mut bytes = Vec::with_capacity(buffer.len() + 1);
    if torn {
        bytes.push(b'\n');
    }
    bytes.extend_from_slice(buffer.as_bytes());
    file.write_all(&bytes).map_err(append_error)
}

/// Replaces the store with `contents` through a temp file and a rename.
fn replace_store(store: &Path, contents: &[u8]) -> Result<(), String> {
    if let Some(parent) = store.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    }
    let tmp = store.with_extension("jsonl.tmp");
    {
        let mut options = OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&tmp)
            .map_err(|error| format!("cannot open claims {}: {error}", tmp.display()))?;
        file.write_all(contents)
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

/// The store, opened for append and created when missing.
fn open_store(store: &Path) -> Result<fs::File, String> {
    if let Some(parent) = store.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    }
    let mut options = OpenOptions::new();
    options.read(true).append(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(store)
        .map_err(|error| format!("cannot open claims {}: {error}", store.display()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// An append. Many may hold it at once.
    Shared,
    /// A compaction. Waits for appends to finish, and they for it.
    Exclusive,
}

fn with_lock<T>(
    store: &Path,
    mode: Mode,
    body: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    // The lock file orders processes. Compaction also takes this mutex so two
    // threads of one process do not both rewrite; appends do not, so they do
    // not queue behind each other. Where there is no file lock, the mutex is
    // the only ordering there is and every writer takes it.
    static LOCAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _local = (cfg!(not(unix)) || mode == Mode::Exclusive).then(|| {
        LOCAL
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    });
    let _file = acquire(store, mode)?;
    body()
}

struct HeldLock {
    #[cfg(unix)]
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

fn acquire(store: &Path, mode: Mode) -> Result<Option<HeldLock>, String> {
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        let lock_path = store.with_extension("lock");
        if let Some(parent) = lock_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
        }
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true);
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options
            .open(&lock_path)
            .map_err(|error| format!("cannot open claim lock {}: {error}", lock_path.display()))?;
        let operation = match mode {
            Mode::Shared => libc::LOCK_SH,
            Mode::Exclusive => libc::LOCK_EX,
        } | libc::LOCK_NB;
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(LOCK_WAIT_MS);
        loop {
            // SAFETY: `file` is an open descriptor this frame owns.
            let rc = unsafe { libc::flock(file.as_raw_fd(), operation) };
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
        let _ = (store, mode);
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

    fn on_disk_paths(store: &Path) -> Vec<String> {
        fs::read_to_string(store)
            .unwrap()
            .lines()
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_str::<FileClaim>(line).unwrap().path)
            .collect()
    }

    #[test]
    fn the_store_drops_expired_rows_without_sleeping_and_evicts_the_oldest_past_the_cap() {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("file-claims.jsonl");
        let ttl = 1_000;
        let cap = 2;
        // Enough rows to cross the compaction threshold (cap * 512 bytes), so
        // the file itself is rewritten. A read-side trim alone would hide an
        // append path that never evicted.
        for index in 0..12u64 {
            let name = format!("{index:02}.rs");
            append_claim(
                &store,
                &claim(&name, 1_000 + index),
                cap,
                ttl,
                1_000 + index,
            )
            .unwrap();
        }
        let live = read_store(&store, 1_100, ttl, cap).unwrap();
        let paths: Vec<_> = live.rows.iter().map(|row| row.path.as_str()).collect();
        assert!(
            paths.len() <= cap && !paths.contains(&"00.rs") && paths.contains(&"11.rs"),
            "{paths:?}"
        );
        let on_disk = on_disk_paths(&store);
        assert!(
            on_disk.len() < 12 && !on_disk.iter().any(|path| path == "00.rs"),
            "the file was never rewritten: on disk {on_disk:?}"
        );
        assert!(on_disk.iter().any(|path| path == "11.rs"), "{on_disk:?}");
        let expired = read_store(&store, 1_011 + ttl + 1, ttl, cap).unwrap();
        assert!(expired.rows.is_empty(), "{expired:?}");
    }

    #[test]
    fn a_missing_store_is_empty_and_an_unwritable_store_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope").join("file-claims.jsonl");
        let read = read_store(&missing, 1, DEFAULT_TTL_MS, 4).unwrap();
        assert!(read.rows.is_empty() && read.skipped == 0, "{read:?}");
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

    /// C1. A row is one buffer ending in a newline: the previous implementation
    /// wrote the row and its newline separately, so a writer killed between
    /// them left a line with no terminator.
    #[test]
    fn a_row_is_written_whole_and_ends_in_a_newline() {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("file-claims.jsonl");
        append_claim(&store, &claim("a.rs", 1), 8, DEFAULT_TTL_MS, 1).unwrap();
        append_claim(&store, &claim("b.rs", 2), 8, DEFAULT_TTL_MS, 2).unwrap();
        let text = fs::read_to_string(&store).unwrap();
        assert!(
            text.ends_with('\n') && text.lines().count() == 2,
            "{text:?}"
        );
    }

    /// C1. One torn line used to fail every later read and every compacting
    /// append for good, because only a successful rewrite could remove it and
    /// the rewrite refused to read the store.
    #[test]
    fn a_torn_line_is_skipped_and_counted_and_every_good_row_survives_the_appends_after_it() {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("gitpulse").join("file-claims.jsonl");
        let now = 1_000_000u64;
        append_claim(
            &store,
            &claim("a.rs", now),
            DEFAULT_CAP,
            DEFAULT_TTL_MS,
            now,
        )
        .unwrap();
        // A writer cut off mid-row: no newline, and not JSON.
        let torn = serde_json::to_string(&claim("torn.rs", now)).unwrap();
        let mut file = OpenOptions::new().append(true).open(&store).unwrap();
        file.write_all(&torn.as_bytes()[..torn.len() / 2]).unwrap();
        drop(file);
        append_claim(
            &store,
            &claim("b.rs", now + 1),
            DEFAULT_CAP,
            DEFAULT_TTL_MS,
            now + 1,
        )
        .unwrap();
        append_claim(
            &store,
            &claim("c.rs", now + 2),
            DEFAULT_CAP,
            DEFAULT_TTL_MS,
            now + 2,
        )
        .unwrap();

        let read = read_store(&store, now + 3, DEFAULT_TTL_MS, DEFAULT_CAP).unwrap();
        let paths: Vec<_> = read.rows.iter().map(|row| row.path.as_str()).collect();
        assert_eq!(paths, ["a.rs", "b.rs", "c.rs"], "{read:?}");
        assert_eq!(read.skipped, 1, "the torn line is counted, not hidden");
        assert!(
            read.degraded()
                .is_some_and(|text| text.contains("1 unreadable")),
            "{read:?}"
        );
        // Long after every claim expired the store still reads, and does not
        // fail the way the whole-store error did.
        let later = read_store(
            &store,
            now + DEFAULT_TTL_MS * 10,
            DEFAULT_TTL_MS,
            DEFAULT_CAP,
        );
        assert!(later.is_ok(), "{later:?}");
    }

    /// C1. A torn tail is closed by the next append instead of swallowing it,
    /// and a final line with no newline is never trusted even if it parses.
    #[test]
    fn an_unterminated_final_line_is_skipped_even_when_it_parses() {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("file-claims.jsonl");
        let whole = serde_json::to_string(&claim("a.rs", 5)).unwrap();
        fs::write(&store, whole).unwrap();
        let read = read_store(&store, 6, DEFAULT_TTL_MS, 8).unwrap();
        assert!(read.rows.is_empty() && read.skipped == 1, "{read:?}");
    }

    /// C1. The next compaction drops every bad line, so the store heals.
    #[test]
    fn a_compaction_heals_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("file-claims.jsonl");
        let cap = 2;
        append_claim(&store, &claim("a.rs", 1), cap, DEFAULT_TTL_MS, 1).unwrap();
        let mut file = OpenOptions::new().append(true).open(&store).unwrap();
        file.write_all(b"{\"source\":\"observ").unwrap();
        file.write_all(b"\n\xff\xfe not utf-8 \n").unwrap();
        drop(file);
        assert_eq!(
            read_store(&store, 2, DEFAULT_TTL_MS, cap).unwrap().skipped,
            2
        );
        for index in 0..12u64 {
            let name = format!("{index:02}.rs");
            append_claim(
                &store,
                &claim(&name, 10 + index),
                cap,
                DEFAULT_TTL_MS,
                10 + index,
            )
            .unwrap();
        }
        let healed = read_store(&store, 30, DEFAULT_TTL_MS, cap).unwrap();
        assert_eq!(healed.skipped, 0, "{healed:?}");
        assert!(!healed.rows.is_empty(), "{healed:?}");
        let text = fs::read(&store).unwrap();
        assert!(
            std::str::from_utf8(&text).is_ok() && !text.windows(6).any(|w| w == b"observ\n"),
            "the bad lines were rewritten away"
        );
    }

    /// M6. One holder editing one file many times is one row, not a flood of
    /// rows that pushes everyone else out of the cap.
    #[test]
    fn repeated_edits_by_one_session_to_one_path_keep_only_the_newest_row() {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("file-claims.jsonl");
        for ts in 1..=50u64 {
            append_claim(&store, &claim("a.rs", ts), DEFAULT_CAP, DEFAULT_TTL_MS, ts).unwrap();
        }
        append_claim(&store, &claim("b.rs", 60), DEFAULT_CAP, DEFAULT_TTL_MS, 60).unwrap();
        let read = read_store(&store, 61, DEFAULT_TTL_MS, DEFAULT_CAP).unwrap();
        let rows: Vec<_> = read
            .rows
            .iter()
            .map(|r| (r.path.as_str(), r.ts_ms))
            .collect();
        assert_eq!(rows, [("a.rs", 50), ("b.rs", 60)]);
    }

    /// M1. Readers take no lock: an exclusive holder (a compaction in
    /// progress) does not make a read fail.
    #[cfg(unix)]
    #[test]
    fn a_read_does_not_wait_for_the_claim_lock() {
        use std::os::unix::io::AsRawFd;
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("gitpulse").join("file-claims.jsonl");
        append_claim(&store, &claim("a.rs", 1), DEFAULT_CAP, DEFAULT_TTL_MS, 1).unwrap();
        let holder = OpenOptions::new()
            .read(true)
            .write(true)
            .open(store.with_extension("lock"))
            .unwrap();
        // SAFETY: `holder` is an open descriptor owned by this test.
        assert_eq!(unsafe { libc::flock(holder.as_raw_fd(), libc::LOCK_EX) }, 0);
        let started = std::time::Instant::now();
        let read = read_store(&store, 2, DEFAULT_TTL_MS, DEFAULT_CAP);
        let elapsed = started.elapsed();
        // SAFETY: as above.
        unsafe { libc::flock(holder.as_raw_fd(), libc::LOCK_UN) };
        assert_eq!(read.unwrap().rows.len(), 1);
        assert!(elapsed.as_millis() < LOCK_WAIT_MS as u128, "{elapsed:?}");
    }

    /// M1. Appends share the lock: threads appending together all succeed,
    /// across a compaction in the middle of them.
    #[test]
    fn concurrent_appends_at_the_compaction_threshold_all_succeed() {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("gitpulse").join("file-claims.jsonl");
        let cap = 64;
        let now = 1_000_000u64;
        // Fill to just under the threshold so the first appends are cheap and a
        // compaction lands inside the run.
        let mut index = 0u64;
        while fs::metadata(&store).map(|m| m.len()).unwrap_or(0) + 400
            < cap as u64 * COMPACT_BYTES_PER_ROW
        {
            let name = format!("src/fill_{index:05}.rs");
            append_claim(
                &store,
                &claim(&name, now + index),
                cap,
                DEFAULT_TTL_MS,
                now + index,
            )
            .unwrap();
            index += 1;
        }
        let failures = std::sync::atomic::AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for thread in 0..6u64 {
                let (store, failures) = (&store, &failures);
                scope.spawn(move || {
                    for k in 0..40u64 {
                        let name = format!("src/t{thread}_{k:03}.rs");
                        let ts = now + 10_000 + thread * 100 + k;
                        if append_claim(store, &claim(&name, ts), cap, DEFAULT_TTL_MS, ts).is_err()
                        {
                            failures.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        }
                    }
                });
            }
        });
        assert_eq!(failures.load(std::sync::atomic::Ordering::SeqCst), 0);
        let read = read_store(&store, now + 20_000, DEFAULT_TTL_MS, cap).unwrap();
        assert_eq!(read.skipped, 0, "{read:?}");
    }

    /// Security. The edit hook runs in every repository an agent edits, trusted
    /// or not. An untrusted one gets no `.git/gitpulse` directory, no lock file,
    /// no row, and a read says it was not looked at rather than "no claims".
    #[test]
    fn an_untrusted_repository_is_neither_written_nor_read() {
        let untrusted = tempfile::tempdir().unwrap();
        crate::test_support::git_in(untrusted.path(), &["--no-pager", "init", "-q"]);
        let read = read_for_worktree(untrusted.path(), 1).unwrap();
        assert!(read.untrusted && read.rows.is_empty(), "{read:?}");
        record_observed(untrusted.path(), "session-a", "a.rs", "/x", 1).unwrap();
        assert!(
            !untrusted.path().join(".git").join("gitpulse").exists(),
            "an untrusted repository's .git was written to"
        );

        let trusted = crate::test_support::git_repo();
        record_observed(trusted.path(), "session-a", "a.rs", "/x", now_ms()).unwrap();
        let read = read_for_worktree(trusted.path(), now_ms()).unwrap();
        assert!(!read.untrusted && read.rows.len() == 1, "{read:?}");
    }

    /// M1. A full-size store compacts rarely: a compaction keeps half the cap,
    /// so the appends after it do not rewrite the file again.
    #[test]
    fn a_compaction_leaves_room_so_the_next_appends_do_not_rewrite() {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("file-claims.jsonl");
        let cap = 32;
        let mut rewrites = 0;
        let mut previous = 0u64;
        for index in 0..400u64 {
            let name = format!("src/some/longer/path/to/a/module_{index:04}.rs");
            append_claim(
                &store,
                &claim(&name, 1_000 + index),
                cap,
                DEFAULT_TTL_MS,
                1_000 + index,
            )
            .unwrap();
            let len = fs::metadata(&store).unwrap().len();
            if len < previous {
                rewrites += 1;
            }
            previous = len;
        }
        // 400 appends of ~200 byte rows into a 16 KiB threshold: a handful of
        // rewrites, not one per append.
        assert!(
            rewrites > 0 && rewrites < 40,
            "{rewrites} rewrites in 400 appends"
        );
    }
}
