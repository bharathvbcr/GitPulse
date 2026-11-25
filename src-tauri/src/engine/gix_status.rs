//! In-process repository status via `gix`, producing `git status --porcelain=v1 -u -z` bytes.
//!
//! `gix 0.90.0` does not talk to `fsmonitor--daemon`, avoiding Apple Git's
//! un-timed `_pthread_cond_wait` hangs while matching `--porcelain=v1 -u -z`
//! semantics across tracked changes, staged renames (`old_path`),
//! typechanges, conflict stage pairs, submodules, symlinks, CRLF filters, and
//! untracked enumeration. Repositories requiring unsupported features
//! (bare checkouts, repo-local `filter.*` drivers, split or sparse indexes,
//! copy detection configured through `status.renames` or `diff.renames`,
//! non-UTF-8 paths, or worktree-to-index rewrites) return
//! [`GixStatus::Fallback`] so `worktree::spawn_porcelain` falls back cleanly
//! to `git_cli` with `core.fsmonitor=false`.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use gix::bstr::{BStr, BString};

use crate::engine::process::{
    read_cancel::{self, ReadCancel},
    DEFAULT_TIMEOUT, TIMEOUT_MARKER,
};
use crate::engine::worktree::{ScanDeadline, DEADLINE_PASSED, STATUS_GRACE};
use crate::ipc_error::CommandError;

/// Result of attempting in-process `gix` status for a single worktree.
#[derive(Debug)]
pub(crate) enum GixStatus {
    /// Complete `--porcelain=v1 -u -z` byte payload.
    Porcelain(String),
    /// Caller should fall back to `git_cli` (reason for diagnostics/tests).
    #[allow(dead_code)]
    Fallback(&'static str),
}

struct StatusSemaphore {
    state: Mutex<usize>,
    cap: usize,
    ready: Condvar,
}

impl StatusSemaphore {
    fn global() -> &'static Self {
        static SEM: OnceLock<StatusSemaphore> = OnceLock::new();
        SEM.get_or_init(|| {
            let cap = std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4)
                .clamp(2, 6);
            StatusSemaphore {
                state: Mutex::new(0),
                cap,
                ready: Condvar::new(),
            }
        })
    }

    fn acquire(
        &'static self,
        started: Instant,
        deadline: Option<&ScanDeadline>,
        cancel: Option<&ReadCancel>,
    ) -> Result<StatusPermit<'static>, CommandError> {
        let mut active = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            if cancel.is_some_and(ReadCancel::is_cancelled) {
                return Err(read_cancel::cancelled());
            }
            if let Some(d) = deadline {
                if d.expired() {
                    return Err(DEADLINE_PASSED.to_string().into());
                }
            } else if started.elapsed() >= DEFAULT_TIMEOUT {
                return Err(timeout_error(started));
            }
            if *active < self.cap {
                *active += 1;
                return Ok(StatusPermit { sem: self });
            }
            let remaining = match deadline {
                Some(d) => d.remaining(),
                None => (started + DEFAULT_TIMEOUT).saturating_duration_since(Instant::now()),
            };
            if remaining.is_zero() {
                return Err(match deadline {
                    Some(_) => DEADLINE_PASSED.to_string().into(),
                    None => timeout_error(started),
                });
            }
            let slice = if cancel.is_some() {
                remaining.min(Duration::from_millis(50))
            } else {
                remaining
            };
            active = self
                .ready
                .wait_timeout(active, slice)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }
}

struct StatusPermit<'a> {
    sem: &'a StatusSemaphore,
}

impl Drop for StatusPermit<'_> {
    fn drop(&mut self) {
        let mut active = self
            .sem
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        *active = active.saturating_sub(1);
        self.sem.ready.notify_one();
    }
}

struct WatchEntry {
    interrupt: Arc<AtomicBool>,
    cancel: Option<Arc<AtomicBool>>,
}

struct WatcherState {
    due: BTreeMap<(Instant, u64), WatchEntry>,
    next_id: u64,
}

struct DeadlineWatcher {
    state: Mutex<WatcherState>,
    cvar: Condvar,
}

