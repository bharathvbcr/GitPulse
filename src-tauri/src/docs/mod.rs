//! Repository doc vault: MarkDev `Vault` fed from `git ls-files`, not a walk.
//!
//! Git is the authority on which `.md` files belong in the repo (tracked,
//! non-ignored). Notes are parsed with `Note::parse` and indexed through
//! `Vault::build`. Rename uses the pure `rewrite_links_in` helper plus
//! GitPulse's git writer — never MarkDev's `rename_note`.
//!
//! A refresh is incremental: it re-lists only when the Git index changed and
//! re-reads only files whose stat key changed, and it builds outside the lock
//! queries read through, so they keep the previous snapshot until the new one
//! is published.

mod rename;

use crate::engine::git_cli::{
    git_text, resolve_git_dir, sandbox_join, sandbox_join_canonical, validate_repo,
};
use markdev_vault::note::{has_markdown_extension, MARKDOWN_EXTENSIONS};
use markdev_vault::{Backlink, Graph, GraphQuery, Note, SearchHit, Vault, DEFAULT_MAX_NOTE_BYTES};
pub use rename::{rename_doc, DocRenameFailure, DocRenameOutcome, DocRenamePlan};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

/// Soft ceiling on markdown files held in one vault. Personal / project docs
/// sit far below this; a monorepo that vendors thousands of READMEs degrades
/// to a partial index rather than OOM.
const MAX_DOC_FILES: usize = 5_000;
const MAX_DOC_BYTES: usize = 32 * 1024 * 1024;
const MAX_CACHED_VAULTS: usize = 8;

/// [`MAX_DOC_FILES`], except that a test can lower it for its own thread.
fn max_doc_files() -> usize {
    #[cfg(test)]
    {
        probe::max_doc_files().unwrap_or(MAX_DOC_FILES)
    }
    #[cfg(not(test))]
    {
        MAX_DOC_FILES
    }
}

/// Default search hit limit (also the UI honesty threshold).
pub const DEFAULT_SEARCH_LIMIT: usize = 50;

/// A file whose ctime lies this close to the scan clock may still be written
/// again within the same timestamp tick (coarse kernel clocks, filesystems
/// with 1–2 s timestamps), leaving its stat key unchanged. Such a key is not
/// trusted to stand for the bytes until a later scan sees it settled.
const RACY_WINDOW: Duration = Duration::from_secs(2);

/// Cached vault plus the build-time honesty fields (`truncated`, skips).
/// `status()` must not invent `truncated: false` after a capped build.
struct CachedVault {
    vault: Vault,
    status: DocsStatus,
    /// What the next build may skip. Only the builder holding this
    /// repository's build lock touches it; queries never do.
    memo: Mutex<ScanMemo>,
}

/// What a build learned about the files behind its snapshot.
#[derive(Default)]
struct ScanMemo {
    /// Settled stat key per tracked path whose bytes this snapshot holds as a
    /// note, or found not to be UTF-8.
    files: HashMap<String, Seen>,
    /// The Git index this snapshot's listing was read from, once settled.
    listing: Option<Listing>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Seen {
    Note(StatKey),
    NotUtf8(StatKey),
}

struct Listing {
    index: PathBuf,
    key: StatKey,
    paths: Arc<[String]>,
}

/// One version of one file as `stat` reports it. A write moves `ctime`, and
/// nothing outside the kernel can set it back, so a restored mtime or an
/// in-place same-size edit still changes the key.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct StatKey {
    dev: u64,
    ino: u64,
    size: u64,
    mtime: (i64, i64),
    ctime: (i64, i64),
}

impl StatKey {
    #[cfg(unix)]
    fn of(meta: &std::fs::Metadata) -> Option<StatKey> {
        use std::os::unix::fs::MetadataExt;
        Some(StatKey {
            dev: meta.dev(),
            ino: meta.ino(),
            size: meta.size(),
            mtime: (meta.mtime(), meta.mtime_nsec()),
            ctime: (meta.ctime(), meta.ctime_nsec()),
        })
    }

    /// No ctime here, so nothing is skipped: every file is read and compared.
    #[cfg(not(unix))]
    fn of(_meta: &std::fs::Metadata) -> Option<StatKey> {
        None
    }

    /// Whether a later write must produce a different key: the change time
    /// is a full [`RACY_WINDOW`] before this scan began.
    fn settled(&self, scan_started: SystemTime) -> bool {
        let (secs, nanos) = self.ctime;
        let (Ok(secs), Ok(nanos)) = (u64::try_from(secs), u32::try_from(nanos)) else {
            return false;
        };
        if nanos >= 1_000_000_000 {
            return false;
        }
        let changed = SystemTime::UNIX_EPOCH + Duration::new(secs, nanos);
        changed
            .checked_add(RACY_WINDOW)
            .is_some_and(|settled_at| settled_at < scan_started)
    }
}

/// The clock a scan judges settledness against.
fn scan_clock() -> SystemTime {
    #[cfg(test)]
    {
        SystemTime::now() + probe::clock_ahead()
    }
    #[cfg(not(test))]
    {
        SystemTime::now()
    }
}

/// One repository's published snapshot and the lock its builds take.
///
/// Lock order: `build`, then `published` — never the reverse. `published` is
/// held only to clone or swap the `Arc`, so a query never waits on a build
/// while any snapshot exists.
#[derive(Default)]
struct VaultSlot {
    published: Mutex<Option<Arc<CachedVault>>>,
    /// The build lock. Its value is [`REFRESH_TICKETS`] as read just before
    /// the scan that produced `published` read any file: a forced refresh whose
    /// ticket is `<=` it was requested before that scan began, so the scan
    /// already reflects it.
    build: Mutex<u64>,
}

/// Hands out a ticket per load, in request order.
static REFRESH_TICKETS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

impl VaultSlot {
    fn current(&self) -> Option<Arc<CachedVault>> {
        self.published
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn publish(&self, value: &Arc<CachedVault>) {
        *self
            .published
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::clone(value));
    }
}

struct CacheEntry {
    slot: Arc<VaultSlot>,
    used_at: Instant,
}

fn vault_cache() -> &'static Mutex<HashMap<PathBuf, CacheEntry>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, CacheEntry>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// A detached/evicted slot can finish for its existing callers but cannot
/// republish itself into the cache after invalidation. Work for a resident
/// repository shares one slot instead of racing duplicate cold builds.
fn vault_slot(repo: &Path) -> Arc<VaultSlot> {
    let mut cache = vault_cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(entry) = cache.get_mut(repo) {
        entry.used_at = Instant::now();
        return Arc::clone(&entry.slot);
    }
    if cache.len() >= MAX_CACHED_VAULTS {
        let oldest = cache
            .iter()
            .min_by_key(|(_, entry)| entry.used_at)
            .map(|(path, _)| path.clone());
        if let Some(path) = oldest {
            cache.remove(&path);
        }
    }
    let slot = Arc::new(VaultSlot::default());
    cache.insert(
        repo.to_path_buf(),
        CacheEntry {
            slot: Arc::clone(&slot),
            used_at: Instant::now(),
        },
    );
    slot
}