impl DeadlineWatcher {
    fn global() -> &'static Arc<Self> {
        static WATCHER: OnceLock<Arc<DeadlineWatcher>> = OnceLock::new();
        WATCHER.get_or_init(|| {
            let watcher = Arc::new(DeadlineWatcher {
                state: Mutex::new(WatcherState {
                    due: BTreeMap::new(),
                    next_id: 0,
                }),
                cvar: Condvar::new(),
            });
            let bg = Arc::clone(&watcher);
            // Long-lived, so it starts under the supervisor: a panic is
            // recorded and the loop restarts with backoff. `run` rebuilds
            // nothing but the lock guard, and the due list it serves lives
            // in `bg`, so a restart resumes the same deadlines.
            if let Err(err) =
                crate::supervisor::spawn_service("gitpulse-gix-deadline", move || bg.run())
            {
                log::warn!("failed to spawn gitpulse-gix-deadline thread: {err}");
            }
            watcher
        })
    }

    fn register(
        &'static self,
        deadline: Instant,
        interrupt: Arc<AtomicBool>,
        cancel: Option<Arc<AtomicBool>>,
    ) -> WatchGuard<'static> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.next_id = state.next_id.wrapping_add(1);
        let key = (deadline, state.next_id);
        state.due.insert(key, WatchEntry { interrupt, cancel });
        self.cvar.notify_one();
        WatchGuard { watcher: self, key }
    }

    fn run(&self) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            if state.due.is_empty() {
                state = self
                    .cvar
                    .wait(state)
                    .unwrap_or_else(PoisonError::into_inner);
                continue;
            }
            let now = Instant::now();
            let mut to_trip = Vec::new();
            let mut expired_keys = Vec::new();
            let mut cancelled_keys = Vec::new();
            let mut has_cancel = false;
            for (key, entry) in &state.due {
                if key.0 <= now {
                    expired_keys.push(*key);
                } else if entry
                    .cancel
                    .as_ref()
                    .is_some_and(|flag| flag.load(Ordering::Acquire))
                {
                    cancelled_keys.push(*key);
                } else if entry.cancel.is_some() {
                    has_cancel = true;
                }
            }
            for key in expired_keys.into_iter().chain(cancelled_keys) {
                if let Some(entry) = state.due.remove(&key) {
                    to_trip.push(entry.interrupt);
                }
            }
            if !to_trip.is_empty() {
                drop(state);
                for interrupt in to_trip {
                    interrupt.store(true, Ordering::Release);
                }
                state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
                continue;
            }
            let Some((&(earliest, _), _)) = state.due.iter().next() else {
                continue;
            };
            let wait = earliest.saturating_duration_since(now);
            let wait = if has_cancel {
                wait.min(Duration::from_millis(25))
            } else {
                wait
            };
            state = self
                .cvar
                .wait_timeout(state, wait)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }
}

struct WatchGuard<'a> {
    watcher: &'a DeadlineWatcher,
    key: (Instant, u64),
}

impl Drop for WatchGuard<'_> {
    fn drop(&mut self) {
        let mut state = self
            .watcher
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        state.due.remove(&self.key);
    }
}

fn timeout_error(started: Instant) -> CommandError {
    format!(
        "git status{TIMEOUT_MARKER}{:.3}s",
        started.elapsed().as_secs_f64()
    )
    .into()
}

fn check_cut(
    started: Instant,
    effective_deadline: Instant,
    cancel: Option<&ReadCancel>,
    interrupt: &AtomicBool,
) -> Result<(), CommandError> {
    if cancel.is_some_and(ReadCancel::is_cancelled) {
        return Err(read_cancel::cancelled());
    }
    if Instant::now() >= effective_deadline || interrupt.load(Ordering::Acquire) {
        return Err(timeout_error(started));
    }
    Ok(())
}

fn bstr_to_utf8(bytes: &BStr) -> Option<String> {
    std::str::from_utf8(bytes.as_ref()).ok().map(str::to_owned)
}

fn is_regular_file(mode: gix::index::entry::Mode) -> bool {
    matches!(
        mode,
        gix::index::entry::Mode::FILE | gix::index::entry::Mode::FILE_EXECUTABLE
    )
}

fn is_typechange(prev: gix::index::entry::Mode, cur: gix::index::entry::Mode) -> bool {
    prev != cur && !(is_regular_file(prev) && is_regular_file(cur))
}

fn conflict_letters(summary: gix::status::plumbing::index_as_worktree::Conflict) -> (char, char) {
    use gix::status::plumbing::index_as_worktree::Conflict;
    match summary {
        Conflict::BothDeleted => ('D', 'D'),
        Conflict::AddedByUs => ('A', 'U'),
        Conflict::DeletedByThem => ('U', 'D'),
        Conflict::AddedByThem => ('U', 'A'),
        Conflict::DeletedByUs => ('D', 'U'),
        Conflict::BothAdded => ('A', 'A'),
        Conflict::BothModified => ('U', 'U'),
    }
}