/// The published snapshot, or a fresh one when `force` or nothing is cached.
///
/// A build runs under the slot's build lock only: concurrent queries keep
/// answering from the previous snapshot until the new one is swapped in with
/// one write. Forced refreshes queue behind each other, and none returns a
/// snapshot older than its request: each is satisfied by the first scan that
/// began after it was made. Callers that overlap therefore share one scan
/// instead of each running its own after the last, because a scan reads the
/// files after every request that queued before it started. A failed build
/// publishes nothing.
fn load_vault(repo_path: &str, force: bool) -> Result<Arc<CachedVault>, String> {
    use std::sync::atomic::Ordering;
    let repo = validate_repo(repo_path)?;
    let slot = vault_slot(&repo);
    if !force {
        if let Some(value) = slot.current() {
            return Ok(value);
        }
    }
    // Drawn before waiting on the build lock, so a refresh queued behind a
    // running scan can tell whether a scan that began after its request has
    // finished by the time it gets the lock.
    let ticket = REFRESH_TICKETS.fetch_add(1, Ordering::SeqCst) + 1;
    let mut scan_began = slot
        .build
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let previous = slot.current();
    if let Some(value) = previous.as_ref() {
        // A cold query that queued behind another cold build, or a forced
        // refresh requested before the scan that published this snapshot began.
        if !force || *scan_began >= ticket {
            return Ok(Arc::clone(value));
        }
    }
    let began = REFRESH_TICKETS.load(Ordering::SeqCst);
    let value = build_vault(&repo, previous.as_ref())?;
    slot.publish(&value);
    *scan_began = began;
    Ok(value)
}

/// One broken link: source note path and the unresolved target as written.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BrokenLink {
    pub source: String,
    pub target: String,
}

/// Summary of the vault currently cached for a repo.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DocsStatus {
    pub note_count: u32,
    pub truncated: bool,
    pub skipped_oversized: u32,
    pub skipped_unreadable: u32,
    /// Markdown files the scan did not cover. From `status`/`refresh` that is
    /// the tracked files the vault does not hold, whatever the reason: past
    /// the file or byte cap, too large, unreadable, not UTF-8. In a rename's
    /// `scan` it also counts the untracked, non-ignored markdown files, which
    /// the vault never lists. Anything that searched or rewrote across the
    /// vault did not see these.
    pub not_scanned: u32,
}

/// Builds (or rebuilds) the vault for `repo_path` from tracked markdown files.
pub fn refresh(repo_path: &str) -> Result<DocsStatus, String> {
    Ok(load_vault(repo_path, true)?.status.clone())
}

/// Drops a cached vault so the next query rebuilds.
pub fn invalidate(repo_path: &str) {
    let Ok(repo) = validate_repo(repo_path) else {
        return;
    };
    let mut cache = vault_cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    cache.remove(&repo);
}

fn with_vault<T>(repo_path: &str, f: impl FnOnce(&Vault) -> T) -> Result<T, String> {
    let cached = load_vault(repo_path, false)?;
    Ok(f(&cached.vault))
}

/// Repo-wide markdown full-text search.
pub fn search(repo_path: &str, query: &str, limit: usize) -> Result<Vec<SearchHit>, String> {
    let limit = limit.clamp(1, 200);
    with_vault(repo_path, |vault| vault.search(query, limit))
}

/// Every unresolved note link in the vault.
pub fn broken_links(repo_path: &str) -> Result<Vec<BrokenLink>, String> {
    with_vault(repo_path, |vault| {
        vault
            .broken_links()
            .into_iter()
            .map(|(source, target)| BrokenLink { source, target })
            .collect()
    })
}

/// Notes that link at `path`.
pub fn backlinks(repo_path: &str, path: &str) -> Result<Vec<Backlink>, String> {
    with_vault(repo_path, |vault| vault.backlinks(path))
}

/// Force-directed doc graph payload (coords in a 1000×1000 extent).
pub fn graph(
    repo_path: &str,
    focus: Option<&str>,
    depth: u32,
    tag: Option<&str>,
    folder: Option<&str>,
) -> Result<Graph, String> {
    with_vault(repo_path, |vault| {
        let query = GraphQuery {
            focus,
            depth,
            tag,
            folder,
        };
        Graph::build(vault, &query)
    })
}

pub fn status(repo_path: &str) -> Result<DocsStatus, String> {
    // Preserve the actual build's partial/skipped status on a cache hit.
    Ok(load_vault(repo_path, false)?.status.clone())
}

fn normalize_rel(path: &str) -> Result<String, String> {
    let trimmed = path.trim().replace('\\', "/");
    if trimmed.is_empty() || trimmed.contains('\0') || trimmed.starts_with('/') {
        return Err("path must be a non-empty repository-relative path".into());
    }
    if trimmed
        .split('/')
        .any(|c| c.is_empty() || c == "." || c == "..")
    {
        return Err("path must not contain '.' or '..' components".into());
    }
    Ok(trimmed)
}

/// Builds the vault for `repo`, reusing what `previous` already holds.
///
/// The listing is reused while the Git index's stat key is unchanged, so an
/// unchanged refresh spawns nothing. Each tracked path is `stat`ed; a file
/// whose settled key matches the one recorded with `previous` keeps its note
/// without being opened, and every other file is read and compared byte for
/// byte. Byte budget, caps and skip counts are applied identically on both
/// paths, so the result equals a build with no `previous`. When nothing
/// changed, `previous` itself is returned and its memo updated in place.
fn build_vault(
    repo: &Path,
    previous: Option<&Arc<CachedVault>>,
) -> Result<Arc<CachedVault>, String> {
    let started = Instant::now();
    let scan_started = scan_clock();
    let previous = previous.filter(|cached| cached.vault.root() == repo);
    // Held for the whole build: only the builder reads or writes a memo.
    let mut known = previous.map(|cached| {
        cached
            .memo
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    });
    let mut next = ScanMemo::default();

    let index = index_key(repo);
    let reused_listing = known
        .as_deref()
        .and_then(|memo| memo.listing.as_ref())
        .filter(|listing| {
            index
                .as_ref()
                .is_some_and(|(path, key)| listing.index == *path && listing.key == *key)
        })
        .map(|listing| Arc::clone(&listing.paths));
    let listing_reused = reused_listing.is_some();
    let paths: Arc<[String]> = match reused_listing {
        Some(paths) => paths,
        None => list_markdown_paths(repo)?.into(),
    };
    next.listing = index
        .filter(|(_, key)| key.settled(scan_started))
        .map(|(index, key)| Listing {
            index,
            key,
            paths: Arc::clone(&paths),
        });
    #[cfg(test)]
    probe::note_scan(repo);
    #[cfg(test)]
    probe::pause_point(repo);
    let cap = max_doc_files();
    let mut truncated = paths.len() > cap;
    let mut remaining = MAX_DOC_BYTES;
    let take = paths.len().min(cap);

    let mut notes = Vec::with_capacity(take);
    let mut skipped_oversized = 0u32;
    let mut skipped_unreadable = 0u32;
    let mut parsed = 0usize;
    let mut read = 0usize;

    let mut locator = Locator::new(repo);
    for rel in paths.iter().take(take) {
        let (abs, stat) = match locator.locate(rel) {
            Some((abs, stat)) if stat.is_file() => (abs, stat),
            _ => {
                skipped_unreadable = skipped_unreadable.saturating_add(1);
                continue;
            }
        };
        if stat.len() > DEFAULT_MAX_NOTE_BYTES as u64 {
            skipped_oversized = skipped_oversized.saturating_add(1);
            continue;
        }
        if stat.len() > remaining as u64 {
            truncated = true;
            continue;
        }
        if let Some(key) = StatKey::of(&stat) {
            let seen = known
                .as_deref()
                .and_then(|memo| memo.files.get(rel.as_str()))
                .copied();
            match seen {
                Some(Seen::Note(seen)) if seen == key => {
                    if let Some(note) = previous.and_then(|cached| cached.vault.note(rel)) {
                        remaining -= note.text.len();
                        notes.push(Cow::Borrowed(note));
                        next.files.insert(rel.clone(), Seen::Note(key));
                        continue;
                    }
                }
                Some(Seen::NotUtf8(seen)) if seen == key => {
                    skipped_unreadable = skipped_unreadable.saturating_add(1);
                    next.files.insert(rel.clone(), Seen::NotUtf8(key));
                    continue;
                }
                _ => {}
            }
        }
        read += 1;
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            #[cfg(target_os = "macos")]
            let nofollow = libc::O_NOFOLLOW_ANY;
            #[cfg(not(target_os = "macos"))]
            let nofollow = libc::O_NOFOLLOW;
            options.custom_flags(nofollow | libc::O_NONBLOCK);
        }
        #[cfg(test)]
        probe::note_read();
        let file = match options.open(&abs) {
            Ok(file) => file,
            Err(_) => {
                skipped_unreadable = skipped_unreadable.saturating_add(1);
                continue;
            }
        };
        let meta = match file.metadata() {
            Ok(meta) if meta.is_file() => meta,
            _ => {
                skipped_unreadable = skipped_unreadable.saturating_add(1);
                continue;
            }
        };
        if meta.len() > DEFAULT_MAX_NOTE_BYTES as u64 {
            skipped_oversized = skipped_oversized.saturating_add(1);
            continue;
        }
        if meta.len() > remaining as u64 {
            truncated = true;
            continue;
        }
        // Metadata can race a growing file; bound the actual read as well.
        let limit = DEFAULT_MAX_NOTE_BYTES.min(remaining);
        let mut bytes = Vec::new();
        if file.take(limit as u64 + 1).read_to_end(&mut bytes).is_err() {
            skipped_unreadable = skipped_unreadable.saturating_add(1);
            continue;
        }
        if bytes.len() > DEFAULT_MAX_NOTE_BYTES {
            skipped_oversized = skipped_oversized.saturating_add(1);
            continue;
        }
        if bytes.len() > remaining {
            truncated = true;
            continue;
        }
        // The key the bytes just read were opened under, recorded only when
        // settled and of the same length. A write racing the read moves
        // ctime, so the next scan sees another key and reads again.
        let read_key = StatKey::of(&meta)
            .filter(|key| key.settled(scan_started) && key.size == bytes.len() as u64);
        let text = match String::from_utf8(bytes) {
            Ok(text) => text,
            Err(_) => {
                skipped_unreadable = skipped_unreadable.saturating_add(1);
                if let Some(key) = read_key {
                    next.files.insert(rel.clone(), Seen::NotUtf8(key));
                }
                continue;
            }
        };
        remaining -= text.len();
        if let Some(key) = read_key {
            next.files.insert(rel.clone(), Seen::Note(key));
        }
        // Compare exact bytes: a stat key read inside the racy window is not
        // trusted. Borrow unchanged notes until we know whether the entire
        // indexed snapshot can survive.
        if let Some(note) = previous
            .and_then(|cached| cached.vault.note(rel))
            .filter(|note| note.text == text)
        {
            notes.push(Cow::Borrowed(note));
        } else {
            parsed += 1;
            notes.push(Cow::Owned(Note::parse(rel.clone(), &text)));
        }
    }

    let status = DocsStatus {
        note_count: notes.len() as u32,
        truncated,
        skipped_oversized,
        skipped_unreadable,
        not_scanned: (paths.len() - notes.len()) as u32,
    };
    let unchanged = previous.is_some_and(|cached| {
        parsed == 0 && cached.vault.notes().len() == notes.len() && cached.status == status
    });
    let reused = notes.len() - parsed;
    let value = match previous.filter(|_| unchanged) {
        Some(cached) => {
            if let Some(memo) = known.as_deref_mut() {
                *memo = next;
            }
            Arc::clone(cached)
        }
        None => Arc::new(CachedVault {
            vault: Vault::build(
                repo.to_path_buf(),
                notes.into_iter().map(Cow::into_owned).collect(),
            ),
            status,
            memo: Mutex::new(next),
        }),
    };
    log::debug!(target: "performance", "docs_refresh admitted={} listing_reused={} read={} parsed={} reused={} index_reused={} work_ms={}",
        value.status.note_count, listing_reused, read, parsed, reused,
        unchanged, started.elapsed().as_millis());
    Ok(value)
}

/// [`sandbox_join_canonical`] for every path of one listing. Each directory is
/// resolved, and its containment checked, once per build; a final component
/// that is itself a link takes the full walk. Thousands of notes in a few
/// dozen directories then cost one `lstat` each instead of a `canonicalize`
/// per path component.
struct Locator<'a> {
    repo: &'a Path,
    root: Option<PathBuf>,
    dirs: HashMap<String, Option<PathBuf>>,
}

impl<'a> Locator<'a> {
    fn new(repo: &'a Path) -> Self {
        Locator {
            repo,
            root: repo.canonicalize().ok(),
            dirs: HashMap::new(),
        }
    }

    /// The canonical path of tracked `rel` and its `lstat`, or `None` when it
    /// escapes the repository, is missing or cannot be resolved.
    fn locate(&mut self, rel: &str) -> Option<(PathBuf, std::fs::Metadata)> {
        sandbox_join(self.repo, rel).ok()?;
        let (parent, name) = match rel.rsplit_once('/') {
            Some((dir, name)) => {
                let repo = self.repo;
                let parent = self
                    .dirs
                    .entry(dir.to_string())
                    .or_insert_with(|| sandbox_join_canonical(repo, dir).ok())
                    .clone()?;
                (parent, name)
            }
            None => (self.root.clone()?, rel),
        };
        let candidate = parent.join(name);
        let meta = std::fs::symlink_metadata(&candidate).ok()?;
        if !meta.file_type().is_symlink() {
            return Some((candidate, meta));
        }
        let abs = sandbox_join_canonical(self.repo, rel).ok()?;
        let meta = std::fs::symlink_metadata(&abs).ok()?;
        Some((abs, meta))
    }
}