fn has_local_external_filter(repo: &gix::Repository) -> bool {
    let config = repo.config_snapshot();
    let found = config
        .plumbing()
        .sections_by_name("filter")
        .into_iter()
        .flatten()
        .any(|section| {
            matches!(
                section.meta().source,
                gix::config::Source::Local | gix::config::Source::Worktree
            ) && (section.value("clean").is_some()
                || section.value("smudge").is_some()
                || section.value("process").is_some())
        });
    found
}

/// Whether `status.renames` or `diff.renames` names `copies` (or `copy`; git
/// compares both without case). `gix 0.90` and git disagree in both
/// directions once either key asks for copies: with `status.renames=copies`
/// gix reports a staged copy as an addition where git reports
/// `C  <new>\0<source>\0`, and with `diff.renames=copies` under
/// `status.renames=true` gix reports a copy (and drops the source's own `M`
/// row) where git reports neither. git is the reference, so these
/// repositories read through it.
fn asks_for_copy_detection(repo: &gix::Repository) -> bool {
    let config = repo.config_snapshot();
    ["status.renames", "diff.renames"].into_iter().any(|key| {
        config.string(key).is_some_and(|value| {
            let value: &BStr = value.as_ref();
            value.eq_ignore_ascii_case(b"copies") || value.eq_ignore_ascii_case(b"copy")
        })
    })
}

fn has_unregistered_gitlink(repo: &gix::Repository, index: &gix::index::State) -> bool {
    use gix::bstr::ByteSlice;
    let mut gitlinks = index
        .entries()
        .iter()
        .filter(|entry| entry.mode == gix::index::entry::Mode::COMMIT)
        .map(|entry| entry.path(index))
        .peekable();
    if gitlinks.peek().is_none() {
        return false;
    }
    let Ok(Some(submodules)) = repo.submodules() else {
        return true;
    };
    let mut registered: Vec<BString> = submodules.filter_map(|sm| sm.path().ok()).collect();
    registered.sort();
    gitlinks.any(|path| {
        registered
            .binary_search_by(|p| p.as_bstr().cmp(path))
            .is_err()
    })
}

#[derive(Clone, Debug)]
struct TrackedRow {
    x: char,
    y: char,
    old_path: Option<String>,
    conflicted: bool,
}

impl Default for TrackedRow {
    fn default() -> Self {
        Self {
            x: ' ',
            y: ' ',
            old_path: None,
            conflicted: false,
        }
    }
}

/// Computes `git status --porcelain=v1 -u -z` in-process using `gix`, or
/// returns [`GixStatus::Fallback`] when `dir` needs features `gix` declines.
pub(crate) fn porcelain_v1_z(
    dir: &Path,
    deadline: Option<&ScanDeadline>,
) -> Result<GixStatus, CommandError> {
    let started = Instant::now();
    let cancel = read_cancel::current();
    if cancel.as_ref().is_some_and(ReadCancel::is_cancelled) {
        return Err(read_cancel::cancelled());
    }
    if deadline.is_some_and(ScanDeadline::expired) {
        return Err(DEADLINE_PASSED.to_string().into());
    }

    let _permit = StatusSemaphore::global().acquire(started, deadline, cancel.as_ref())?;
    let effective_deadline = match deadline {
        Some(d) => Instant::now() + d.remaining().max(STATUS_GRACE),
        None => started + DEFAULT_TIMEOUT,
    };

    let interrupt = Arc::new(AtomicBool::new(false));
    let cancel_atomic = cancel.as_ref().map(ReadCancel::as_atomic);
    let _watch = DeadlineWatcher::global().register(
        effective_deadline,
        Arc::clone(&interrupt),
        cancel_atomic,
    );

    run_status_with_interrupt(
        dir,
        started,
        effective_deadline,
        cancel.as_ref(),
        &interrupt,
    )
}