/// The Git index file and its stat key, when the listing can be tied to it.
/// `GIT_INDEX_FILE` points Git elsewhere, so it disables the shortcut.
fn index_key(repo: &Path) -> Option<(PathBuf, StatKey)> {
    if std::env::var_os("GIT_INDEX_FILE").is_some() {
        return None;
    }
    let index = resolve_git_dir(repo).ok()?.join("index");
    let meta = std::fs::symlink_metadata(&index).ok()?;
    if !meta.is_file() {
        return None;
    }
    StatKey::of(&meta).map(|key| (index, key))
}

/// Tracked markdown paths from `git ls-files`, never a filesystem walk.
fn list_markdown_paths(repo: &Path) -> Result<Vec<String>, String> {
    #[cfg(test)]
    probe::note_listing();
    ls_files_markdown(repo, &[])
}

/// Markdown files git does not track and does not ignore. The vault never
/// holds them, so anything that searches or rewrites across it did not see
/// them. Listed the same way as the tracked set (`git ls-files`, same
/// extension patterns). Only the output stream is bounded, by `git_text`'s cap,
/// which fails the call when exceeded; unlike the tracked listing there is no
/// file-count ceiling, since the result is only counted.
fn list_untracked_markdown_paths(repo: &Path) -> Result<Vec<String>, String> {
    ls_files_markdown(repo, &["--others", "--exclude-standard"])
}

fn ls_files_markdown(repo: &Path, selection: &[&str]) -> Result<Vec<String>, String> {
    let patterns: Vec<String> = MARKDOWN_EXTENSIONS
        .iter()
        .map(|extension| format!(":(icase)*.{extension}"))
        .collect();
    let args: Vec<&str> = ["-c", "core.quotepath=off", "ls-files", "-z"]
        .into_iter()
        .chain(selection.iter().copied())
        .chain(["--"])
        .chain(patterns.iter().map(String::as_str))
        .collect();
    let stdout = git_text(repo, &args)?;
    let mut paths: Vec<String> = stdout
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(|s| s.replace('\\', "/"))
        .filter(|s| has_markdown_extension(s))
        .collect();
    paths.sort();
    paths.dedup();
    Ok(paths)
}

/// Test-only observation points: what a build read and spawned on this thread,
/// a clock the racy-entry rule can be moved past, and a pause after the
/// listing so a test can hold one build open while it queries.
#[cfg(test)]
mod probe {
    use std::cell::Cell;
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
    use std::sync::{Mutex, OnceLock};
    use std::time::Duration;

    thread_local! {
        static FILES_READ: Cell<usize> = const { Cell::new(0) };
        static LISTINGS: Cell<usize> = const { Cell::new(0) };
        static CLOCK_AHEAD: Cell<Duration> = const { Cell::new(Duration::ZERO) };
        static MAX_DOC_FILES: Cell<Option<usize>> = const { Cell::new(None) };
        static WRITE_FAULT: Cell<Option<usize>> = const { Cell::new(None) };
    }

    /// Lowers this thread's file cap, so truncation can be exercised with a
    /// handful of notes instead of five thousand.
    pub(super) fn set_max_doc_files(cap: Option<usize>) {
        MAX_DOC_FILES.set(cap);
    }

    pub(super) fn max_doc_files() -> Option<usize> {
        MAX_DOC_FILES.get()
    }

    /// Makes this thread's rename fail its `attempt`th rewrite (from 0) with
    /// an injected write error.
    pub(super) fn set_write_fault_at(attempt: Option<usize>) {
        WRITE_FAULT.set(attempt);
    }

    pub(super) fn write_fault_at() -> Option<usize> {
        WRITE_FAULT.get()
    }

    pub(super) fn note_read() {
        FILES_READ.set(FILES_READ.get() + 1);
    }

    pub(super) fn files_read() -> usize {
        FILES_READ.get()
    }

    pub(super) fn note_listing() {
        LISTINGS.set(LISTINGS.get() + 1);
    }

    pub(super) fn listings() -> usize {
        LISTINGS.get()
    }

    /// Moves this thread's scan clock forward, so files written moments ago
    /// count as settled without the test sleeping through the racy window.
    pub(super) fn set_clock_ahead(ahead: Duration) {
        CLOCK_AHEAD.set(ahead);
    }

    pub(super) fn clock_ahead() -> Duration {
        CLOCK_AHEAD.get()
    }

    type Armed = (SyncSender<()>, Receiver<()>);

    fn armed() -> &'static Mutex<HashMap<PathBuf, Armed>> {
        static ARMED: OnceLock<Mutex<HashMap<PathBuf, Armed>>> = OnceLock::new();
        ARMED.get_or_init(|| Mutex::new(HashMap::new()))
    }

    pub(super) struct Pause {
        entered: Receiver<()>,
        release: SyncSender<()>,
    }

    impl Pause {
        pub(super) fn wait_entered(&self, timeout: Duration) -> bool {
            self.entered.recv_timeout(timeout).is_ok()
        }

        pub(super) fn release(&self) {
            let _ = self.release.try_send(());
        }
    }

    /// Holds the next build of `repo` (canonical path) after its listing.
    pub(super) fn arm_pause(repo: &Path) -> Pause {
        let (entered_tx, entered_rx) = sync_channel(1);
        let (release_tx, release_rx) = sync_channel(1);
        armed()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(repo.to_path_buf(), (entered_tx, release_rx));
        Pause {
            entered: entered_rx,
            release: release_tx,
        }
    }

    fn scans() -> &'static Mutex<HashMap<PathBuf, u64>> {
        static SCANS: OnceLock<Mutex<HashMap<PathBuf, u64>>> = OnceLock::new();
        SCANS.get_or_init(|| Mutex::new(HashMap::new()))
    }

    /// A build of `repo` began. Process-wide (not per thread like the counters
    /// above), because the builds a test counts run on other threads.
    pub(super) fn note_scan(repo: &Path) {
        *scans()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(repo.to_path_buf())
            .or_default() += 1;
    }

    pub(super) fn scans_run(repo: &Path) -> u64 {
        scans()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(repo)
            .copied()
            .unwrap_or(0)
    }

    pub(super) fn pause_point(repo: &Path) {
        let armed = armed()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(repo);
        if let Some((entered, release)) = armed {
            let _ = entered.send(());
            let _ = release.recv_timeout(Duration::from_secs(60));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{git_in, git_repo, write};

    /// Past the racy window, so every file the fixture wrote counts as settled.
    const SETTLED: Duration = Duration::from_secs(5);

    /// The cache keeps 8 repositories. Tests that measure what a resident
    /// snapshot saves hold this shared; the test that opens ten repositories
    /// to prove eviction holds it exclusively, so it cannot evict them midway.
    fn cache_residency() -> &'static std::sync::RwLock<()> {
        static RESIDENCY: std::sync::RwLock<()> = std::sync::RwLock::new(());
        &RESIDENCY
    }

    fn resident() -> std::sync::RwLockReadGuard<'static, ()> {
        cache_residency()
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn write_corpus(dir: &Path, count: usize) {
        for n in 0..count {
            write(
                dir,
                &format!("notes/{:02}/note-{n:05}.md", n % 50),
                &format!("# Note {n}\n[[note-00000]] #corpus body-{n}\n"),
            );
        }
    }

    #[test]
    fn no_change_forced_refresh_reads_no_file_and_spawns_no_listing() {
        let _resident = resident();
        probe::set_clock_ahead(SETTLED);
        let dir = fixture_repo();
        let path = dir.path().to_str().unwrap();
        let first = load_vault(path, true).unwrap();
        // The first pass may still re-read entries it saw inside the racy
        // window; the one after it has nothing left to learn.
        load_vault(path, true).unwrap();
        let (reads, listings) = (probe::files_read(), probe::listings());
        let started = Instant::now();
        let after = load_vault(path, true).unwrap();
        let elapsed = started.elapsed();
        assert_eq!(
            probe::files_read() - reads,
            0,
            "an unchanged refresh opened files"
        );
        assert_eq!(
            probe::listings() - listings,
            0,
            "an unchanged index must not spawn git ls-files"
        );
        assert!(Arc::ptr_eq(&first, &after));
        assert!(
            elapsed < crate::test_support::coverage_relaxed(Duration::from_millis(100)),
            "no-change refresh took {elapsed:?}"
        );
    }

    #[test]
    fn refresh_after_a_one_file_edit_reads_only_that_file() {
        let _resident = resident();
        probe::set_clock_ahead(SETTLED);
        let dir = git_repo();
        write_corpus(dir.path(), 200);
        git_in(dir.path(), &["add", "."]);
        let path = dir.path().to_str().unwrap();
        load_vault(path, true).unwrap();
        load_vault(path, true).unwrap();
        write(
            dir.path(),
            "notes/07/note-00007.md",
            "# Note 7\nfreshly-edited-token\n",
        );
        let reads = probe::files_read();
        let after = load_vault(path, true).unwrap();
        assert_eq!(
            probe::files_read() - reads,
            1,
            "only the edited note may be read"
        );
        assert!(!after.vault.search("freshly-edited-token", 5).is_empty());
        assert_eq!(after.status.note_count, 200);
    }

    #[test]
    fn a_docs_query_during_a_rebuild_answers_from_the_previous_snapshot() {
        let _resident = resident();
        let dir = git_repo();
        write_corpus(dir.path(), MAX_DOC_FILES);
        git_in(dir.path(), &["add", "."]);
        let path = dir.path().to_str().unwrap().to_string();
        refresh(&path).unwrap();
        write(
            dir.path(),
            "notes/03/note-00003.md",
            "# Note 3\nrebuilt-snapshot-token\n",
        );
        let pause = probe::arm_pause(&dir.path().canonicalize().unwrap());
        let refreshing = {
            let path = path.clone();
            std::thread::spawn(move || refresh(&path))
        };
        assert!(
            pause.wait_entered(Duration::from_secs(60)),
            "the rebuild never reached its file pass"
        );
        let (sent, answered) = std::sync::mpsc::channel();
        let querying = {
            let path = path.clone();
            std::thread::spawn(move || {
                let started = Instant::now();
                let hits = search(&path, "rebuilt-snapshot-token", 5);
                let _ = sent.send(started.elapsed());
                hits
            })
        };
        let waited = answered.recv_timeout(Duration::from_millis(200));
        pause.release();
        let hits = querying.join().unwrap().unwrap();
        refreshing.join().unwrap().unwrap();
        let waited = waited.expect("docs_search waited behind the rebuild for more than 200 ms");
        assert!(
            waited < Duration::from_millis(200),
            "search took {waited:?}"
        );
        assert!(
            hits.is_empty(),
            "a query during the rebuild must see the previous snapshot: {hits:?}"
        );
        assert!(!search(&path, "rebuilt-snapshot-token", 5)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn rename_reuses_the_cached_vault_instead_of_rebuilding() {
        let _resident = resident();
        probe::set_clock_ahead(SETTLED);
        let dir = fixture_repo();
        let path = dir.path().to_str().unwrap();
        refresh(path).unwrap();
        refresh(path).unwrap();
        let reads = probe::files_read();
        let outcome =
            rename_doc(path, "docs/Roadmap.md", "docs/Plan.md", |_| Ok(())).expect("rename");
        let read = probe::files_read() - reads;
        // The moved note and each rewritten note are new bytes; nothing else is.
        let budget = 1 + outcome.rewritten_paths.len();
        assert!(
            read <= budget,
            "rename read {read} files, more than the {budget} it changed"
        );
        let status = status(path).unwrap();
        assert_eq!(status.note_count, 4);
        assert!(!backlinks(path, "docs/Plan.md").unwrap().is_empty());
    }

    /// Before/after timing for the document refresh, min of N runs.
    ///
    /// `cargo test --manifest-path src-tauri/Cargo.toml --lib
    /// docs::tests::docs_refresh_benchmark -- --ignored --nocapture`, with
    /// `PROFILE_REPO=<absolute repository path>` to add a real repository to
    /// the synthetic 5,000-note corpus. "No snapshot" is a build with nothing
    /// cached (page cache warm); "unchanged" and "one edit" build on the
    /// previous snapshot the way the watcher's refresh does.
    #[test]
    #[ignore = "manual timing workload; set PROFILE_REPO to include a real repository"]
    fn docs_refresh_benchmark() {
        const RUNS: usize = 7;
        fn min_ms(mut run: impl FnMut()) -> f64 {
            (0..RUNS)
                .map(|_| {
                    let started = Instant::now();
                    run();
                    started.elapsed().as_secs_f64() * 1000.0
                })
                .fold(f64::INFINITY, f64::min)
        }
        probe::set_clock_ahead(SETTLED);
        let synthetic = git_repo();
        write_corpus(synthetic.path(), MAX_DOC_FILES);
        git_in(synthetic.path(), &["add", "."]);
        git_in(synthetic.path(), &["commit", "-qm", "benchmark corpus"]);
        let mut targets = vec![(
            "synthetic-5000".to_string(),
            synthetic.path().canonicalize().unwrap(),
        )];
        if let Some(repo) = std::env::var_os("PROFILE_REPO") {
            let repo = PathBuf::from(repo).canonicalize().unwrap();
            // A session grant held in this test process only; nothing is saved.
            crate::test_support::trust_repo(&repo);
            targets.push(("profile".to_string(), repo));
        }
        for (label, repo) in &targets {
            let listing_ms = min_ms(|| {
                list_markdown_paths(repo).unwrap();
            });
            let no_snapshot_ms = min_ms(|| {
                build_vault(repo, None).unwrap();
            });
            let mut previous = build_vault(repo, None).unwrap();
            previous = build_vault(repo, Some(&previous)).unwrap();
            let reads = probe::files_read();
            let unchanged_ms = min_ms(|| {
                previous = build_vault(repo, Some(&previous)).unwrap();
            });
            let unchanged_reads = (probe::files_read() - reads) / RUNS;
            // The re-index any changed content pays, whatever was read.
            let reindex_ms = min_ms(|| {
                Vault::build(repo.clone(), previous.vault.notes().to_vec());
            });
            println!(
                "docs_refresh_bench target={label} notes={} listing_ms={listing_ms:.2} \
                 no_snapshot_ms={no_snapshot_ms:.2} unchanged_ms={unchanged_ms:.2} \
                 unchanged_reads_per_run={unchanged_reads} reindex_ms={reindex_ms:.2}",
                previous.status.note_count
            );
        }
        let repo = &targets[0].1;
        let mut previous = build_vault(repo, None).unwrap();
        let mut edit = 0usize;
        let reads = probe::files_read();
        let one_edit_ms = min_ms(|| {
            edit += 1;
            write(
                repo,
                "notes/01/note-00001.md",
                &format!("# Note 1\nedit {edit}\n"),
            );
            previous = build_vault(repo, Some(&previous)).unwrap();
        });
        println!(
            "docs_refresh_bench target=synthetic-5000 one_edit_ms={one_edit_ms:.2} \
             one_edit_reads_per_run={}",
            (probe::files_read() - reads) / RUNS
        );
    }

    /// Forced refreshes for one repository that arrive together share a scan.
    ///
    /// Every forced refresh used to queue on the repository's slot and then run
    /// its own full read of every tracked note, so N overlapping requests cost N
    /// sequential scans — the 1.2-1.9 s `cmd_docs_refresh` entries in the live
    /// log, repeated. A request is satisfied by any scan that began after it was
    /// made, because that scan reads the files after the request.
    #[test]
    fn overlapping_forced_refreshes_share_one_scan() {
        let _resident = resident();
        const CALLERS: usize = 8;
        let dir = git_repo();
        let path = dir.path().to_str().unwrap().to_owned();
        let body = "## Section\nText with [[note-000]] and #tag.\n\n".repeat(200);
        for n in 0..300 {
            write(
                dir.path(),
                &format!("note-{n:03}.md"),
                &format!("# Note {n}\n{body}"),
            );
        }
        git_in(dir.path(), &["add", "."]);
        let repo = validate_repo(&path).unwrap();
        let before = probe::scans_run(&repo);
        let barrier = Arc::new(std::sync::Barrier::new(CALLERS));
        let started = Instant::now();
        let handles: Vec<_> = (0..CALLERS)
            .map(|_| {
                let (path, barrier) = (path.clone(), Arc::clone(&barrier));
                std::thread::spawn(move || {
                    barrier.wait();
                    refresh(&path).unwrap().note_count
                })
            })
            .collect();
        for handle in handles {
            assert_eq!(handle.join().unwrap(), 300);
        }
        let scans = probe::scans_run(&repo) - before;
        println!(
            "docs_refresh_overlap callers={CALLERS} scans={scans} wall_ms={}",
            started.elapsed().as_millis()
        );
        assert!(
            scans <= 2,
            "{CALLERS} overlapping refreshes ran {scans} scans"
        );
    }

    #[test]
    fn unchanged_documents_reuse_the_indexed_snapshot_on_forced_refresh() {
        let _resident = resident();
        let dir = fixture_repo();
        let path = dir.path().to_str().unwrap();
        let before = load_vault(path, true).unwrap();
        let after = load_vault(path, true).unwrap();
        assert!(
            Arc::ptr_eq(&before, &after),
            "unchanged source must not allocate/rebuild the vault"
        );
    }

    #[test]
    fn incremental_refresh_detects_same_size_same_timestamp_edits() {
        let dir = fixture_repo();
        let path = dir.path().to_str().unwrap();
        let file_path = dir.path().join("docs/Guide.md");
        std::fs::write(&file_path, "# Guide\noldtoken\n").unwrap();
        let before = load_vault(path, true).unwrap();
        let modified = std::fs::metadata(&file_path).unwrap().modified().unwrap();
        std::fs::write(&file_path, "# Guide\nnewtoken\n").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&file_path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(modified))
            .unwrap();
        let after = load_vault(path, true).unwrap();
        assert!(!Arc::ptr_eq(&before, &after));
        let guide = after
            .vault
            .notes()
            .iter()
            .find(|note| note.path == "docs/Guide.md")
            .unwrap();
        assert!(guide.text.contains("newtoken"));
        assert!(!guide.text.contains("oldtoken"));
    }

    #[test]
    fn incremental_refresh_is_scoped_to_the_vault_root() {
        let first = fixture_repo();
        let second = fixture_repo();
        let before = build_vault(first.path(), None).unwrap();
        let after = build_vault(second.path(), Some(&before)).unwrap();
        assert!(!Arc::ptr_eq(&before, &after));
        assert_eq!(after.vault.root(), second.path());
    }

    #[test]
    fn tracked_document_extensions_match_the_parser_including_uppercase() {
        let dir = git_repo();
        let mut expected = Vec::new();
        for (i, extension) in markdev_vault::note::MARKDOWN_EXTENSIONS.iter().enumerate() {
            for (j, extension) in [extension.to_string(), extension.to_uppercase()]
                .iter()
                .enumerate()
            {
                let name = format!("docs/note-{i}-{j}.{extension}");
                write(dir.path(), &name, "# Document\n");
                expected.push(name);
            }
        }
        write(dir.path(), "docs/ordinary.txt", "# Not Markdown\n");
        git_in(dir.path(), &["add", "."]);
        write(dir.path(), "untracked.md", "# Untracked\n");
        expected.sort();
        assert_eq!(list_markdown_paths(dir.path()).unwrap(), expected);
    }

    #[test]
    fn incremental_refresh_matches_full_rebuild_through_file_churn() {
        // Settled clock: the incremental side must take its stat-reuse path,
        // not fall back to reading everything as racy.
        probe::set_clock_ahead(SETTLED);
        let dir = git_repo();
        for n in 0..48 {
            write(
                dir.path(),
                &format!("note-{n:03}.md"),
                &format!("# Note {n}\n[[note-000]] #shared\n"),
            );
        }
        git_in(dir.path(), &["add", "."]);
        let mut previous = build_vault(dir.path(), None).unwrap();
        git_in(dir.path(), &["commit", "-qm", "document churn fixture"]);
        let outside = tempfile::tempdir().unwrap();
        write(outside.path(), "outside.md", "# Outside\nprivate-body\n");
        for step in 0..40 {
            let changed = format!("note-{step:03}.md");
            let changed_abs = dir.path().join(&changed);
            match step % 8 {
                0 => write(
                    dir.path(),
                    &changed,
                    &format!("# Changed {step}\n[[absent]] #different\n"),
                ),
                1 => {
                    git_in(dir.path(), &["rm", &changed]);
                }
                2 => {
                    git_in(
                        dir.path(),
                        &["mv", &changed, &format!("moved-{step:03}.md")],
                    );
                }
                3 => {
                    std::fs::write(&changed_abs, [0xff, 0xfe]).unwrap();
                }
                4 => {
                    // Same size, timestamp restored: only ctime can tell.
                    let before = std::fs::read_to_string(&changed_abs).unwrap();
                    let modified = std::fs::metadata(&changed_abs).unwrap().modified().unwrap();
                    let edited = before.replacen("Note", "Edit", 1);
                    assert_eq!(edited.len(), before.len());
                    std::fs::write(&changed_abs, edited).unwrap();
                    std::fs::File::options()
                        .write(true)
                        .open(&changed_abs)
                        .unwrap()
                        .set_times(std::fs::FileTimes::new().set_modified(modified))
                        .unwrap();
                }
                5 => {
                    // Touch only: the bytes did not change.
                    std::fs::File::options()
                        .write(true)
                        .open(&changed_abs)
                        .unwrap()
                        .set_times(
                            std::fs::FileTimes::new().set_modified(std::time::SystemTime::now()),
                        )
                        .unwrap();
                }
                6 => {
                    #[cfg(unix)]
                    {
                        std::fs::remove_file(&changed_abs).unwrap();
                        std::os::unix::fs::symlink(outside.path().join("outside.md"), &changed_abs)
                            .unwrap();
                    }
                }
                _ => {
                    #[cfg(unix)]
                    {
                        std::fs::remove_file(&changed_abs).unwrap();
                        std::os::unix::fs::symlink("note-047.md", &changed_abs).unwrap();
                    }
                }
            }
            let reads = probe::files_read();
            let incremental = build_vault(dir.path(), Some(&previous)).unwrap();
            let read = probe::files_read() - reads;
            assert!(read <= 1, "step {step}: one changed path cost {read} reads");
            let full = build_vault(dir.path(), None).unwrap();
            assert_eq!(incremental.status, full.status, "step {step}");
            assert_eq!(incremental.vault.notes(), full.vault.notes(), "step {step}");
            assert_eq!(
                incremental.vault.broken_links(),
                full.vault.broken_links(),
                "step {step}"
            );
            assert_eq!(
                serde_json::to_value(incremental.vault.search("shared", 100)).unwrap(),
                serde_json::to_value(full.vault.search("shared", 100)).unwrap(),
                "step {step}"
            );
            previous = incremental;
        }
    }

    #[cfg(unix)]
    #[test]
    fn previously_cached_document_replaced_by_outside_symlink_is_removed() {
        let dir = fixture_repo();
        let previous = build_vault(dir.path(), None).unwrap();
        let outside = tempfile::tempdir().unwrap();
        write(outside.path(), "outside.md", "outside private body");
        std::fs::remove_file(dir.path().join("docs/Guide.md")).unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("outside.md"),
            dir.path().join("docs/Guide.md"),
        )
        .unwrap();
        let next = build_vault(dir.path(), Some(&previous)).unwrap();
        assert!(!next
            .vault
            .notes()
            .iter()
            .any(|note| note.path == "docs/Guide.md"));
        assert_eq!(next.status.skipped_unreadable, 1);
    }

    #[cfg(unix)]
    #[test]
    fn a_directory_swapped_for_an_outside_link_drops_its_cached_notes() {
        probe::set_clock_ahead(SETTLED);
        let dir = fixture_repo();
        let previous = build_vault(dir.path(), None).unwrap();
        let outside = tempfile::tempdir().unwrap();
        write(
            outside.path(),
            "Guide.md",
            "# Guide\n\nSee [[Roadmap]] and [intro](Intro.md).\n",
        );
        std::fs::rename(dir.path().join("docs"), dir.path().join("moved-docs")).unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("docs")).unwrap();
        let next = build_vault(dir.path(), Some(&previous)).unwrap();
        assert!(
            next.vault
                .notes()
                .iter()
                .all(|note| !note.path.starts_with("docs/")),
            "a note behind an escaping directory link was kept"
        );
        assert_eq!(next.status.skipped_unreadable, 3);
        assert_eq!(next.status, build_vault(dir.path(), None).unwrap().status);
    }

    #[test]
    fn a_stat_key_is_trusted_only_once_its_change_time_has_settled() {
        let at = |ctime: Duration| StatKey {
            dev: 1,
            ino: 2,
            size: 3,
            mtime: (0, 0),
            ctime: (ctime.as_secs() as i64, i64::from(ctime.subsec_nanos())),
        };
        let now = SystemTime::now();
        let since_epoch = now.duration_since(SystemTime::UNIX_EPOCH).unwrap();
        assert!(!at(since_epoch).settled(now), "changed this instant");
        assert!(!at(since_epoch - RACY_WINDOW / 2).settled(now));
        assert!(at(since_epoch - RACY_WINDOW - Duration::from_millis(1)).settled(now));
        let mut future = at(since_epoch + Duration::from_secs(60));
        assert!(!future.settled(now), "a change time ahead of the clock");
        future.ctime = (-1, 0);
        assert!(!future.settled(now), "a change time before the epoch");
        future.ctime = (1, 1_000_000_000);
        assert!(!future.settled(now), "out-of-range nanoseconds");
    }

    #[test]
    fn document_queries_do_not_hold_the_cross_repository_cache_lock() {
        let dir = fixture_repo();
        let path = dir.path().to_str().unwrap();
        refresh(path).unwrap();
        with_vault(path, |_| {
            assert!(
                vault_cache().try_lock().is_ok(),
                "search/graph work must not lock out every repository"
            );
        })
        .unwrap();
    }

    #[test]
    fn document_vault_cache_does_not_retain_every_repository_ever_opened() {
        let _evicting = cache_residency()
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut repos = Vec::new();
        for _ in 0..10 {
            let dir = git_repo();
            refresh(dir.path().to_str().unwrap()).unwrap();
            repos.push(dir);
        }
        let cache = vault_cache()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(cache.len() <= 8, "retained {} vaults", cache.len());
    }

    #[test]
    fn a_slow_query_cannot_republish_a_vault_after_invalidation() {
        let dir = fixture_repo();
        let path = dir.path().to_str().unwrap();
        invalidate(path);
        with_vault(path, |_| {
            write(dir.path(), "docs/Guide.md", "# Guide\nnewlyupdatedtoken");
            invalidate(path);
            refresh(path).unwrap();
        })
        .unwrap();
        assert!(!search(path, "newlyupdatedtoken", 10).unwrap().is_empty());
    }

    #[test]
    fn document_vault_has_an_aggregate_byte_budget() {
        let dir = git_repo();
        let body = "a".repeat(1024 * 1024);
        for i in 0..34 {
            write(dir.path(), &format!("{i:02}.md"), &body);
        }
        git_in(dir.path(), &["add", "."]);
        let built = build_vault(dir.path(), None).unwrap();
        let vault = &built.vault;
        let status = &built.status;
        let bytes: usize = vault.notes().iter().map(|note| note.text.len()).sum();
        assert!(bytes <= 32 * 1024 * 1024, "retained {bytes} bytes");
        assert!(
            status.truncated,
            "a byte-capped vault must report incomplete coverage"
        );
    }

    #[cfg(unix)]
    #[test]
    fn document_vault_refuses_a_tracked_link_outside_the_repository() {
        let dir = git_repo();
        let outside = tempfile::tempdir().unwrap();
        write(outside.path(), "secret.md", "# Outside\nprivate-body");
        std::os::unix::fs::symlink(outside.path().join("secret.md"), dir.path().join("link.md"))
            .unwrap();
        git_in(dir.path(), &["add", "link.md"]);
        let built = build_vault(dir.path(), None).unwrap();
        let vault = &built.vault;
        let status = &built.status;
        assert!(vault.notes().is_empty());
        assert_eq!(status.skipped_unreadable, 1);
    }

    fn fixture_repo() -> tempfile::TempDir {
        let dir = git_repo();
        write(
            dir.path(),
            "docs/Guide.md",
            "# Guide\n\nSee [[Roadmap]] and [intro](Intro.md).\n",
        );
        write(
            dir.path(),
            "docs/Intro.md",
            "# Intro\n\nBack to [Guide](Guide.md).\n\n```md\n[[Roadmap]]\n```\n",
        );
        write(dir.path(), "docs/Roadmap.md", "# Roadmap\n\nPlan items.\n");
        write(
            dir.path(),
            "README.md",
            "# Repo\n\nDocs in [Guide](docs/Guide.md).\n",
        );
        // Ignored markdown must not enter the vault.
        write(dir.path(), ".gitignore", "vendor/\n");
        write(dir.path(), "vendor/Secret.md", "# Secret\n[[Guide]]\n");
        git_in(dir.path(), &["add", "docs", "README.md", ".gitignore"]);
        git_in(dir.path(), &["commit", "-m", "docs fixture"]);
        dir
    }

    #[test]
    fn vault_comes_from_git_ls_files_not_the_filesystem_walk() {
        let dir = fixture_repo();
        let path = dir.path().to_str().unwrap();
        let status = refresh(path).expect("refresh");
        // Four tracked notes; vendor/Secret.md is ignored.
        assert_eq!(status.note_count, 4, "{status:?}");
        assert_eq!(status.skipped_unreadable, 0);

        let hits = search(path, "Plan", 10).expect("search");
        assert!(hits.iter().any(|h| h.path == "docs/Roadmap.md"), "{hits:?}");

        let broken = broken_links(path).expect("broken");
        assert!(
            broken.is_empty(),
            "fixture links should resolve: {broken:?}"
        );

        let backs = backlinks(path, "docs/Roadmap.md").expect("backlinks");
        assert!(backs.iter().any(|b| b.path == "docs/Guide.md"), "{backs:?}");

        let g = graph(path, None, 0, None, None).expect("graph");
        assert_eq!(g.total_notes, 4);
        assert_eq!(g.nodes.len(), 4);
        assert!(!g.edges.is_empty());
        // Deterministic layout: same vault → same coords.
        let g2 = graph(path, None, 0, None, None).expect("graph again");
        assert_eq!(g.nodes, g2.nodes);
    }

    #[test]
    fn ignored_markdown_never_enters_the_vault() {
        let dir = fixture_repo();
        let path = dir.path().to_str().unwrap();
        let _ = refresh(path).expect("refresh");
        let hits = search(path, "Secret", 10).expect("search");
        assert!(hits.iter().all(|h| !h.path.contains("vendor")), "{hits:?}");
    }

    #[test]
    fn rename_rewrites_links_and_protects_fenced_samples() {
        let dir = fixture_repo();
        let path = dir.path().to_str().unwrap();
        let outcome =
            rename_doc(path, "docs/Roadmap.md", "docs/Plan.md", |_| Ok(())).expect("rename");
        assert_eq!(outcome.from, "docs/Roadmap.md");
        assert_eq!(outcome.to, "docs/Plan.md");
        assert!(outcome.links_rewritten >= 1, "{outcome:?}");
        assert!(
            outcome.rewritten_paths.iter().any(|p| p == "docs/Guide.md"),
            "{outcome:?}"
        );

        let guide = std::fs::read_to_string(dir.path().join("docs/Guide.md")).unwrap();
        assert!(
            guide.contains("[[Plan]]") || guide.contains("[[docs/Plan]]"),
            "wikilink should move: {guide}"
        );
        let intro = std::fs::read_to_string(dir.path().join("docs/Intro.md")).unwrap();
        // Fenced sample must keep the old spelling.
        assert!(
            intro.contains("```md\n[[Roadmap]]\n```"),
            "code fence must be protected: {intro}"
        );

        // Staged for review — not committed.
        let status = git_text(dir.path(), &["status", "--porcelain"]).unwrap();
        assert!(
            status.contains("Plan.md") || status.contains("Roadmap.md"),
            "rename should be staged/visible: {status}"
        );
    }

    #[test]
    fn refresh_invalidates_and_rebuilds() {
        let dir = fixture_repo();
        let path = dir.path().to_str().unwrap();
        assert_eq!(refresh(path).unwrap().note_count, 4);
        write(dir.path(), "docs/Extra.md", "# Extra\n");
        git_in(dir.path(), &["add", "docs/Extra.md"]);
        // Cached vault is stale until refresh.
        invalidate(path);
        assert_eq!(refresh(path).unwrap().note_count, 5);
    }

    #[test]
    fn status_retains_honesty_fields_from_refresh() {
        let dir = fixture_repo();
        let path = dir.path().to_str().unwrap();
        let refreshed = refresh(path).expect("refresh");
        let reported = status(path).expect("status");
        assert_eq!(reported.note_count, refreshed.note_count);
        assert_eq!(reported.truncated, refreshed.truncated);
        assert_eq!(reported.skipped_oversized, refreshed.skipped_oversized);
        assert_eq!(reported.skipped_unreadable, refreshed.skipped_unreadable);
    }
}

#[cfg(test)]
mod rename_tests;