fn run_status_with_interrupt(
    dir: &Path,
    started: Instant,
    effective_deadline: Instant,
    cancel: Option<&ReadCancel>,
    interrupt: &Arc<AtomicBool>,
) -> Result<GixStatus, CommandError> {
    check_cut(started, effective_deadline, cancel, interrupt)?;

    let Ok(repo) = gix::open(dir) else {
        return Ok(GixStatus::Fallback("gix_open"));
    };
    if repo.is_bare() {
        return Ok(GixStatus::Fallback("bare"));
    }
    if has_local_external_filter(&repo) {
        return Ok(GixStatus::Fallback("local_external_filter"));
    }
    if repo.config_snapshot().boolean("core.splitIndex") == Some(true) {
        return Ok(GixStatus::Fallback("split_index"));
    }
    if asks_for_copy_detection(&repo) {
        return Ok(GixStatus::Fallback("copy_detection"));
    }

    let Ok(index) = repo.index_or_empty() else {
        check_cut(started, effective_deadline, cancel, interrupt)?;
        return Ok(GixStatus::Fallback("index_load"));
    };
    if index.is_sparse() || index.link().is_some() {
        return Ok(GixStatus::Fallback("split_or_sparse_index"));
    }
    if has_unregistered_gitlink(&repo, &index) {
        return Ok(GixStatus::Fallback("unregistered_gitlink"));
    }

    let Ok(dirwalk_options) = repo.dirwalk_options() else {
        check_cut(started, effective_deadline, cancel, interrupt)?;
        return Ok(GixStatus::Fallback("dirwalk_options"));
    };
    let Ok(platform) = repo.status(gix::progress::Discard) else {
        check_cut(started, effective_deadline, cancel, interrupt)?;
        return Ok(GixStatus::Fallback("status_platform"));
    };
    let platform = platform
        .index(gix::worktree::IndexPersistedOrInMemory::Persisted(index))
        // `repo.status()` disables `dirwalk_options` when the user configured
        // `status.showUntrackedFiles=no`, which makes `.untracked_files(Files)`
        // a no-op. Restore default dirwalk options before requesting `-u` so a
        // user preference never hides held untracked files.
        .index_worktree_options_mut(|opts| {
            if opts.dirwalk_options.is_none() {
                opts.dirwalk_options = Some(dirwalk_options);
            }
            opts.thread_limit = Some(2);
        })
        .untracked_files(gix::status::UntrackedFiles::Files)
        .index_worktree_submodules(gix::status::Submodule::AsConfigured { check_dirty: true })
        .should_interrupt_owned(Arc::clone(interrupt));

    let Ok(mut iter) = platform.into_iter(Vec::<BString>::new()) else {
        check_cut(started, effective_deadline, cancel, interrupt)?;
        return Ok(GixStatus::Fallback("status_iter"));
    };

    let mut tracked: HashMap<String, TrackedRow> = HashMap::new();
    let mut untracked: Vec<String> = Vec::new();

    for item in &mut iter {
        check_cut(started, effective_deadline, cancel, interrupt)?;
        let Ok(item) = item else {
            check_cut(started, effective_deadline, cancel, interrupt)?;
            return Ok(GixStatus::Fallback("status_item"));
        };
        match item {
            gix::status::Item::TreeIndex(change) => {
                use gix::diff::index::Change;
                let (path, letter, old_path) = match &change {
                    Change::Addition { location, .. } => {
                        let Some(path) = bstr_to_utf8(location.as_ref()) else {
                            return Ok(GixStatus::Fallback("non_utf8_path"));
                        };
                        (path, 'A', None)
                    }
                    Change::Deletion { location, .. } => {
                        let Some(path) = bstr_to_utf8(location.as_ref()) else {
                            return Ok(GixStatus::Fallback("non_utf8_path"));
                        };
                        (path, 'D', None)
                    }
                    Change::Modification {
                        location,
                        previous_entry_mode,
                        entry_mode,
                        ..
                    } => {
                        let Some(path) = bstr_to_utf8(location.as_ref()) else {
                            return Ok(GixStatus::Fallback("non_utf8_path"));
                        };
                        let letter = if is_typechange(*previous_entry_mode, *entry_mode) {
                            'T'
                        } else {
                            'M'
                        };
                        (path, letter, None)
                    }
                    Change::Rewrite {
                        source_location,
                        location,
                        copy,
                        ..
                    } => {
                        let Some(path) = bstr_to_utf8(location.as_ref()) else {
                            return Ok(GixStatus::Fallback("non_utf8_path"));
                        };
                        let Some(src) = bstr_to_utf8(source_location.as_ref()) else {
                            return Ok(GixStatus::Fallback("non_utf8_path"));
                        };
                        (path, if *copy { 'C' } else { 'R' }, Some(src))
                    }
                };
                let row = tracked.entry(path).or_default();
                if !row.conflicted {
                    row.x = letter;
                    row.old_path = old_path;
                }
            }
            gix::status::Item::IndexWorktree(iw) => {
                use gix::status::index_worktree::{iter::Summary, Item as IwItem};
                use gix::status::plumbing::index_as_worktree::EntryStatus;
                if let IwItem::Modification {
                    status: EntryStatus::Conflict { summary, .. },
                    rela_path,
                    ..
                } = &iw
                {
                    let Some(path) = bstr_to_utf8(rela_path.as_ref()) else {
                        return Ok(GixStatus::Fallback("non_utf8_path"));
                    };
                    let (cx, cy) = conflict_letters(*summary);
                    let row = tracked.entry(path).or_default();
                    row.x = cx;
                    row.y = cy;
                    row.old_path = None;
                    row.conflicted = true;
                    continue;
                }
                let Some(summary) = iw.summary() else {
                    continue;
                };
                let Some(path) = bstr_to_utf8(iw.rela_path()) else {
                    return Ok(GixStatus::Fallback("non_utf8_path"));
                };
                match summary {
                    Summary::Added => untracked.push(path),
                    Summary::Removed => {
                        let row = tracked.entry(path).or_default();
                        if !row.conflicted {
                            row.y = 'D';
                        }
                    }
                    Summary::Modified => {
                        let row = tracked.entry(path).or_default();
                        if !row.conflicted {
                            row.y = 'M';
                        }
                    }
                    Summary::TypeChange => {
                        let row = tracked.entry(path).or_default();
                        if !row.conflicted {
                            row.y = 'T';
                        }
                    }
                    Summary::IntentToAdd => {
                        let row = tracked.entry(path).or_default();
                        if !row.conflicted {
                            row.y = 'A';
                        }
                    }
                    Summary::Conflict => {
                        let row = tracked.entry(path).or_default();
                        row.x = 'U';
                        row.y = 'U';
                        row.old_path = None;
                        row.conflicted = true;
                    }
                    Summary::Renamed | Summary::Copied => {
                        return Ok(GixStatus::Fallback("worktree_rewrite"));
                    }
                }
            }
        }
    }

    // Critical `gix 0.90.0` invariant: `Iter::next()` returns `None` (not `Err`)
    // on `should_interrupt`, leaving `into_outcome() == None`. Never emit a
    // partial or clean status unless cancellation, timeout, and outcome completion
    // have all been ruled out.
    check_cut(started, effective_deadline, cancel, interrupt)?;
    if iter.into_outcome().is_none() {
        return Ok(GixStatus::Fallback("incomplete_outcome"));
    }

    let mut rows: Vec<(String, char, char, Option<String>)> =
        Vec::with_capacity(tracked.len() + untracked.len());
    for (path, row) in tracked {
        rows.push((path, row.x, row.y, row.old_path));
    }
    for path in untracked {
        rows.push((path, '?', '?', None));
    }
    rows.sort();

    let mut out = String::new();
    for (path, x, y, old_path) in rows {
        if x == 'R' || x == 'C' || y == 'R' || y == 'C' {
            let Some(old) = old_path else {
                return Ok(GixStatus::Fallback("rewrite_missing_old_path"));
            };
            out.push(x);
            out.push(y);
            out.push(' ');
            out.push_str(&path);
            out.push('\0');
            out.push_str(&old);
            out.push('\0');
        } else {
            out.push(x);
            out.push(y);
            out.push(' ');
            out.push_str(&path);
            out.push('\0');
        }
    }
    Ok(GixStatus::Porcelain(out))
}

/// Integration-test entry point returning `Ok(Some(porcelain_z))` when `gix`
/// answered or `Ok(None)` when `gix` declined to `Fallback`.
#[doc(hidden)]
pub fn porcelain_v1_z_for_test(dir: &Path) -> Result<Option<String>, CommandError> {
    match porcelain_v1_z(dir, None)? {
        GixStatus::Porcelain(bytes) => Ok(Some(bytes)),
        GixStatus::Fallback(_) => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::process::read_cancel::{enter, is_cancelled_error, ReadCancel};
    use crate::test_support::{git_in, git_repo};

    #[test]
    fn pre_cancelled_read_returns_cancelled_immediately() {
        let dir = git_repo();
        let cancel = ReadCancel::new();
        cancel.cancel();
        let _scope = enter(Some(&cancel));
        let err = porcelain_v1_z(dir.path(), None).expect_err("cancelled read must fail");
        assert!(is_cancelled_error(&err), "{err:?}");
    }

    #[test]
    fn expired_deadline_returns_error_not_clean_status() {
        let dir = git_repo();
        let expired = ScanDeadline::after(Instant::now(), Duration::ZERO);
        let err =
            porcelain_v1_z(dir.path(), Some(&expired)).expect_err("expired deadline must fail");
        assert_eq!(err.message(), DEADLINE_PASSED);
    }

    #[test]
    fn pre_interrupted_status_never_returns_clean_porcelain() {
        let dir = git_repo();
        let interrupt = Arc::new(AtomicBool::new(true));
        let deadline = Instant::now() + Duration::from_secs(30);
        let err = run_status_with_interrupt(dir.path(), Instant::now(), deadline, None, &interrupt)
            .expect_err("interrupted run must error, not return clean status");
        assert!(err.message().contains(TIMEOUT_MARKER), "{err:?}");
    }

    #[test]
    fn local_external_filter_triggers_fallback() {
        let dir = git_repo();
        let root = dir.path();
        git_in(root, &["config", "--local", "filter.custom.clean", "cat"]);
        match porcelain_v1_z(root, None).expect("status") {
            GixStatus::Fallback(reason) => assert_eq!(reason, "local_external_filter"),
            GixStatus::Porcelain(text) => panic!("expected fallback, got porcelain {text:?}"),
        }
    }

    #[test]
    fn split_index_triggers_fallback() {
        let dir = git_repo();
        let root = dir.path();
        std::fs::write(root.join("seed.txt"), "one\n").unwrap();
        git_in(root, &["add", "seed.txt"]);
        git_in(root, &["commit", "-q", "-m", "seed"]);
        git_in(root, &["config", "--local", "core.splitIndex", "true"]);
        match porcelain_v1_z(root, None).expect("status") {
            GixStatus::Fallback(reason) => assert_eq!(reason, "split_index"),
            GixStatus::Porcelain(text) => panic!("expected fallback, got porcelain {text:?}"),
        }
    }

    /// A staged duplicate of a changed file, the shape git reports as a copy
    /// when asked to look for copies.
    fn staged_copy_repo() -> tempfile::TempDir {
        let dir = git_repo();
        let root = dir.path();
        std::fs::write(root.join("orig.txt"), "same\nlines\n").unwrap();
        git_in(root, &["add", "orig.txt"]);
        git_in(root, &["commit", "-q", "-m", "seed"]);
        std::fs::copy(root.join("orig.txt"), root.join("dup.txt")).unwrap();
        std::fs::write(root.join("orig.txt"), "same\nlines\nplus one\n").unwrap();
        git_in(root, &["add", "-A"]);
        dir
    }

    /// Wherever either key asks for copies, gix and git disagree about a
    /// staged copy (gix says `A  dup.txt` under `status.renames=copies`, and
    /// `C  dup.txt` without the source's `M` under `diff.renames=copies` with
    /// `status.renames=true`, where git says the opposite each time), so the
    /// repository is read by git, through either key, in either case.
    #[test]
    fn configured_copy_detection_triggers_fallback() {
        for settings in [
            &[("status.renames", "copies")][..],
            &[("status.renames", "Copy")][..],
            &[("diff.renames", "copies")][..],
            &[("diff.renames", "copies"), ("status.renames", "true")][..],
        ] {
            let dir = staged_copy_repo();
            for (key, value) in settings {
                git_in(dir.path(), &["config", "--local", key, value]);
            }
            match porcelain_v1_z(dir.path(), None).expect("status") {
                GixStatus::Fallback(reason) => {
                    assert_eq!(reason, "copy_detection", "{settings:?}")
                }
                GixStatus::Porcelain(text) => {
                    panic!("{settings:?}: expected fallback, got porcelain {text:?}")
                }
            }
        }
    }

    /// Renames without copies stay in-process, and agree with git on the
    /// staged duplicate: an addition, and the source's own modification.
    #[test]
    fn renames_without_copies_stay_in_process() {
        let dir = staged_copy_repo();
        git_in(dir.path(), &["config", "--local", "status.renames", "true"]);
        match porcelain_v1_z(dir.path(), None).expect("status") {
            GixStatus::Porcelain(text) => assert_eq!(text, "A  dup.txt\0M  orig.txt\0"),
            GixStatus::Fallback(reason) => panic!("unexpected fallback: {reason}"),
        }
    }
}
