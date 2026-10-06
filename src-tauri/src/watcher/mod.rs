pub mod debouncer;

pub use debouncer::RepoFileWatcher;

use crate::engine::git_cli::{
    git_text_with_timeout, resolve_git_common_dir, resolve_git_dir, validate_repo,
};
use notify::Event;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};

pub const MAX_WATCHES: usize = 24;

pub struct WatchSession {
    stop: Arc<AtomicBool>,
    retired: Option<std::sync::mpsc::Receiver<Result<(), String>>>,
    /// Path spellings that identified this session when it was created:
    /// the canonical map key plus whatever raw path the caller supplied.
    /// Kept so `unwatch` can still resolve the slot after the watched
    /// directory disappears and canonicalization starts failing (which
    /// would otherwise leak one of MAX_WATCHES slots forever).
    aliases: Vec<String>,
}

impl Drop for WatchSession {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

#[derive(Clone, Default)]
pub struct WatcherState {
    /// Shared with each watch thread so it can reap its own session when the
    /// repository vanishes under it (audit D1). An Arc keeps that handoff
    /// cheap while every other caller keeps using `&WatcherState`.
    sessions: std::sync::Arc<Mutex<HashMap<String, WatchSession>>>,
}

#[derive(Clone, Serialize)]
pub struct RepoChangedPayload {
    pub path: String,
}

impl WatcherState {
    fn lock_sessions(&self) -> Result<MutexGuard<'_, HashMap<String, WatchSession>>, String> {
        self.sessions
            .lock()
            .map_err(|e| format!("Watcher lock poisoned: {}", e))
    }

    /// Drops a reserved slot only if it is still the session we inserted.
    /// Used when thread spawn fails after the sessions lock has been released.
    fn abandon_watch_slot(&self, key: &str, stop: &Arc<AtomicBool>) -> Result<(), String> {
        let mut guard = self.lock_sessions()?;
        let same = guard
            .get(key)
            .is_some_and(|session| Arc::ptr_eq(&session.stop, stop));
        if same {
            guard.remove(key);
        }
        Ok(())
    }
}

fn insert_watch_session(
    sessions: &mut HashMap<String, WatchSession>,
    key: &str,
    caller_paths: &[String],
    retired: Option<std::sync::mpsc::Receiver<Result<(), String>>>,
) -> Result<Option<Arc<AtomicBool>>, String> {
    if sessions.contains_key(key) {
        return Ok(None);
    }
    if sessions.len() >= MAX_WATCHES {
        return Err(format!("Too many watched repositories (max {MAX_WATCHES})"));
    }
    let mut aliases = vec![key.to_string()];
    for path in caller_paths {
        if !aliases.contains(path) {
            aliases.push(path.clone());
        }
    }
    let stop = Arc::new(AtomicBool::new(false));
    sessions.insert(
        key.to_string(),
        WatchSession {
            stop: stop.clone(),
            retired,
            aliases,
        },
    );
    Ok(Some(stop))
}

/// Keys that may identify the same watch slot as `repo_path`.
///
/// Relative paths are never canonicalized against the process cwd — that
/// would make `unwatch(".")` clobber a watch of whatever directory the
/// backend happens to be running in.
fn watch_lookup_keys(repo_path: &str) -> Vec<String> {
    let mut keys = Vec::with_capacity(3);
    let mut push = |value: String| {
        if !keys.iter().any(|existing| existing == &value) {
            keys.push(value);
        }
    };
    push(repo_path.to_string());
    let path = Path::new(repo_path);
    if !path.is_absolute() {
        return keys;
    }
    if let Ok(canonical) = path.canonicalize() {
        push(canonical.to_string_lossy().into_owned());
    }
    if let Ok(validated) = validate_repo(repo_path) {
        push(validated.to_string_lossy().into_owned());
    }
    keys
}

/// Quiet period before a pending refresh is emitted.
const DEBOUNCE_QUIET: Duration = Duration::from_millis(400);
/// Upper bound on how long a pending refresh may be postponed by continuous
/// churn: once the FIRST unemitted event is this old, emit even though events
/// keep arriving, so a busy repo never goes stale on screen.
const DEBOUNCE_MAX_WAIT: Duration = Duration::from_millis(2000);

/// How many ignored directory prefixes one watch will remember. A longer
/// `ls-files` answer is a partial result: the prefixes that fit still apply,
/// and [`IgnoreRules::load_error`] says the rest were not recorded.
const MAX_IGNORE_PREFIXES: usize = 4096;
/// The watch thread must not sit on `git ls-files` for the full git timeout
/// while filesystem events queue behind it.
const IGNORE_QUERY_TIMEOUT: Duration = Duration::from_secs(3);
/// A failed ignore query is retried on this period. Retrying every debounce
/// tick would itself be a git storm.
const IGNORE_RETRY: Duration = Duration::from_secs(30);

/// Directory prefixes `git ls-files` confirmed, plus the built-in build-dir
/// names applied in [`is_build_dir_name`]. An empty prefix list is the
/// built-in names only — a failed or truncated query must not look like
/// "nothing is ignored" and must not look like "everything is ignored".
struct IgnoreRules {
    /// Folded, `/`-separated directory prefixes (`scratch`, `pkg/generated`).
    prefixes: HashSet<String>,
    /// `None` only when the query finished and the prefix list was complete.
    load_error: Option<String>,
}

impl IgnoreRules {
    /// Rules with no repository-specific prefixes. Tests only: production
    /// always goes through [`IgnoreRules::load`], whose failure path records
    /// its reason instead of returning an empty set that looks complete.
    #[cfg(test)]
    fn builtin() -> Self {
        Self {
            prefixes: HashSet::new(),
            load_error: None,
        }
    }

    /// One bounded `git ls-files` for directories the ignore rules already cover.
    /// A failed or truncated query keeps the built-in build-dir names and
    /// records why: it does not pretend the tree was scanned and found clean.
    fn load(repo: &Path) -> Self {
        match git_text_with_timeout(
            repo,
            &[
                "ls-files",
                "-o",
                "-i",
                "--directory",
                "--exclude-standard",
                "-z",
            ],
            IGNORE_QUERY_TIMEOUT,
        ) {
            Ok(stdout) => {
                let (prefixes, truncated) = parse_ignored_directories(&stdout);
                if truncated {
                    let error = format!(
                        "ignore directory list truncated at {MAX_IGNORE_PREFIXES}; further ignored directories were not recorded"
                    );
                    log::warn!(target: "watcher", "{}: {error}", repo.display());
                    Self {
                        prefixes,
                        load_error: Some(error),
                    }
                } else {
                    Self {
                        prefixes,
                        load_error: None,
                    }
                }
            }
            Err(error) => {
                log::warn!(
                    target: "watcher",
                    "gitignore query failed for {}: {error}",
                    repo.display()
                );
                Self {
                    prefixes: HashSet::new(),
                    load_error: Some(error),
                }
            }
        }
    }
}

fn parse_ignored_directories(stdout: &str) -> (HashSet<String>, bool) {
    let mut prefixes = HashSet::new();
    let mut truncated = false;
    for field in stdout.split('\0') {
        if field.is_empty() || !(field.ends_with('/') || field.ends_with('\\')) {
            continue;
        }
        let folded = field
            .trim_end_matches(['/', '\\'])
            .trim_start_matches("./")
            .to_ascii_lowercase();
        if folded.is_empty() {
            continue;
        }
        if prefixes.len() >= MAX_IGNORE_PREFIXES {
            truncated = true;
            break;
        }
        prefixes.insert(folded);
    }
    (prefixes, truncated)
}

/// Directories that are build output by construction. Ordinary source names
/// (`build`, `out`, `dist`, `coverage`) are not in this list: an un-ignored
/// tree with those names is source, and gitignore covers them when it should.
///
/// Cargo `target`, `target2`, `target-release`, `target_debug`. Not
/// `target.rs` and not `targeting`.
fn is_build_dir_name(name: &str) -> bool {
    let folded = name.to_ascii_lowercase();
    if matches!(
        folded.as_str(),
        "node_modules" | "__pycache__" | ".next" | ".nuxt" | ".turbo" | ".parcel-cache" | ".gradle"
    ) {
        return true;
    }
    let Some(rest) = folded.strip_prefix("target") else {
        return false;
    };
    rest.is_empty()
        || rest.starts_with('-')
        || rest.starts_with('_')
        || (!rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()))
}

fn is_build_or_ignored_path(relative: &Path, rules: &IgnoreRules) -> bool {
    let mut accumulated = String::new();
    for component in relative.components() {
        let name = component.as_os_str().to_string_lossy();
        if is_build_dir_name(&name) {
            return true;
        }
        if !accumulated.is_empty() {
            accumulated.push('/');
        }
        accumulated.push_str(&name.to_ascii_lowercase());
        if rules.prefixes.contains(&accumulated) {
            return true;
        }
    }
    false
}

/// Reload ignore rules after a real `.gitignore` edit, but not on every
/// keystroke, and retry a failed query on [`IGNORE_RETRY`] rather than on
/// every debounce tick.
fn rules_due(
    rules: &IgnoreRules,
    last_load: Instant,
    now: Instant,
    rule_file_changed: bool,
) -> bool {
    let elapsed = now.saturating_duration_since(last_load);
    if rule_file_changed {
        return elapsed >= SCAN_COALESCE;
    }
    rules.load_error.is_some() && elapsed >= IGNORE_RETRY
}

fn event_updates_ignore_rules(event: &Event) -> bool {
    event.paths.iter().any(
        |path| match path.file_name().and_then(|name| name.to_str()) {
            Some(".gitignore") => true,
            Some("exclude") => path.parent().is_some_and(|parent| parent.ends_with("info")),
            _ => false,
        },
    )
}

/// True when `event` carries at least one path that can move repository state.
///
/// Events whose every path is git-internal refresh noise (see
/// [`is_git_internal_noise`]) or generated-state noise (see
/// [`is_generated_state_noise_in`]) are dropped before they enter the debounce
/// accumulation. Without this gate, editors/build tools churning `.lock`
/// transients or `COMMIT_EDITMSG` force a full app refresh every
/// [`DEBOUNCE_MAX_WAIT`] indefinitely — the anti-starvation bound turns pure
/// noise into a constant refresh loop. The same loop appears when the live
/// index vacuums `.devcouncil` sqlite: that write is not git-internal, so it
/// used to emit `repo-changed`, which rebuilt the index, which vacuumed again.
/// Events without any path cannot be classified, so they count as signal
/// (fail open toward refreshing).
///
/// `worktree_canonical` is resolved once per debounce batch so a linked
/// worktree's `internal_roots` storm never pays `realpath` per event. Paths
/// under `internal_roots` are never generated-state noise (they are either
/// git noise or signal by construction), so the alias walk is skipped for
/// them entirely.
fn event_has_signal(
    event: &Event,
    internal_roots: &[PathBuf],
    worktree: &Path,
    worktree_canonical: Option<&Path>,
    rules: &IgnoreRules,
) -> bool {
    if event.paths.is_empty() {
        return true;
    }
    event.paths.iter().any(|path| {
        if is_git_internal_noise(path, internal_roots) {
            return false;
        }
        // Linked-worktree ref/object writes live under the common git dir,
        // outside the worktree checkout. They are never `.devcouncil` noise
        // and must not trigger the path-alias canonicalize walk.
        if internal_roots.iter().any(|root| path.starts_with(root)) {
            return true;
        }
        if is_generated_state_noise_cached(path, worktree, worktree_canonical) {
            return false;
        }
        match worktree_relative(path, worktree, worktree_canonical) {
            Some(relative) => !is_build_or_ignored_path(&relative, rules),
            // The worktree directory itself is what FSEvents reports when a
            // storm coalesces. It names no file. A `..` path still fails open:
            // refusing to classify an escape is not the same as calling it noise.
            None => !is_worktree_root(path, worktree, worktree_canonical),
        }
    })
}

/// True when `path` is generated per-worktree state that GitPulse (or a
/// sibling indexer) writes as a *consequence* of a refresh, never as the
/// worktree change that should start one.
///
/// Live index vacuums, DevCouncil ledgers, and GitNexus indexes live under
/// `.devcouncil`, `.devmap`, and `.gitnexus`. A worktree watch is
/// non-recursive, but those top-level directories still fire, and nested
/// writes can leak through FSEvents. Ledger appends already have their own
/// `ledger-appended` event; ignoring `.devcouncil` here does not drop that
/// channel.
///
/// Only the first path component *inside* the worktree counts. Scanning the
/// absolute path would freeze live refresh for a worktree whose own path
/// contains `.devcouncil` (legal, if unusual): every event would match. Nested
/// lookalikes such as `src/.gitnexus/` or a branch named `.devcouncil/topic`
/// stay signal.
///
/// Tracked edits under `.devcouncil/config.yaml` therefore no longer fire
/// `repo-changed` until the commit moves `.git/index` — the state-dir gate
/// owns the top-level name, and git's index event carries the commit signal.
///
/// `strip_prefix` is not enough on its own. macOS FSEvents often spell `/var`
/// as `/private/var`, and some backends emit a relative `.devcouncil/...`.
/// Those must still classify against the worktree-relative first component,
/// or live-index vacuums reopen the refresh loop.
#[cfg(test)]
pub(crate) fn is_generated_state_noise_in(path: &Path, worktree: &Path) -> bool {
    let canonical = worktree.canonicalize().ok();
    is_generated_state_noise_cached(path, worktree, canonical.as_deref())
}

fn is_generated_state_noise_cached(
    path: &Path,
    worktree: &Path,
    worktree_canonical: Option<&Path>,
) -> bool {
    first_worktree_relative_component(path, worktree, worktree_canonical)
        .is_some_and(|name| is_generated_state_dir(&name))
}

fn is_worktree_root(path: &Path, worktree: &Path, canonical: Option<&Path>) -> bool {
    path == worktree || canonical.is_some_and(|root| path == root)
}

fn path_has_parent_dir(path: &Path) -> bool {
    path.components().any(|c| matches!(c, Component::ParentDir))
}

fn first_worktree_relative_component(
    path: &Path,
    worktree: &Path,
    worktree_canonical: Option<&Path>,
) -> Option<OsString> {
    worktree_relative(path, worktree, worktree_canonical).and_then(|relative| {
        relative
            .components()
            .next()
            .map(|component| component.as_os_str().to_os_string())
    })
}

fn worktree_relative(
    path: &Path,
    worktree: &Path,
    worktree_canonical: Option<&Path>,
) -> Option<PathBuf> {
    // Resolve no lexical escape as noise, including paths whose prefix matches.
    if path_has_parent_dir(path) || path_has_parent_dir(worktree) {
        return None;
    }
    if let Ok(relative) = path.strip_prefix(worktree) {
        // The worktree root itself is not a path inside it.
        relative.components().next()?;
        return Some(relative.to_path_buf());
    }
    if path.is_relative() && worktree.is_absolute() {
        return Some(path.to_path_buf());
    }
    // A shared suffix is not proof of repository identity. Only filesystem
    // identity can establish an alias. Canonicalize the worktree once (caller)
    // and probe only the first *existing* ancestor of the event path — deleted
    // leaves still resolve through a live parent. The first remaining
    // component may live in the canonicalized ancestor, so it is owned.
    let canonical = worktree_canonical?;
    let existing = path
        .ancestors()
        .find(|ancestor| !ancestor.as_os_str().is_empty() && ancestor.exists())?;
    let existing_canon = existing.canonicalize().ok()?;
    let relative_from_wt = existing_canon.strip_prefix(canonical).ok()?;
    let mut relative = relative_from_wt.to_path_buf();
    if let Ok(rest) = path.strip_prefix(existing) {
        if !rest.as_os_str().is_empty() {
            relative.push(rest);
        }
    }
    relative.components().next()?;
    Some(relative)
}

/// GitPulse-owned indexer state beside DevMap's [`devmap_query::paths::STATE_DIR_NAMES`].
const GITNEXUS_STATE_DIR: &str = ".gitnexus";

fn is_generated_state_dir(name: &OsStr) -> bool {
    // Windows and some macOS volumes fold case; FSEvents can spell the same
    // directory `.DevCouncil`. Canonical names in `devmap_query::paths` are
    // lowercase, so fold here before consulting them.
    let Some(raw) = name.to_str() else {
        return false;
    };
    let folded = raw.to_ascii_lowercase();
    if devmap_query::paths::is_state_dir_name(&folded) {
        return true;
    }
    folded == GITNEXUS_STATE_DIR
}

/// True when `path` sits inside one of the watched git directories (the
/// resolved git dir plus the shared common dir of linked worktrees) AND names
/// a transient git-internals artifact whose churn never moves repo state:
///
/// - `*.lock`: lockfiles (`index.lock`, `config.lock`, `packed-refs.lock`,
///   `COMMIT_EDITMSG.lock`) that exist only for the duration of one git write;
///   real index/ref changes also emit `index` / `refs/` events themselves.
/// - `COMMIT_EDITMSG`, `MERGE_MSG`: message files typed into while no commit
///   has been made yet; an actual commit also moves `refs/heads/...`.
/// - `ORIG_HEAD`, `FETCH_HEAD`: transient pointers; real ref moves also emit
///   `refs/` events.
/// - `gc.log*`: garbage-collection progress logs.
/// - `fsmonitor--daemon/**` and `fsmonitor--daemon.ipc`: git's built-in
///   filesystem monitor (`core.fsmonitor=true`). Every `git status` makes the
///   daemon write a cookie under `fsmonitor--daemon/cookies/` to synchronise
///   with its event stream, so a refresh that runs `git status` would announce
///   its own `repo-changed` and run again — the ~1 s storm measured against
///   the one watched repository that had fsmonitor on. The daemon never moves
///   refs, the index or the worktree. A linked worktree's daemon lives under
///   `worktrees/<name>/` of the common directory, which the main worktree
///   watches recursively, so that shape is the daemon's too.
/// - `sharedindex.*`: with `core.splitIndex`, every read of the index — the
///   `git status` and `git diff` a refresh runs — touches the shared index's
///   mtime so it does not expire, even with optional locks off. A real index
///   change in split-index mode always rewrites `index` itself.
///
/// The filter is deliberately scoped to the git directories: identically
/// named files in the worktree root are tracked content with different
/// semantics (`Cargo.lock`, `yarn.lock`, `poetry.lock` are dependency state,
/// not transients) and must keep firing `repo-changed`.
pub(crate) fn is_git_internal_noise(path: &Path, internal_roots: &[std::path::PathBuf]) -> bool {
    if !internal_roots.iter().any(|root| path.starts_with(root)) {
        return false;
    }
    // An event naming the git directory ITSELF, rather than something inside
    // it, carries no information about what changed — and that same directory
    // is watched recursively, so whatever changed delivers its own event. The
    // worktree-root watch is non-recursive, and on Windows
    // ReadDirectoryChangesW reports a direct child directory for any write
    // beneath it: every `.git/index.lock` touch arrived a second time as a
    // bare `<repo>/.git` event, which passed this filter and made pure
    // git-internal churn read as repository change on Windows only.
    if internal_roots.iter().any(|root| path == root) {
        return true;
    }
    if is_fsmonitor_daemon_path(path, internal_roots) {
        return true;
    }
    is_noise_leaf_name(path)
}

/// The daemon's state directory (`fsmonitor--daemon`, cookies included) or its
/// IPC socket, either directly below a git directory or below a linked
/// worktree's private directory, `worktrees/<name>/`, inside the common one.
/// Only those positions are consulted, so a ref that happens to be named
/// `refs/heads/fsmonitor--daemon` still moves the repository.
fn is_fsmonitor_daemon_path(path: &Path, internal_roots: &[PathBuf]) -> bool {
    let is_daemon = |component: Option<Component<'_>>| {
        component.is_some_and(|c| {
            matches!(
                c.as_os_str().to_str(),
                Some("fsmonitor--daemon" | "fsmonitor--daemon.ipc")
            )
        })
    };
    internal_roots.iter().any(|root| {
        let Ok(relative) = path.strip_prefix(root) else {
            return false;
        };
        let mut components = relative.components();
        let first = components.next();
        if is_daemon(first) {
            return true;
        }
        first.is_some_and(|c| c.as_os_str() == "worktrees")
            && components.next().is_some()
            && is_daemon(components.next())
    })
}

/// Leaf-name half of the noise rules, applied once the path is known to live
/// inside a git directory. Paths under `refs/` always matter (branch/tag
/// moves), even when a component happens to look like noise. A directory that
/// merely shares a noise name (`refs`-adjacent tooling, odd user dirs) is
/// kept: the rules target transient FILES, so a deny-list hit is confirmed as
/// a non-directory via symlink metadata before it is dropped.
fn is_noise_leaf_name(path: &Path) -> bool {
    // Conservative containment check: any literal `refs` component wins over
    // the name rules below.
    if path.components().any(|c| c.as_os_str() == "refs") {
        return false;
    }
    let Some(name) = path.file_name() else {
        return false;
    };
    let name = name.to_string_lossy();
    let matches_deny_list = name.ends_with(".lock")
        || matches!(
            &*name,
            "COMMIT_EDITMSG" | "ORIG_HEAD" | "FETCH_HEAD" | "MERGE_MSG"
        )
        || name.starts_with("gc.log")
        || name.starts_with("sharedindex.");
    if !matches_deny_list {
        return false;
    }
    // Deleted-already paths (stat fails) are transient-file churn by
    // definition — exactly what this filter exists to drop.
    !std::fs::symlink_metadata(path)
        .map(|meta| meta.is_dir())
        .unwrap_or(false)
}

/// Whether the loop owes an emission at `now`: events have been quiet for
/// [`DEBOUNCE_QUIET`], or the oldest unemitted event has waited
/// [`DEBOUNCE_MAX_WAIT`] — whichever comes first.
fn should_emit(
    pending: bool,
    last_event: Instant,
    first_pending: Option<Instant>,
    now: Instant,
) -> bool {
    if !pending {
        return false;
    }
    let quiet = now.duration_since(last_event) >= DEBOUNCE_QUIET;
    let max_wait =
        first_pending.is_some_and(|first| now.duration_since(first) >= DEBOUNCE_MAX_WAIT);
    quiet || max_wait
}

/// At most one scan per repo per [`DEBOUNCE_MAX_WAIT`], even when real edits
/// keep arriving. The quiet period still releases the first scan; the next
/// one waits out the coalesce window so a busy checkout cannot refresh at
/// the debounce rate for hours.
const SCAN_COALESCE: Duration = DEBOUNCE_MAX_WAIT;

fn scan_is_due(
    pending: bool,
    last_event: Instant,
    first_pending: Option<Instant>,
    last_scan: Option<Instant>,
    now: Instant,
) -> bool {
    should_emit(pending, last_event, first_pending, now)
        && last_scan.is_none_or(|previous| now.saturating_duration_since(previous) >= SCAN_COALESCE)
}

/// Everything the debounce loop needs to know about WHERE it is watching and
/// WHAT to emit. Bundled so [`run_watch_loop`] stays under the argument cap:
/// `git_dir` is the liveness probe target, `internal_roots` scopes the
/// noise filter (always contains `git_dir`, plus the common dir for linked
/// worktrees), and `emit_path` is the payload handed to `on_change`.
struct WatchLoopContext {
    git_dir: std::path::PathBuf,
    internal_roots: Vec<std::path::PathBuf>,
    emit_path: String,
}

/// Why the debounce loop ended. The spawn-site supervisor logs every
/// variant: a watch thread that dies silently leaves a stale UI and a leaked
/// session slot — the failure mode nobody can diagnose after the fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WatchLoopExit {
    /// `stop` flag set: deliberate teardown (unwatch).
    Stopped,
    /// Watched git directory confirmed gone; the loop already reaped its own
    /// session slot.
    DeadRepo,
    /// Event channel closed: the notify backend is gone and nothing will
    /// ever fire again for this path until it is re-watched.
    EventStreamClosed,
}

fn run_watch_loop<F>(
    watcher: &RepoFileWatcher,
    ctx: WatchLoopContext,
    stop: Arc<AtomicBool>,
    sessions: Option<std::sync::Arc<Mutex<HashMap<String, WatchSession>>>>,
    session_stop: Arc<AtomicBool>,
    on_change: F,
) -> WatchLoopExit
where
    F: Fn(String),
{
    let WatchLoopContext {
        git_dir,
        internal_roots,
        emit_path: path,
    } = ctx;
    let mut pending = false;
    let mut last_event = Instant::now();
    let mut first_pending: Option<Instant> = None;
    let mut last_scan: Option<Instant> = None;
    let mut rules = IgnoreRules::load(Path::new(&path));
    let mut last_rules_load = Instant::now();
    // When the watched git directory is deleted (repo moved/removed), notify
    // keeps delivering remove/error events forever, and the settle timer would
    // still fire one last `repo-changed` for the corpse. Liveness is therefore
    // checked at the exact point of emission: a dead path can never be
    // announced, and after the miss counter confirms it stays dead, the loop
    // exits and reaps its own session instead of hammering a dead path.
    const DEAD_MISSES: u32 = 3;
    let mut dead_misses: u32 = 0;
    let mut exit = WatchLoopExit::Stopped;
    'outer: while !stop.load(Ordering::Relaxed) {
        if !git_dir.exists() {
            dead_misses += 1;
            if dead_misses >= DEAD_MISSES {
                exit = WatchLoopExit::DeadRepo;
                break;
            }
        } else {
            dead_misses = 0;
        }
        match watcher.receiver.recv_timeout(Duration::from_millis(200)) {
            Ok(Ok(event)) => {
                // Noise gate before any accumulation: a batch whose every
                // path is git-internal churn must not open or extend the
                // pending window. Drained leftovers are classified too, so a
                // real event hiding behind noise in the same queue is never
                // lost. Backend errors carry no classifiable path and count
                // as signal (fail open toward refreshing), preserving the
                // pre-filter treatment.
                let worktree = Path::new(&path);
                // One canonicalize per batch — never per drained leftover.
                let worktree_canonical = worktree.canonicalize().ok();
                let mut reload_rules = event_updates_ignore_rules(&event);
                let mut significant = event_has_signal(
                    &event,
                    &internal_roots,
                    worktree,
                    worktree_canonical.as_deref(),
                    &rules,
                );
                for leftover in watcher.receiver.try_iter() {
                    significant |= match leftover {
                        Ok(event) => {
                            reload_rules |= event_updates_ignore_rules(&event);
                            event_has_signal(
                                &event,
                                &internal_roots,
                                worktree,
                                worktree_canonical.as_deref(),
                                &rules,
                            )
                        }
                        Err(_) => true,
                    };
                }
                if rules_due(&rules, last_rules_load, Instant::now(), reload_rules) {
                    rules = IgnoreRules::load(worktree);
                    last_rules_load = Instant::now();
                }
                if !significant {
                    continue;
                }
                if !pending {
                    first_pending = Some(Instant::now());
                }
                pending = true;
                last_event = Instant::now();
                if scan_is_due(
                    pending,
                    last_event,
                    first_pending,
                    last_scan,
                    Instant::now(),
                ) {
                    if !git_dir.exists() {
                        exit = WatchLoopExit::DeadRepo;
                        break 'outer;
                    }
                    on_change(path.clone());
                    pending = false;
                    first_pending = None;
                    last_scan = Some(Instant::now());
                }
            }
            // A notify backend error carries no classifiable path; per the
            // fail-open rule above it counts as signal — open or extend the
            // pending window so the missed-events repo still refreshes.
            Ok(Err(_)) => {
                if !pending {
                    first_pending = Some(Instant::now());
                }
                pending = true;
                last_event = Instant::now();
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if rules_due(&rules, last_rules_load, Instant::now(), false) {
                    rules = IgnoreRules::load(Path::new(&path));
                    last_rules_load = Instant::now();
                }
                if scan_is_due(
                    pending,
                    last_event,
                    first_pending,
                    last_scan,
                    Instant::now(),
                ) {
                    if !git_dir.exists() {
                        exit = WatchLoopExit::DeadRepo;
                        break 'outer;
                    }
                    on_change(path.clone());
                    pending = false;
                    first_pending = None;
                    last_scan = Some(Instant::now());
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                exit = WatchLoopExit::EventStreamClosed;
                break;
            }
        }
    }
    if matches!(
        exit,
        WatchLoopExit::DeadRepo | WatchLoopExit::EventStreamClosed
    ) {
        // A vanished repo or closed backend relinquishes its session, but only
        // while it is still ours —
        // the same ptr_eq discipline abandon_watch_slot uses, so an unwatch
        // plus rewatch of the same path is never torn down by this ghost.
        if let Some(sessions) = &sessions {
            let mut guard = sessions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if guard
                .get(&path)
                .is_some_and(|session| Arc::ptr_eq(&session.stop, &session_stop))
            {
                guard.remove(&path);
            }
        }
    }
    exit
}

/// Flattens a panic payload for log output (mirror of the logging hook's
/// downcast chain; kept local so the watcher has no logging-internal deps).
fn panic_payload_text(panic: &(dyn std::any::Any + Send)) -> String {
    panic
        .downcast_ref::<&str>()
        .map(|s| (*s).to_string())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "<non-string panic payload>".to_string())
}

/// Debounced git-directory watcher that emits `repo-changed` after writes settle.
///
/// Concurrent watches are keyed by canonical repository path. Re-watching an
/// existing path is idempotent. Returns the canonical path used as the map key.
pub fn start_watch(
    app: AppHandle,
    state: &WatcherState,
    repo_path: String,
) -> Result<String, String> {
    start_watch_inner(state, repo_path, move |path| {
        if let Err(e) = app.emit("repo-changed", RepoChangedPayload { path: path.clone() }) {
            log::warn!(target: "watcher", "repo-changed emit failed for {path}: {e}");
        }
    })
}

pub(crate) fn start_watch_inner<F>(
    state: &WatcherState,
    repo_path: String,
    on_change: F,
) -> Result<String, String>
where
    F: Fn(String) + Send + 'static,
{
    #[cfg(test)]
    eprintln!("watch setup {repo_path}: validate repository");
    let canonical = validate_repo(&repo_path)?;
    let key = canonical.to_string_lossy().into_owned();
    #[cfg(test)]
    eprintln!("watch setup {repo_path}: resolve git directory");
    let git_dir = resolve_git_dir(&canonical)?;
    // Bare repos have no separate worktree root (git dir == repo); a normal
    // checkout and a linked worktree both do. The non-recursive worktree
    // watch is what makes unstaged edits fire `repo-changed`.
    let worktree_root = if git_dir == canonical {
        None
    } else {
        Some(canonical.clone())
    };

    {
        let guard = state.lock_sessions()?;
        if guard.contains_key(&key) {
            return Ok(key);
        }
        if guard.len() >= MAX_WATCHES {
            return Err(format!("Too many watched repositories (max {MAX_WATCHES})"));
        }
    }

    // Linked worktrees keep refs/heads in the COMMON dir; watch it too so
    // checkouts made in any worktree refresh every view of the repo.
    #[cfg(test)]
    eprintln!("watch setup {repo_path}: resolve common directory");
    let common_dir = match resolve_git_common_dir(&canonical) {
        Ok(dir) => Some(dir),
        Err(e) => {
            log::debug!(
                target: "watcher",
                "linked-worktree ref watching degraded for {key}: {e}"
            );
            None
        }
    };
    #[cfg(test)]
    eprintln!("watch setup {repo_path}: register native backend");
    let mut watcher =
        RepoFileWatcher::watch_repo(&git_dir, worktree_root.as_deref(), common_dir.as_deref())?;

    #[cfg(test)]
    eprintln!("watch setup {repo_path}: reserve and launch session");
    let (retired_tx, retired_rx) = std::sync::mpsc::channel();
    let stop = {
        let mut guard = state.lock_sessions()?;
        match insert_watch_session(
            &mut guard,
            &key,
            std::slice::from_ref(&repo_path),
            Some(retired_rx),
        )? {
            None => return Ok(key),
            Some(stop) => stop,
        }
    };

    // Release the sessions mutex before spawn. Holding it across spawn can
    // deadlock if the watch thread (or `on_change`) needs the same lock.
    // The noise filter needs every watched git-internal root (private git dir
    // plus the shared common dir) to scope its rules; the worktree root is
    // deliberately NOT in this set — worktree-top-level files are content.
    let mut internal_roots = vec![git_dir.clone()];
    if let Some(common) = &common_dir {
        if common != &git_dir && !internal_roots.contains(common) {
            internal_roots.push(common.clone());
        }
    }
    let emit_path = key.clone();
    let thread_stop = stop.clone();
    let session_stop = stop.clone();
    let loop_sessions = state.sessions.clone();
    if let Err(e) = thread::Builder::new()
        .name("gitpulse-fs-watch".into())
        .spawn(move || {
            // The loop runs under catch_unwind so a panic inside it can
            // neither kill its thread silently nor leak the session slot:
            // the supervisor logs the outcome and, on panic, performs the
            // same ptr_eq-guarded reap the DeadRepo path uses.
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run_watch_loop(
                    &watcher,
                    WatchLoopContext {
                        git_dir,
                        internal_roots,
                        emit_path: emit_path.clone(),
                    },
                    thread_stop,
                    Some(loop_sessions.clone()),
                    session_stop.clone(),
                    on_change,
                )
            }));
            match result {
                Ok(WatchLoopExit::Stopped) => {
                    log::debug!(target: "watcher", "watch on {emit_path} stopped by request");
                }
                Ok(WatchLoopExit::DeadRepo) => {
                    log::info!(target: "watcher", "watch on {emit_path} ended: repository disappeared");
                }
                Ok(WatchLoopExit::EventStreamClosed) => {
                    log::warn!(
                        target: "watcher",
                        "watch event stream closed unexpectedly for {emit_path}; \
                         UI refresh for this repo is dead until it is re-watched"
                    );
                }
                Err(panic) => {
                    log::error!(
                        target: "watcher",
                        "fs-watch thread PANICKED for {emit_path}: {}; reaping session slot",
                        panic_payload_text(&*panic)
                    );
                    if let Ok(mut guard) = loop_sessions.lock() {
                        if guard
                            .get(&emit_path)
                            .is_some_and(|session| Arc::ptr_eq(&session.stop, &session_stop))
                        {
                            guard.remove(&emit_path);
                        }
                    }
                }
            }
            // Release the callback and native handles before acknowledging
            // retirement. No sessions mutex is held while waiting here.
            let retired = watcher.shutdown();
            if let Err(error) = &retired {
                log::warn!(target: "watcher", "{emit_path}: {error}");
            }
            let _ = retired_tx.send(retired);
        })
    {
        let _ = state.abandon_watch_slot(&key, &stop);
        return Err(format!("Failed to start watcher: {}", e));
    }
    Ok(key)
}

/// Purely lexical normalizations of `repo_path` that stay valid even when
/// the path no longer exists: trailing slashes and inner `.` components are
/// stripped without touching the filesystem. Symlinked prefixes (macOS
/// `/var` → `/private/var`) cannot be resolved lexically — that gap is
/// covered by the watch-time aliases recorded on each session.
fn lexical_normalizations(repo_path: &str) -> Vec<String> {
    let mut out = Vec::with_capacity(2);
    let mut push = |value: String| {
        if !value.is_empty() && !out.iter().any(|existing| existing == &value) {
            out.push(value);
        }
    };
    push(repo_path.to_string());
    // `components()` drops trailing slashes and non-leading `.` segments,
    // so `path/./` collapses to `path` with no filesystem access.
    let normalized = Path::new(repo_path)
        .components()
        .collect::<std::path::PathBuf>()
        .to_string_lossy()
        .into_owned();
    push(normalized);
    out
}

pub fn unwatch(state: &WatcherState, repo_path: String) -> Result<(), String> {
    let keys = watch_lookup_keys(&repo_path);
    let mut guard = state.lock_sessions()?;
    let mut retired = Vec::new();
    // Pass 1: exact matches against map keys. Covers the healthy case where
    // canonicalization still works.
    for key in keys {
        if let Some(session) = guard.remove(&key) {
            retired.push(session);
        }
    }
    // Pass 2: post-deletion recovery. The directory is gone, so the
    // canonicalize()/validate_repo() lookups above failed; fall back to
    // lexical variants matched against both the map keys and every spelling
    // recorded when each session was created.
    let lexical = lexical_normalizations(&repo_path);
    let stale: Vec<String> = guard
        .iter()
        .filter(|(key, session)| {
            lexical.iter().any(|candidate| {
                key == &candidate || session.aliases.iter().any(|alias| alias == candidate)
            })
        })
        .map(|(key, _)| key.clone())
        .collect();
    for key in stale {
        if let Some(session) = guard.remove(&key) {
            retired.push(session);
        }
    }
    drop(guard);
    retire_sessions(retired)
}

pub fn unwatch_all(state: &WatcherState) -> Result<(), String> {
    let retired = state
        .lock_sessions()?
        .drain()
        .map(|(_, session)| session)
        .collect();
    retire_sessions(retired)
}

/// Signal every session first, then share one deadline across all receipts.
/// Reaping and callbacks may use the sessions mutex, so callers release it
/// before entering this function. A timeout is a failed shutdown, never success.
fn retire_sessions(mut sessions: Vec<WatchSession>) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(3);
    for session in &sessions {
        session.stop.store(true, Ordering::SeqCst);
    }
    let mut errors = Vec::new();
    for session in &mut sessions {
        if let Some(retired) = session.retired.take() {
            match retired.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(Ok(())) => {}
                Ok(Err(error)) => errors.push(error),
                Err(error) => {
                    errors.push(format!("Watcher retirement was not acknowledged: {error}"))
                }
            }
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::procguard::LockedSpawn;
    use std::path::PathBuf;
    use std::process::Command;
    use tempfile::TempDir;

    fn git_init(dir: &Path, bare: bool) {
        let mut cmd = Command::new("git");
        cmd.arg("init");
        if bare {
            cmd.arg("--bare");
        } else {
            cmd.args(["-b", "main"]);
        }
        let output = cmd.current_dir(dir).output_locked().expect("spawn git");
        assert!(
            output.status.success(),
            "git init failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        crate::test_support::trust_repo(dir);
    }

    use crate::test_support::git_in;

    fn init_linked_worktree() -> (TempDir, TempDir, std::path::PathBuf) {
        let main = TempDir::new().unwrap();
        git_init(main.path(), false);
        git_in(main.path(), &["commit", "--allow-empty", "-m", "init"]);
        let work_parent = TempDir::new().unwrap();
        let work_path = work_parent.path().join("linked");
        let output = Command::new("git")
            .args([
                "-c",
                "user.name=GitPulse",
                "-c",
                "user.email=gitpulse@test.local",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(["worktree", "add", "-b", "gitpulse-link"])
            .arg(&work_path)
            .current_dir(main.path())
            .output_locked()
            .expect("spawn git worktree");
        assert!(
            output.status.success(),
            "git worktree add failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            work_path.join(".git").is_file(),
            "linked worktree must use a gitfile"
        );
        crate::test_support::trust_repo(&work_path);
        (main, work_parent, work_path)
    }

    /// A relative spelling of `target` as seen from `from`, both canonical.
    ///
    /// Used where a test needs a relative path that really does resolve to
    /// somewhere: the honest way to prove a lookup does *not* canonicalize is to
    /// hand it a relative path that would find the target if it did.
    ///
    /// `canonicalize` on Windows prefixes `\\?\`. `Path::push` treats that
    /// prefix as absolute and replaces everything already pushed, so a
    /// comparison of a verbatim path against a plain one comes back absolute.
    /// Strip it before walking components.
    fn without_verbatim_prefix(path: &Path) -> PathBuf {
        let text = path.as_os_str().to_string_lossy();
        let rest = text.strip_prefix("\\\\?\\").unwrap_or(text.as_ref());
        if let Some(unc) = rest.strip_prefix("UNC\\") {
            return PathBuf::from(format!("\\\\{unc}"));
        }
        PathBuf::from(rest)
    }

    #[test]
    fn a_verbatim_prefix_is_stripped_before_the_relative_walk() {
        let stripped = without_verbatim_prefix(Path::new("\\\\?\\C:\\Users\\temp"));
        let text = stripped.as_os_str().to_string_lossy();
        assert!(
            !text.starts_with("\\\\?\\"),
            "verbatim prefix survived: {text}"
        );
        assert!(text.contains("C:"), "{text}");
    }

    fn relative_to(from: &Path, target: &Path) -> PathBuf {
        let from = without_verbatim_prefix(from);
        let target = without_verbatim_prefix(target);
        let shared = from
            .components()
            .zip(target.components())
            .take_while(|(here, there)| here == there)
            .count();
        let mut relative = PathBuf::new();
        for _ in shared..from.components().count() {
            relative.push("..");
        }
        for part in target.components().skip(shared) {
            relative.push(part);
        }
        relative
    }

    impl WatcherState {
        fn begin_watch_slot(&self, key: &str) -> Result<bool, String> {
            let mut guard = self.lock_sessions()?;
            Ok(insert_watch_session(&mut guard, key, &[], None)?.is_some())
        }

        fn watch_count(&self) -> Result<usize, String> {
            Ok(self.lock_sessions()?.len())
        }

        fn is_watching(&self, key: &str) -> Result<bool, String> {
            Ok(self.lock_sessions()?.contains_key(key))
        }
    }

    #[test]
    fn test_watch_two_repos_unwatch_leaves_the_other() {
        let a = TempDir::new().unwrap();
        let b = TempDir::new().unwrap();
        git_init(a.path(), false);
        git_init(b.path(), false);
        let state = WatcherState::default();

        let ka = start_watch_inner(&state, a.path().to_string_lossy().into_owned(), |_| {})
            .expect("watch a");
        let kb = start_watch_inner(&state, b.path().to_string_lossy().into_owned(), |_| {})
            .expect("watch b");
        assert_ne!(ka, kb);
        assert_eq!(state.watch_count().unwrap(), 2);
        assert!(state.is_watching(&ka).unwrap());
        assert!(state.is_watching(&kb).unwrap());

        let ka_again = start_watch_inner(&state, a.path().to_string_lossy().into_owned(), |_| {})
            .expect("rewatch a");
        assert_eq!(ka_again, ka);
        assert_eq!(
            state.watch_count().unwrap(),
            2,
            "idempotent re-watch must not consume a second slot"
        );

        unwatch(&state, ka.clone()).unwrap();
        assert_eq!(state.watch_count().unwrap(), 1);
        assert!(!state.is_watching(&ka).unwrap());
        assert!(state.is_watching(&kb).unwrap());

        unwatch(&state, "/no/such/watched/repo".into()).unwrap();
        assert_eq!(state.watch_count().unwrap(), 1);

        unwatch_all(&state).unwrap();
        assert_eq!(state.watch_count().unwrap(), 0);
    }

    #[test]
    fn test_watch_cap_idempotent_and_unwatch_all() {
        let state = WatcherState::default();
        for i in 0..MAX_WATCHES {
            let inserted = state
                .begin_watch_slot(&format!("/cap-repo-{i}"))
                .expect("slot");
            assert!(inserted, "unique path {i} should take a slot");
        }
        assert_eq!(state.watch_count().unwrap(), MAX_WATCHES);

        let err = state
            .begin_watch_slot("/cap-repo-overflow")
            .expect_err("25th unique path must fail");
        assert!(
            err.contains("24") || err.to_lowercase().contains("too many"),
            "cap error should mention the limit, got: {err}"
        );
        assert_eq!(state.watch_count().unwrap(), MAX_WATCHES);

        let inserted = state.begin_watch_slot("/cap-repo-0").unwrap();
        assert!(
            !inserted,
            "idempotent re-watch of an existing key must not consume a second slot"
        );
        assert_eq!(state.watch_count().unwrap(), MAX_WATCHES);

        unwatch(&state, "/not-in-the-map".into()).unwrap();
        assert_eq!(state.watch_count().unwrap(), MAX_WATCHES);

        unwatch_all(&state).unwrap();
        assert_eq!(state.watch_count().unwrap(), 0);
        assert!(state.begin_watch_slot("/cap-repo-after-clear").unwrap());
        unwatch_all(&state).unwrap();
    }

    #[test]
    fn test_start_watch_fails_closed_on_non_repo() {
        let dir = TempDir::new().unwrap();
        let state = WatcherState::default();
        assert!(
            start_watch_inner(&state, dir.path().to_string_lossy().into_owned(), |_| {}).is_err()
        );
        assert_eq!(state.watch_count().unwrap(), 0);
    }

    #[test]
    fn test_watch_missing_git_dir_fails() {
        let missing = Path::new("/definitely/missing-gitpulse-git-dir");
        assert!(!missing.exists());
        assert!(RepoFileWatcher::watch(missing).is_err());
    }

    /// Every path a repository needs must survive being registered as a set.
    ///
    /// The registration was rewritten from three sequential `watch()` calls to
    /// one `paths_mut()` batch committed once, because on FSEvents each
    /// `watch()` tore down and recreated the stream with a fresh
    /// `SinceNow` epoch — losing anything that landed in between. The risk of
    /// the rewrite is the opposite failure: a path silently dropped from the
    /// batch. This asserts all three still deliver.
    #[test]
    fn batched_registration_watches_every_path_it_was_given() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().canonicalize().expect("canonicalize");
        let git_dir = root.join(".git");
        let common = root.join("common.git");
        std::fs::create_dir_all(&git_dir).expect("git dir");
        std::fs::create_dir_all(&common).expect("common dir");

        let watcher = RepoFileWatcher::watch_repo(&git_dir, Some(&root), Some(&common))
            .expect("watch_repo should install every path");

        // Let the backend install before touching anything.
        thread::sleep(Duration::from_millis(400));

        std::fs::write(git_dir.join("HEAD"), b"ref: refs/heads/main\n").expect("write HEAD");
        std::fs::write(common.join("packed-refs"), b"# pack\n").expect("write packed-refs");
        std::fs::write(root.join("tracked.txt"), b"hello\n").expect("write worktree file");

        // Collect whatever arrives within a generous window and check that the
        // three roots are all represented. Bounded rather than blocking: a
        // dropped path shows up as a missing root, not as a hang.
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut saw_git_dir = false;
        let mut saw_common = false;
        let mut saw_worktree = false;
        while Instant::now() < deadline && !(saw_git_dir && saw_common && saw_worktree) {
            match watcher.receiver.recv_timeout(Duration::from_millis(250)) {
                Ok(Ok(event)) => {
                    for path in &event.paths {
                        if path.starts_with(&common) {
                            saw_common = true;
                        } else if path.starts_with(&git_dir) {
                            saw_git_dir = true;
                        } else if path.starts_with(&root) {
                            saw_worktree = true;
                        }
                    }
                }
                Ok(Err(_)) => {}
                Err(_) => {}
            }
        }

        assert!(saw_git_dir, "git dir events were not delivered");
        assert!(saw_common, "common dir events were not delivered");
        assert!(saw_worktree, "worktree root events were not delivered");
    }

    /// A missing worktree root must fail before anything is installed.
    ///
    /// The old order registered the git dir first and only then validated the
    /// root, so the error path left a live, half-registered watcher behind.
    #[test]
    fn a_missing_worktree_root_is_rejected_before_any_path_is_installed() {
        let temp = tempfile::tempdir().expect("tempdir");
        let git_dir = temp.path().join(".git");
        std::fs::create_dir_all(&git_dir).expect("git dir");

        let missing_root = temp.path().join("no-such-worktree");
        // `RepoFileWatcher` holds an OS watcher and is not Debug, so match
        // rather than `expect_err`.
        match RepoFileWatcher::watch_repo(&git_dir, Some(&missing_root), None) {
            Ok(_) => panic!("a missing work tree must be refused"),
            Err(err) => assert!(err.contains("work tree does not exist"), "{err}"),
        }
    }

    #[test]
    fn test_repo_changed_payload_is_path_object() {
        let payload = RepoChangedPayload {
            path: "/tmp/example-repo".into(),
        };
        let value = serde_json::to_value(&payload).expect("serialize");
        assert_eq!(value, serde_json::json!({ "path": "/tmp/example-repo" }));
    }

    #[test]
    fn test_start_watch_inner_fails_closed_at_cap_without_evicting() {
        let state = WatcherState::default();
        for i in 0..MAX_WATCHES {
            assert!(state.begin_watch_slot(&format!("/cap-live-{i}")).unwrap());
        }
        let dir = TempDir::new().unwrap();
        git_init(dir.path(), false);
        let err = start_watch_inner(&state, dir.path().to_string_lossy().into_owned(), |_| {})
            .expect_err("25th live watch must fail closed");
        assert!(
            err.contains("24") || err.to_lowercase().contains("too many"),
            "cap error should mention the limit, got: {err}"
        );
        assert_eq!(state.watch_count().unwrap(), MAX_WATCHES);
        let canonical = dir.path().canonicalize().unwrap();
        assert!(
            !state.is_watching(&canonical.to_string_lossy()).unwrap(),
            "failed watch must not occupy a slot under the canonical path"
        );
    }

    #[test]
    fn test_unwatch_raw_and_aliased_paths_match_canonical_key() {
        // A native backend hang must fail this regression promptly, rather
        // than consuming the CI job and hiding all later integration tests.
        let (phase, phases) = std::sync::mpsc::channel();
        let watchdog = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(60);
            let mut current = "create fixture";
            loop {
                match phases.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                    Ok(next) => {
                        current = next;
                        eprintln!("watch alias regression: {current}");
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        eprintln!("watch alias regression exceeded 60 seconds during: {current}");
                        // Panicking on this helper thread would leave the
                        // blocked test alive. Fail the binary; Cargo's
                        // --no-fail-fast still runs the other test binaries.
                        std::process::exit(124);
                    }
                }
            }
        });
        let dir = TempDir::new().unwrap();
        phase.send("git init").unwrap();
        git_init(dir.path(), false);
        let state = WatcherState::default();
        let raw = dir.path().to_string_lossy().into_owned();
        phase.send("initial watch").unwrap();
        let key = start_watch_inner(&state, raw.clone(), |_| {}).expect("watch");
        assert_eq!(state.watch_count().unwrap(), 1);

        let trailing = format!("{}/", raw.trim_end_matches('/'));
        if trailing != raw {
            phase.send("unwatch trailing slash").unwrap();
            unwatch(&state, trailing).unwrap();
            assert_eq!(
                state.watch_count().unwrap(),
                0,
                "trailing-slash alias must unwatch the canonical slot"
            );
            phase.send("rewatch after trailing slash").unwrap();
            start_watch_inner(&state, raw.clone(), |_| {}).expect("rewatch");
        }

        let dotted = dir.path().join(".").to_string_lossy().into_owned();
        phase.send("unwatch dotted path").unwrap();
        unwatch(&state, dotted).unwrap();
        assert_eq!(
            state.watch_count().unwrap(),
            0,
            "path/./ alias must unwatch the canonical slot"
        );

        phase.send("rewatch after dotted path").unwrap();
        let key_again = start_watch_inner(&state, raw.clone(), |_| {}).expect("rewatch");
        assert_eq!(key_again, key);
        phase.send("unwatch original path").unwrap();
        if raw != key {
            unwatch(&state, raw).unwrap();
            assert_eq!(
                state.watch_count().unwrap(),
                0,
                "pre-canonical path must unwatch after canonicalize (e.g. /var vs /private/var)"
            );
        } else {
            unwatch(&state, key).unwrap();
            assert_eq!(state.watch_count().unwrap(), 0);
        }
        phase.send("drop watcher state").unwrap();
        drop(state);
        phase.send("remove fixture").unwrap();
        dir.close()
            .expect("remove fixture after watches were stopped");
        drop(phase);
        watchdog.join().unwrap();
    }

    #[test]
    fn a_disconnected_backend_releases_its_session_for_a_real_restart() {
        let dir = TempDir::new().unwrap();
        git_init(dir.path(), false);
        let root = dir.path().canonicalize().unwrap();
        let git_dir = root.join(".git");
        let key = root.to_string_lossy().into_owned();
        let state = WatcherState::default();
        let stop = insert_watch_session(&mut state.lock_sessions().unwrap(), &key, &[], None)
            .unwrap()
            .unwrap();
        let mut watcher = RepoFileWatcher::watch(&git_dir).unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        drop(sender);
        watcher.receiver = receiver;
        let exit = run_watch_loop(
            &watcher,
            WatchLoopContext {
                git_dir: git_dir.clone(),
                internal_roots: vec![git_dir],
                emit_path: key.clone(),
            },
            stop.clone(),
            Some(state.sessions.clone()),
            stop,
            |_| panic!("a closed backend must not invent a change"),
        );
        assert_eq!(exit, WatchLoopExit::EventStreamClosed);
        assert_eq!(
            state.watch_count().unwrap(),
            0,
            "a dead backend must release its slot so rewatch actually starts a backend"
        );
        assert_eq!(start_watch_inner(&state, key.clone(), |_| {}).unwrap(), key);
        assert_eq!(state.watch_count().unwrap(), 1);
        unwatch_all(&state).unwrap();
    }

    #[test]
    fn a_disconnected_old_backend_cannot_reap_its_replacement() {
        let dir = TempDir::new().unwrap();
        git_init(dir.path(), false);
        let root = dir.path().canonicalize().unwrap();
        let git_dir = root.join(".git");
        let key = root.to_string_lossy().into_owned();
        let state = WatcherState::default();
        let stop = insert_watch_session(&mut state.lock_sessions().unwrap(), &key, &[], None)
            .unwrap()
            .unwrap();
        // Keep the old generation alive while its replacement owns the slot.
        let old_session = state.lock_sessions().unwrap().remove(&key).unwrap();
        let replacement =
            insert_watch_session(&mut state.lock_sessions().unwrap(), &key, &[], None)
                .unwrap()
                .unwrap();
        let mut watcher = RepoFileWatcher::watch(&git_dir).unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        drop(sender);
        watcher.receiver = receiver;
        let exit = run_watch_loop(
            &watcher,
            WatchLoopContext {
                git_dir: git_dir.clone(),
                internal_roots: vec![git_dir],
                emit_path: key.clone(),
            },
            stop.clone(),
            Some(state.sessions.clone()),
            stop,
            |_| panic!("a closed backend must not invent a change"),
        );
        assert_eq!(exit, WatchLoopExit::EventStreamClosed);
        assert_eq!(state.watch_count().unwrap(), 1);
        assert!(Arc::ptr_eq(
            &state.lock_sessions().unwrap().get(&key).unwrap().stop,
            &replacement
        ));
        assert!(!replacement.load(Ordering::Relaxed));
        drop(old_session);
        unwatch_all(&state).unwrap();
    }

    #[test]
    fn unwatch_waits_for_the_retired_callback_to_release_its_resources() {
        struct Released(Arc<AtomicBool>);
        impl Drop for Released {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let dir = TempDir::new().unwrap();
        git_init(dir.path(), false);
        let state = WatcherState::default();
        let released = Arc::new(AtomicBool::new(false));
        let resource = Released(released.clone());
        let key = start_watch_inner(
            &state,
            dir.path().to_string_lossy().into_owned(),
            move |_| {
                std::hint::black_box(&resource);
            },
        )
        .unwrap();
        unwatch(&state, key).unwrap();
        assert!(
            released.load(Ordering::SeqCst),
            "successful unwatch returned while the retired session still owned resources"
        );
    }

    #[test]
    fn retirement_signals_every_session_before_waiting_and_reports_lost_receipts() {
        let state = WatcherState::default();
        let (tx1, rx1) = std::sync::mpsc::channel();
        let (tx2, rx2) = std::sync::mpsc::channel();
        let one = insert_watch_session(&mut state.lock_sessions().unwrap(), "one", &[], Some(rx1))
            .unwrap()
            .unwrap();
        let two = insert_watch_session(&mut state.lock_sessions().unwrap(), "two", &[], Some(rx2))
            .unwrap()
            .unwrap();
        let state_for_worker = state.clone();
        let worker = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(2);
            while !(one.load(Ordering::SeqCst) && two.load(Ordering::SeqCst)) {
                assert!(
                    Instant::now() < deadline,
                    "every session must be signalled before waiting"
                );
                thread::yield_now();
            }
            assert_eq!(
                state_for_worker.watch_count().unwrap(),
                0,
                "retirement cannot hold the sessions mutex"
            );
            tx1.send(Ok(())).unwrap();
            tx2.send(Ok(())).unwrap();
        });
        unwatch_all(&state).unwrap();
        worker.join().unwrap();
        let (lost, receipt) = std::sync::mpsc::channel();
        insert_watch_session(
            &mut state.lock_sessions().unwrap(),
            "lost",
            &[],
            Some(receipt),
        )
        .unwrap();
        drop(lost);
        assert!(unwatch_all(&state)
            .unwrap_err()
            .contains("not acknowledged"));
    }

    #[test]
    fn concurrent_alias_watch_lifecycles_do_not_stall() {
        let deadline = Instant::now() + Duration::from_secs(120);
        thread::scope(|scope| {
            let mut lanes = Vec::new();
            for lane in 0..2 {
                lanes.push(scope.spawn(move || {
                    for cycle in 0..16 {
                        assert!(
                            Instant::now() < deadline,
                            "watch lifecycle stress exceeded its total budget"
                        );
                        eprintln!("watch alias stress lane {lane}, cycle {cycle}");
                        test_unwatch_raw_and_aliased_paths_match_canonical_key();
                    }
                }));
            }
            for lane in lanes {
                lane.join().unwrap();
            }
        });
        assert!(
            Instant::now() < deadline,
            "watch lifecycle stress exceeded its total budget"
        );
    }

    /// After the watched directory is deleted, `canonicalize()` and
    /// `validate_repo()` both fail, so the lookup can only succeed via the
    /// watch-time aliases or lexical variants. Losing this race leaked a
    /// MAX_WATCHES slot forever (macOS `/var` → `/private/var` makes the
    /// stored key differ lexically from every path the caller still holds).
    #[test]
    fn unwatch_after_directory_deletion_releases_the_slot() {
        let dir = TempDir::new().unwrap();
        git_init(dir.path(), false);
        let state = WatcherState::default();
        let raw = dir.path().to_string_lossy().into_owned();
        start_watch_inner(&state, raw.clone(), |_| {}).expect("watch");
        assert_eq!(state.watch_count().unwrap(), 1);

        drop(dir); // directory gone: canonicalization now fails

        unwatch(&state, raw).unwrap();
        assert_eq!(
            state.watch_count().unwrap(),
            0,
            "deleted-dir unwatch must not leak a watch slot"
        );
        assert!(state.begin_watch_slot("/post-deletion-slot").unwrap());
    }

    #[test]
    fn lexical_normalizations_strip_trailing_slash_and_dot_segments() {
        // `components()` rebuilds with the platform separator, so the
        // normalized variant is `\tmp\repo` on Windows. That is correct — it
        // is what the stored keys look like there — and the property under
        // test is the stripping of trailing slashes and `.` segments, so the
        // comparison is made separator-agnostic rather than pinned to Unix.
        fn keys(path: &str) -> Vec<String> {
            let mut values: Vec<String> = lexical_normalizations(path)
                .into_iter()
                .map(|v| v.replace('\\', "/"))
                .collect();
            // Windows produces a second, back-slashed spelling of an input
            // already written with forward slashes; once both are compared in
            // one spelling the duplicate is the same key.
            values.dedup();
            values
        }
        assert_eq!(
            keys("/tmp/repo/"),
            vec!["/tmp/repo/".to_string(), "/tmp/repo".to_string()]
        );
        assert_eq!(keys("."), vec![".".to_string()]);
        assert_eq!(keys("relative/repo"), vec!["relative/repo".to_string()]);
    }

    /// An event naming the git directory itself is not a repository change.
    ///
    /// It says only "something under here moved", which the recursive watch on
    /// that same directory already reports in detail. Windows delivered
    /// exactly this event for every git-internal write, so without the rule
    /// pure lockfile churn read as repository change there.
    #[test]
    fn an_event_for_the_git_directory_itself_is_noise() {
        let tmp = TempDir::new().unwrap();
        let git_dir = tmp.path().join(".git");
        std::fs::create_dir_all(git_dir.join("refs")).unwrap();
        let roots = internal_roots_for(tmp.path());

        assert!(
            is_git_internal_noise(&git_dir, &roots),
            "a bare git-dir event carries no change information"
        );
        assert!(
            !is_git_internal_noise(&git_dir.join("refs").join("heads"), &roots),
            "but a ref write inside it still matters"
        );
    }

    /// A relative path names whatever the *caller's* cwd says it names, and the
    /// watch registry is keyed by canonical repository paths, so resolving one
    /// here would unwatch a repository nobody named.
    ///
    /// This used to prove it by `set_current_dir`-ing into the watched
    /// repository and unwatching `"."`. Cwd is process-wide, so that needed a
    /// whole second copy of this test binary to run one test in — with a 15s
    /// deadline on a child that has to link, load and start libtest, which is
    /// the most load-sensitive wait in the suite and the first to fail on a busy
    /// machine. Handing `unwatch` a relative path that resolves to the watched
    /// repository *from the cwd the test already has* proves the same thing:
    /// an implementation that canonicalized would find the watch and remove it.
    #[test]
    fn test_unwatch_relative_path_does_not_canonicalize_against_cwd() {
        // The system temp directory and the runner's cwd are different volumes
        // on GitHub's Windows image (`C:\Users\...\Temp` against `D:\a\...`).
        // No relative path crosses a volume, so the fixture has to live on the
        // cwd's volume or `unwatch` is handed an absolute path and the
        // assertion cannot tell a bad fixture from a lookup that canonicalized.
        let cwd_for_fixture = std::env::current_dir().expect("cwd");
        let dir = tempfile::tempdir_in(&cwd_for_fixture).expect("tempdir on the cwd volume");
        git_init(dir.path(), false);
        let state = WatcherState::default();
        let key = start_watch_inner(&state, dir.path().to_string_lossy().into_owned(), |_| {})
            .expect("watch");

        let repo = dir.path().canonicalize().expect("canonical repo");
        let cwd = cwd_for_fixture.canonicalize().expect("canonical cwd");
        let relative = relative_to(&cwd, &repo);
        // Without this the test could pass on a path that resolves nowhere,
        // which is a fixture that cannot fail rather than a behaviour that
        // cannot break.
        assert!(
            relative.is_relative(),
            "the fixture must hand `unwatch` a relative path: {}",
            relative.display()
        );
        assert_eq!(
            cwd.join(&relative).canonicalize().expect("resolvable"),
            repo,
            "the relative path must really resolve to the watched repository"
        );

        unwatch(&state, relative.to_string_lossy().into_owned()).unwrap();
        assert!(
            state.is_watching(&key).unwrap(),
            "unwatch must look a relative path up raw, not resolve it against cwd"
        );
        unwatch(&state, key.clone()).unwrap();
        assert!(!state.is_watching(&key).unwrap());
    }

    #[test]
    fn test_watch_bare_repo_and_gitfile_worktree() {
        let bare = TempDir::new().unwrap();
        git_init(bare.path(), true);
        let state = WatcherState::default();
        let bare_key =
            start_watch_inner(&state, bare.path().to_string_lossy().into_owned(), |_| {})
                .expect("watch bare");
        assert!(state.is_watching(&bare_key).unwrap());

        let (_main, _work_parent, work_path) = init_linked_worktree();
        let wt_key = start_watch_inner(&state, work_path.to_string_lossy().into_owned(), |_| {})
            .expect("watch gitfile worktree");
        assert_ne!(bare_key, wt_key);
        assert_eq!(state.watch_count().unwrap(), 2);

        unwatch(&state, work_path.to_string_lossy().into_owned()).unwrap();
        assert_eq!(state.watch_count().unwrap(), 1);
        assert!(state.is_watching(&bare_key).unwrap());
        unwatch(&state, bare_key).unwrap();
        assert_eq!(state.watch_count().unwrap(), 0);
    }

    #[test]
    fn test_rewatch_raw_then_canonical_is_idempotent() {
        let dir = TempDir::new().unwrap();
        git_init(dir.path(), false);
        let state = WatcherState::default();
        let raw = dir.path().to_string_lossy().into_owned();
        let first = start_watch_inner(&state, raw, |_| {}).expect("watch raw");
        let second = start_watch_inner(&state, first.clone(), |_| {}).expect("watch canonical");
        assert_eq!(first, second);
        assert_eq!(state.watch_count().unwrap(), 1);
        unwatch_all(&state).unwrap();
    }

    #[test]
    fn test_watch_lookup_keys_relative_is_raw_only() {
        let keys = watch_lookup_keys(".");
        assert_eq!(keys, vec![".".to_string()]);
        let keys = watch_lookup_keys("relative/repo");
        assert_eq!(keys, vec!["relative/repo".to_string()]);
    }

    /// Spawns the real debounce loop over a real watcher and returns a
    /// receiver of `repo-changed` paths plus a stop handle.
    fn spawn_loop(dir: &Path) -> (std::sync::mpsc::Receiver<String>, Arc<AtomicBool>, PathBuf) {
        use std::process::Command;

        let output = Command::new("git")
            .arg("init")
            .current_dir(dir)
            .output_locked()
            .expect("spawn git init");
        assert!(output.status.success());

        let canonical = dir.canonicalize().unwrap();
        let git_dir = resolve_git_dir(&canonical).expect("git dir");
        let watcher =
            RepoFileWatcher::watch_repo(&git_dir, Some(&canonical), None).expect("watcher");

        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let session_stop = stop.clone();
        let emit_path = canonical.to_string_lossy().into_owned();
        let loop_git_dir = canonical.clone();
        let internal_roots = vec![canonical.join(".git")];
        let (tx, rx) = std::sync::mpsc::channel();
        thread::Builder::new()
            .name("watch-loop-test".into())
            .spawn(move || {
                run_watch_loop(
                    &watcher,
                    WatchLoopContext {
                        git_dir: loop_git_dir,
                        internal_roots,
                        emit_path,
                    },
                    thread_stop,
                    None,
                    session_stop,
                    move |p| {
                        let _ = tx.send(p);
                    },
                );
            })
            .expect("spawn loop");
        // Give the OS backend time to install its watches before the storm.
        thread::sleep(Duration::from_millis(300));
        (rx, stop, canonical)
    }

    /// Overall ceiling for priming the watcher pipeline in tests: generous
    /// enough for a machine shared with cargo builds and other suites, yet
    /// bounded so a genuinely broken backend fails fast instead of hanging.
    ///
    /// Matched to the ceiling `watch_loop_emits_under_continuous_churn_within_max_wait`
    /// already argued for: FSEvents delivery plus the 400 ms debounce stretches
    /// far past its idle latency when the whole workspace suite runs at once.
    /// Raising it costs nothing on a passing run — every user is a
    /// `while now < deadline` loop that breaks on success — and only lengthens
    /// how long a genuinely broken backend takes to be declared broken.
    ///
    /// `panicking_on_change_cannot_leak_the_watch_slot` failed exactly once
    /// here under `cargo test --workspace`, at a hardcoded 8 s that bypassed
    /// this constant, and passed 3/3 in isolation. Two tests had their own
    /// budget; both now use this one, so the next machine that is slower still
    /// gets one place to change.
    const PRIME_DEADLINE: Duration = Duration::from_secs(20);

    /// Consumes callbacks until a silent stretch longer than [`DEBOUNCE_MAX_WAIT`]
    /// passes. Every delivered event is guaranteed to produce an emission
    /// within [`DEBOUNCE_MAX_WAIT`] (`should_emit` anti-starvation bound), so
    /// silence of that length proves no event is still in flight — a plain
    /// fixed sleep is not enough, because under parallel-suite load FSEvents
    /// can trail its writes by hundreds of ms and a leaked emission would
    /// pollute whatever the caller measures next.
    fn await_watcher_quiescence<T>(rx: &std::sync::mpsc::Receiver<T>) {
        loop {
            match rx.recv_timeout(DEBOUNCE_MAX_WAIT + Duration::from_millis(100)) {
                Ok(_) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => break,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
    }

    /// Writes uniquely-named probe files into `probe_dir` until the debounce
    /// pipeline delivers one settled callback, then quiesces via
    /// [`await_watcher_quiescence`] so the caller starts from an empty,
    /// genuinely idle channel.
    ///
    /// Why a retry loop instead of one write plus one long recv_timeout:
    /// FSEvents gives no delivery guarantee for writes racing watch-stream
    /// installation (each `watch()` call recreates the stream with a fresh
    /// SinceNow epoch), so a single priming write can be silently lost and
    /// flake the test. Retrying fresh probe names until delivery — the cure
    /// already proven in `dead_repo_watch_stops_emitting_and_reaps_session` —
    /// makes priming deterministic under load. Panics with the attempt count
    /// only if nothing is ever delivered within `deadline`.
    fn prime_watcher<T>(rx: &std::sync::mpsc::Receiver<T>, probe_dir: &Path, deadline: Duration) {
        let overall = Instant::now() + deadline;
        let mut n = 0u32;
        loop {
            std::fs::write(probe_dir.join(format!("watcher-prime-{n}.txt")), "x")
                .expect("write priming probe");
            n += 1;
            if rx
                .recv_timeout(DEBOUNCE_QUIET + Duration::from_millis(200))
                .is_ok()
            {
                break;
            }
            assert!(
                Instant::now() < overall,
                "watcher never delivered a priming callback after {n} probe writes within \
                 {deadline:?}; the OS watch stream likely never installed"
            );
        }
        await_watcher_quiescence(rx);
    }

    /// Debounce proof: 30 rapid top-level writes must settle into exactly one
    /// callback after the quiet window, not thirty. A second callback during
    /// a subsequent quiet window would mean events are being forwarded
    /// per-write instead of debounced. Priming goes through `prime_watcher`
    /// because a single fixed write races watch installation and can be
    /// silently dropped by FSEvents, which made this test fail intermittently.
    #[test]
    fn watch_loop_coalesces_a_write_storm_into_one_settled_callback() {
        let temp = TempDir::new().unwrap();
        let (rx, stop, root) = spawn_loop(temp.path());

        // Prime: prove the pipeline delivers before measuring coalescing,
        // starting the storm from a drained, quiescent channel.
        prime_watcher(&rx, &root, PRIME_DEADLINE);

        // Burst: 30 writes inside roughly one debounce tick must settle into
        // exactly one callback followed by quiet. Each attempt uses fresh
        // file names. If the backend silently drops the whole batch, or a
        // stray late delivery splits the burst (both observed under parallel
        // suite load even on a proven-hot stream), the attempt is retried
        // until the deadline; genuine per-write forwarding instead of
        // debouncing would fail EVERY attempt and still panic below.
        let burst_deadline = Instant::now() + PRIME_DEADLINE;
        let mut attempt = 0u32;
        loop {
            assert!(
                Instant::now() < burst_deadline,
                "no cleanly coalesced 30-write burst within {PRIME_DEADLINE:?} \
                 ({attempt} attempts)"
            );
            for i in 0..30 {
                std::fs::write(root.join(format!("storm_{attempt}_{i}.txt")), "x").unwrap();
            }
            attempt += 1;

            // Exactly one settled callback for the whole burst.
            let Ok(first) = rx.recv_timeout(Duration::from_secs(10)) else {
                continue; // whole batch dropped by the backend: retry
            };
            assert!(!first.is_empty());

            // Quiet window well past the 400ms settle: no further callbacks
            // may arrive for this burst, because no further events exist to
            // debounce.
            match rx.recv_timeout(Duration::from_millis(900)) {
                Ok(_) => {
                    // Burst was split by a stray late delivery: quiesce and
                    // retry with fresh names.
                    await_watcher_quiescence(&rx);
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => break,
                Err(e) => panic!("channel failed during quiet window: {e}"),
            }
        }
        stop.store(true, Ordering::SeqCst);
    }

    /// The reason for the second watch: an unstaged top-level edit fires the
    /// callback even though nothing under `.git` changed — and `.git`-only
    /// writes still fire as before.
    #[test]
    fn worktree_file_event_triggers_and_git_only_events_still_do() {
        let temp = TempDir::new().unwrap();
        let (rx, stop, root) = spawn_loop(temp.path());

        // Prime with bounded retries before asserting: one fixed write racing
        // watch installation can be silently dropped by FSEvents (see
        // prime_watcher).
        prime_watcher(&rx, &root, PRIME_DEADLINE);

        // 1. Worktree-only write: no index/HEAD traffic involved. Kept
        //    retry-bounded too: under parallel suite load even a write to the
        //    freshly-proven-hot stream was once lost for >15s.
        let edit_deadline = Instant::now() + PRIME_DEADLINE;
        let mut n = 0u32;
        loop {
            std::fs::write(root.join(format!("unstaged-edit-{n}.txt")), "dirty\n").unwrap();
            n += 1;
            if rx
                .recv_timeout(DEBOUNCE_QUIET + Duration::from_millis(200))
                .is_ok()
            {
                break;
            }
            assert!(
                Instant::now() < edit_deadline,
                "unstaged worktree edit must trigger repo-changed ({n} writes within \
                 {PRIME_DEADLINE:?})"
            );
        }

        // Close the quiet window so the next write is a distinct emission
        // rather than coalesced with a late event from the first write.
        thread::sleep(DEBOUNCE_QUIET + Duration::from_millis(100));

        // 2. A .git-only write still triggers through the recursive watch.
        //    Retry-bounded for the same reason as above; each attempt uses a
        //    fresh name inside `.git` so no state carries between attempts.
        let git_deadline = Instant::now() + PRIME_DEADLINE;
        let mut n = 0u32;
        loop {
            std::fs::write(root.join(".git").join(format!("gitpulse-probe-{n}")), "x").unwrap();
            n += 1;
            if rx
                .recv_timeout(DEBOUNCE_QUIET + Duration::from_millis(200))
                .is_ok()
            {
                break;
            }
            assert!(
                Instant::now() < git_deadline,
                "git-dir write must still trigger repo-changed ({n} writes within \
                 {PRIME_DEADLINE:?})"
            );
        }

        stop.store(true, Ordering::SeqCst);
    }

    /// Decision table for the emission rule: quiet period alone emits, but a
    /// pending refresh must also fire once the first event has waited out
    /// DEBOUNCE_MAX_WAIT — even while events keep arriving.
    #[test]
    fn should_emit_quiet_period_or_max_wait() {
        let now = Instant::now();
        // Nothing pending: never emit.
        assert!(!should_emit(
            false,
            now - DEBOUNCE_QUIET * 3,
            Some(now),
            now
        ));
        // Freshly active and young: hold.
        let fresh = now - Duration::from_millis(100);
        assert!(!should_emit(true, fresh, Some(fresh), now));
        // Quiet past 400ms: emit.
        assert!(should_emit(
            true,
            now - Duration::from_millis(450),
            Some(now - Duration::from_millis(450)),
            now
        ));
        // Still churning (last_event fresh) but first event older than the
        // max wait: emit anyway — this is the anti-starvation bound.
        assert!(should_emit(
            true,
            fresh,
            Some(now - Duration::from_millis(2100)),
            now
        ));
        // Exactly at the boundary counts as due.
        assert!(should_emit(
            true,
            now - Duration::from_millis(2000),
            Some(now - Duration::from_millis(2000)),
            now
        ));
    }

    /// Build-directory paths must not schedule a scan.
    ///
    /// A non-recursive worktree watch does not deliver nested `target/` writes
    /// (see [`measure_target_directory_event_rate`]). Paths still have to be
    /// classified: FSEvents can report the `target` directory itself, a
    /// `target-*` sibling, or a nested path that leaked through. Before this
    /// filter every one of those was signal, and each signal opens a refresh
    /// that fans out into many `git` children.
    #[test]
    fn build_output_storm_does_not_schedule_a_scan() {
        let root = Path::new("/repo");
        let rules = IgnoreRules::builtin();
        let started = Instant::now();
        let mut scans = 0u32;
        for i in 0..4_000 {
            let paths = [
                root.join("target")
                    .join("debug")
                    .join(format!("lib{i}.rlib")),
                root.join(format!("target{i}")).join("debug").join("x.o"),
                root.join("target-release")
                    .join("deps")
                    .join(format!("{i}.o")),
                root.join("pkg").join("target").join("debug").join("x.o"),
                root.join("node_modules").join("left-pad").join("index.js"),
            ];
            // One representative path per iteration keeps the rate comparable
            // to "events that would each open or extend the debounce window".
            let path = paths[i as usize % paths.len()].clone();
            let event = notify::Event::new(notify::EventKind::Any).add_path(path);
            if event_has_signal(&event, &[], root, Some(root), &rules) {
                scans += 1;
            }
        }
        let elapsed = started.elapsed();
        let per_sec = scans as f64 / elapsed.as_secs_f64().max(1e-9);
        eprintln!(
            "build-storm scheduled_scans={scans} of 4000 in {elapsed:?} ({per_sec:.1}/s of classifier time)"
        );
        assert_eq!(
            scans, 0,
            "build-directory writes scheduled {scans} refreshes; each refresh spawns git"
        );
        let source =
            notify::Event::new(notify::EventKind::Any).add_path(root.join("src").join("main.rs"));
        assert!(
            event_has_signal(&source, &[], root, Some(root), &rules),
            "a source edit must still schedule a scan"
        );
        let source_file_named_like_a_dir =
            notify::Event::new(notify::EventKind::Any).add_path(root.join("target.rs"));
        assert!(
            event_has_signal(&source_file_named_like_a_dir, &[], root, Some(root), &rules),
            "target.rs is source, not a build directory"
        );
        let targeting = notify::Event::new(notify::EventKind::Any)
            .add_path(root.join("targeting").join("lib.rs"));
        assert!(
            event_has_signal(&targeting, &[], root, Some(root), &rules),
            "a directory named targeting is not a Cargo target/ tree"
        );
    }

    /// Names that look like build output but are ordinary source trees must
    /// still refresh. The denylist is only for directories that are build
    /// output by construction (`target*`, `node_modules`, toolchain caches).
    #[test]
    fn source_trees_named_like_build_output_still_scan() {
        let root = Path::new("/repo");
        let rules = IgnoreRules::builtin();
        for relative in [
            "src/build/lib.rs",
            "docs/out/notes.md",
            "coverage/report.md",
            "dist/readme.md",
        ] {
            let event = notify::Event::new(notify::EventKind::Any).add_path(root.join(relative));
            assert!(
                event_has_signal(&event, &[], root, Some(root), &rules),
                "{relative} is source and must schedule a scan"
            );
        }
    }

    /// An event that names the worktree directory itself has no file identity.
    /// FSEvents delivers that path for a coalesced storm. Treating it as signal
    /// schedules a full git refresh for every burst that collapsed to the root.
    #[test]
    fn worktree_root_event_does_not_schedule_a_scan() {
        let root = Path::new("/repo");
        let rules = IgnoreRules::builtin();
        let event = notify::Event::new(notify::EventKind::Any).add_path(root.to_path_buf());
        assert!(
            !event_has_signal(&event, &[], root, Some(root), &rules),
            "the worktree root carries no path to refresh"
        );
        let child = notify::Event::new(notify::EventKind::Any).add_path(root.join("README.md"));
        assert!(event_has_signal(&child, &[], root, Some(root), &rules));
    }

    #[test]
    fn ignore_prefix_matches_the_directory_and_not_a_longer_name() {
        let mut rules = IgnoreRules::builtin();
        rules.prefixes.insert("scratch".into());
        rules.prefixes.insert("foo/bar".into());
        let root = Path::new("/repo");
        let hidden =
            notify::Event::new(notify::EventKind::Any).add_path(root.join("scratch").join("a.txt"));
        assert!(!event_has_signal(&hidden, &[], root, Some(root), &rules));
        let neighbor = notify::Event::new(notify::EventKind::Any)
            .add_path(root.join("scratchpad").join("a.txt"));
        assert!(
            event_has_signal(&neighbor, &[], root, Some(root), &rules),
            "scratch must not swallow scratchpad"
        );
        let nested = notify::Event::new(notify::EventKind::Any)
            .add_path(root.join("foo").join("bar").join("x"));
        assert!(!event_has_signal(&nested, &[], root, Some(root), &rules));
        let lookalike = notify::Event::new(notify::EventKind::Any)
            .add_path(root.join("foo").join("barbaz").join("x"));
        assert!(event_has_signal(&lookalike, &[], root, Some(root), &rules));
    }

    #[test]
    fn ignore_prefixes_fold_case_and_a_failed_query_does_not_hide_source() {
        let (prefixes, truncated) = parse_ignored_directories("Scratch/\0keep.txt\0./Pkg/Gen/\0");
        assert!(!truncated);
        assert!(prefixes.contains("scratch"));
        assert!(prefixes.contains("pkg/gen"));
        assert!(!prefixes.contains("keep.txt"));
        let mut rules = IgnoreRules::builtin();
        rules.prefixes = prefixes;
        let root = Path::new("/repo");
        let folded =
            notify::Event::new(notify::EventKind::Any).add_path(root.join("SCRATCH").join("a.txt"));
        assert!(!event_has_signal(&folded, &[], root, Some(root), &rules));

        let mut failed = IgnoreRules::builtin();
        failed.load_error = Some("git ls-files timed out".into());
        let source =
            notify::Event::new(notify::EventKind::Any).add_path(root.join("src").join("main.rs"));
        assert!(
            event_has_signal(&source, &[], root, Some(root), &failed),
            "a failed ignore query must not classify the tree as ignored"
        );
        let target = notify::Event::new(notify::EventKind::Any)
            .add_path(root.join("TARGET").join("debug").join("x.o"));
        assert!(
            !event_has_signal(&target, &[], root, Some(root), &failed),
            "built-in build directories still apply when the query failed"
        );
    }

    #[test]
    fn ignore_directory_list_records_truncation_instead_of_a_complete_scan() {
        let mut raw = String::new();
        for i in 0..(MAX_IGNORE_PREFIXES + 3) {
            raw.push_str(&format!("dir{i}/\0"));
        }
        let (prefixes, truncated) = parse_ignored_directories(&raw);
        assert!(truncated, "a capped list must not look complete");
        assert_eq!(prefixes.len(), MAX_IGNORE_PREFIXES);
        assert!(!prefixes.contains(&format!("dir{}", MAX_IGNORE_PREFIXES)));
    }

    #[test]
    fn classifier_holds_under_adversarial_paths() {
        let root = Path::new("/repo");
        let mut rules = IgnoreRules::builtin();
        rules.prefixes.insert("scratch".into());
        let started = Instant::now();
        for i in 0..20_000u32 {
            let target = notify::Event::new(notify::EventKind::Any).add_path(
                root.join(format!("target{}", i % 50))
                    .join("debug")
                    .join(format!("{i}.o")),
            );
            assert!(!event_has_signal(&target, &[], root, Some(root), &rules));
            let source = notify::Event::new(notify::EventKind::Any)
                .add_path(root.join("src").join(format!("file{i}.rs")));
            assert!(event_has_signal(&source, &[], root, Some(root), &rules));
        }
        let escaped = notify::Event::new(notify::EventKind::Any).add_path(
            root.join("..")
                .join("elsewhere")
                .join("src")
                .join("main.rs"),
        );
        assert!(
            event_has_signal(&escaped, &[], root, Some(root), &rules),
            "a lexical escape stays signal; failing to classify it is not noise"
        );
        let long_name = "n".repeat(8_000);
        let long =
            notify::Event::new(notify::EventKind::Any).add_path(root.join("src").join(long_name));
        assert!(event_has_signal(&long, &[], root, Some(root), &rules));
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "40k classifications took {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn failed_ignore_query_is_not_retried_on_every_tick() {
        let mut rules = IgnoreRules::builtin();
        rules.load_error = Some("timed out".into());
        let now = Instant::now();
        assert!(!rules_due(&rules, now, now, false));
        assert!(!rules_due(&rules, now, now + Duration::from_secs(5), false));
        assert!(rules_due(&rules, now, now + IGNORE_RETRY, false));
        let clean = IgnoreRules::builtin();
        assert!(!rules_due(&clean, now, now + IGNORE_RETRY, false));
        assert!(rules_due(&clean, now, now + SCAN_COALESCE, true));
        assert!(!rules_due(
            &clean,
            now,
            now + Duration::from_millis(200),
            true
        ));
    }

    /// Raw FSEvents rate for a `target/` write storm, before the debounce
    /// callback. Prints writes, delivered events, and how many of those name
    /// `target`, over a fixed 2 second window.
    #[test]
    fn measure_target_directory_event_rate() {
        let dir = TempDir::new().unwrap();
        git_init(dir.path(), false);
        let git_dir = dir.path().join(".git");
        let target = dir.path().join("target");
        std::fs::create_dir_all(&target).unwrap();
        let watcher = RepoFileWatcher::watch_repo(&git_dir, Some(dir.path()), None).expect("watch");
        std::thread::sleep(Duration::from_millis(400));
        while watcher.receiver.try_recv().is_ok() {}

        let writes = 400u32;
        let started = Instant::now();
        for n in 0..writes {
            std::fs::write(target.join(format!("obj{n}.o")), "x").unwrap();
        }
        let window = started.elapsed();
        std::fs::write(dir.path().join("sentinel-src.rs"), "fn x() {}").unwrap();
        std::thread::sleep(Duration::from_millis(800));
        let mut events = 0u32;
        let mut target_events = 0u32;
        let mut sentinel_events = 0u32;
        let mut sample = String::new();
        while let Ok(item) = watcher.receiver.try_recv() {
            events += 1;
            if let Ok(event) = item {
                let names_target = event.paths.iter().any(|path| {
                    path.components()
                        .any(|component| component.as_os_str() == "target")
                });
                if names_target {
                    target_events += 1;
                }
                if event.paths.iter().any(|path| {
                    path.file_name()
                        .is_some_and(|name| name == "sentinel-src.rs")
                }) {
                    sentinel_events += 1;
                }
                if sample.len() < 400 {
                    for path in &event.paths {
                        sample.push_str(&path.display().to_string());
                        sample.push('\n');
                    }
                }
            }
        }
        let per_sec = target_events as f64 / window.as_secs_f64();
        eprintln!(
            "target-storm writes={writes} raw_events={events} target_events={target_events} ({per_sec:.1}/s) sentinel_events={sentinel_events} window={window:?}\n{sample}"
        );
        assert_eq!(
            target_events, 0,
            "nested target/ writes must not reach the non-recursive worktree watch"
        );
        assert!(
            sentinel_events >= 1,
            "the watch must still deliver a file at the worktree root, else the zero above is a dead stream"
        );
    }

    #[test]
    fn gitignored_directory_does_not_schedule_a_scan() {
        let dir = TempDir::new().unwrap();
        git_init(dir.path(), false);
        std::fs::write(dir.path().join(".gitignore"), "scratch/\n").unwrap();
        std::fs::create_dir_all(dir.path().join("scratch")).unwrap();
        std::fs::write(dir.path().join("scratch").join("a.txt"), "x").unwrap();
        let rules = IgnoreRules::load(dir.path());
        assert!(
            rules.load_error.is_none(),
            "ignore query failed: {:?}",
            rules.load_error
        );
        assert!(
            rules.prefixes.iter().any(|prefix| prefix == "scratch"),
            "prefixes: {:?}",
            rules.prefixes
        );
        let ignored = notify::Event::new(notify::EventKind::Any)
            .add_path(dir.path().join("scratch").join("a.txt"));
        assert!(
            !event_has_signal(&ignored, &[], dir.path(), None, &rules),
            "a gitignored directory must not schedule a scan"
        );
        let source = notify::Event::new(notify::EventKind::Any)
            .add_path(dir.path().join("src").join("main.rs"));
        assert!(event_has_signal(&source, &[], dir.path(), None, &rules));
    }

    #[test]
    fn scans_coalesce_to_one_per_max_wait() {
        let now = Instant::now();
        let first = now - Duration::from_millis(500);
        assert!(
            scan_is_due(true, first, Some(first), None, now),
            "the first quiet period still scans"
        );
        assert!(
            !scan_is_due(
                true,
                first,
                Some(first),
                Some(now - Duration::from_millis(500)),
                now
            ),
            "a second scan inside the coalesce window must wait"
        );
        assert!(scan_is_due(
            true,
            now - SCAN_COALESCE,
            Some(now - SCAN_COALESCE),
            Some(now - SCAN_COALESCE),
            now
        ));
    }

    /// Under continuous churn the old loop postponed emission forever (the
    /// 400ms quiet window never opened). The max-wait bound must produce at
    /// least one refresh within roughly DEBOUNCE_MAX_WAIT even though writes
    /// never stop.
    #[test]
    fn watch_loop_emits_under_continuous_churn_within_max_wait() {
        let temp = TempDir::new().unwrap();
        let (rx, stop, root) = spawn_loop(temp.path());
        let churn_stop = Arc::new(AtomicBool::new(false));
        let writer_stop = churn_stop.clone();
        let churn_root = root.clone();
        let writer = thread::Builder::new()
            .name("watch-churn".into())
            .spawn(move || {
                let mut i = 0u32;
                while !writer_stop.load(Ordering::Relaxed) {
                    let _ = std::fs::write(churn_root.join(format!("churn_{i}.txt")), "x");
                    i += 1;
                    thread::sleep(Duration::from_millis(250));
                }
            })
            .expect("spawn churn writer");

        // Generous ceiling: this suite shares the machine with cargo builds,
        // and FSEvents delivery plus the 400ms debounce can stretch far past
        // their idle latencies under load. The assertion guards against a
        // lost callback, not against millisecond-level latency.
        let got = rx.recv_timeout(Duration::from_secs(20));
        churn_stop.store(true, Ordering::SeqCst);
        stop.store(true, Ordering::SeqCst);
        let _ = writer.join();
        got.expect("continuous churn must still yield a refresh within the max wait");
    }

    /// Regression (supervisor): a panicking `on_change` used to kill the
    /// watch thread with no trace and leak the MAX_WATCHES slot forever.
    /// The spawn-site catch_unwind must contain the panic, reap the session,
    /// and leave the state consistent for a re-watch of the same path.
    #[test]
    fn panicking_on_change_cannot_leak_the_watch_slot() {
        let dir = TempDir::new().unwrap();
        git_init(dir.path(), false);
        let state = WatcherState::default();
        let _key = start_watch_inner(&state, dir.path().to_string_lossy().into_owned(), |_| {
            panic!("emit explosion probe")
        })
        .expect("watch live repo");
        assert_eq!(state.watch_count().unwrap(), 1);

        // Poke the worktree until the debounce pipeline delivers to the
        // panicking callback (single writes race stream installation; see
        // prime_watcher for why retries are required).
        let deadline = Instant::now() + PRIME_DEADLINE;
        let mut n = 0u32;
        loop {
            std::fs::write(dir.path().join(format!("panic-probe-{n}.txt")), "x").unwrap();
            n += 1;
            if state.watch_count().unwrap() == 0 {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "panicking on_change must lead to session reaping \
                 ({n} probes within {PRIME_DEADLINE:?})"
            );
            thread::sleep(Duration::from_millis(200));
        }

        // The slot is free again: a re-watch of the same path succeeds.
        let key2 = start_watch_inner(&state, dir.path().to_string_lossy().into_owned(), |_| {})
            .expect("re-watch after panic");
        assert_eq!(state.watch_count().unwrap(), 1);
        unwatch_all(&state).unwrap();
        drop(key2);
    }

    /// Regression (audit D1): once the watched git directory is gone, notify
    /// keeps delivering remove/error events and the old loop re-emitted
    /// `repo-changed` forever while the session stayed resident. The loop
    /// must fall silent and reap its session.
    /// Unix only, for the technique rather than the behaviour: the test has to
    /// make a watched directory vanish, and Windows refuses to rename or
    /// delete one while a change-notification handle is open on it — the
    /// watcher holds handles on both the git dir (recursive) and the worktree
    /// root. The reaping path itself is platform-independent; there is simply
    /// no way to trigger it from outside on Windows.
    #[cfg(unix)]
    #[test]
    fn dead_repo_watch_stops_emitting_and_reaps_session() {
        let dir = TempDir::new().unwrap();
        git_init(dir.path(), false);
        let state = WatcherState::default();
        let (tx, rx) = std::sync::mpsc::channel();
        let key = start_watch_inner(
            &state,
            dir.path().to_string_lossy().into_owned(),
            move |_| {
                let _ = tx.send(());
            },
        )
        .expect("watch live repo");
        assert!(state.is_watching(&key).unwrap());

        // Keep poking the worktree root until the OS backend delivers: a
        // single write races watch installation, especially when other tests
        // in this process also hold FSEvents/inotify watches.
        let git_dir = dir.path().join(".git");
        let prime_deadline = Instant::now() + PRIME_DEADLINE;
        let mut primed = false;
        let mut n = 0u32;
        while Instant::now() < prime_deadline {
            std::fs::write(dir.path().join(format!("probe-{n}.txt")), "x").unwrap();
            n += 1;
            if rx
                .recv_timeout(DEBOUNCE_QUIET + Duration::from_millis(200))
                .is_ok()
            {
                primed = true;
                break;
            }
            assert!(
                git_dir.exists(),
                "temp repo vanished before the watcher primed"
            );
        }
        assert!(primed, "priming write must be delivered");
        while rx.try_recv().is_ok() {}

        // Rename rather than unlink: macOS FSEvents can block `remove_dir_all`
        // on a directory that still has an active watch, which deadlocks this
        // test against the loop that is waiting to observe the path's death.
        // Renaming makes the stored git_dir path vanish (`exists() == false`)
        // immediately while leaving the watch free to reap.
        let gone = dir.path().with_extension("gone");
        std::fs::rename(dir.path(), &gone).expect("rename repo out from under the watcher");

        let deadline = std::time::Instant::now() + Duration::from_secs(6);
        while std::time::Instant::now() < deadline {
            match rx.recv_timeout(Duration::from_millis(400)) {
                Ok(_) => panic!("emitted a change for a deleted repository"),
                // Reaping drops the sender; exiting early on disconnect is
                // fine, but silence alone also satisfies the test.
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
        // Silence is asserted by construction: the only Ok arm above panics,
        // so reaching here means no `repo-changed` fired for the deleted
        // repository within the window. Channel disconnection is an
        // implementation detail of how the session is reaped and is NOT
        // required; the reap itself is what the next assertion checks.
        assert_eq!(
            state.watch_count().unwrap(),
            0,
            "session for a dead repo must be reaped"
        );
        let _ = std::fs::remove_dir_all(&gone);
    }

    /// Regression (audit D2): a linked worktree only watched its private
    /// `.git/worktrees/<name>` subtree, so branch/ref writes in the COMMON
    /// dir never fired `repo-changed` for that worktree.
    #[test]
    fn linked_worktree_reacts_to_common_dir_ref_writes() {
        let (_main, _work_parent, work_path) = init_linked_worktree();
        let state = WatcherState::default();
        let (tx, rx) = std::sync::mpsc::channel();
        start_watch_inner(
            &state,
            work_path.to_string_lossy().into_owned(),
            move |_| {
                let _ = tx.send(());
            },
        )
        .expect("watch linked worktree");
        thread::sleep(Duration::from_millis(300));

        // The main repo's real git dir hosts refs/heads for all worktrees.
        let gitfile = std::fs::read_to_string(work_path.join(".git")).unwrap();
        let work_git = gitfile
            .lines()
            .find_map(|l| l.strip_prefix("gitdir: "))
            .expect("gitfile gitdir line")
            .trim()
            .to_string();
        // <work>/.git points at <main>/.git/worktrees/linked; strip the tail.
        let common_refs = std::path::PathBuf::from(&work_git)
            .ancestors()
            .nth(2)
            .map(|p| p.join("refs").join("heads"))
            .expect("common dir above worktrees/<name>");

        std::fs::create_dir_all(&common_refs).unwrap();

        // Warm the pipeline with bounded retries before asserting: the probe
        // files land in the watched (non-recursive) worktree root. A single
        // assertion write races FSEvents stream installation (see
        // prime_watcher).
        prime_watcher(&rx, &work_path, PRIME_DEADLINE);

        // Retry-bounded assertion: write fresh uniquely-named refs until one
        // triggers repo-changed, instead of betting delivery on exactly one
        // write.
        let ref_deadline = Instant::now() + PRIME_DEADLINE;
        let mut n = 0u32;
        loop {
            std::fs::write(
                common_refs.join(format!("pulse_new_branch_{n}")),
                "0".repeat(40),
            )
            .unwrap();
            n += 1;
            if rx
                .recv_timeout(DEBOUNCE_QUIET + Duration::from_millis(200))
                .is_ok()
            {
                break;
            }
            assert!(
                Instant::now() < ref_deadline,
                "common-dir ref write must trigger repo-changed for the worktree ({n} probe refs \
                 within {PRIME_DEADLINE:?})"
            );
        }
    }

    // ---------------------------------------------------------------------------
    // Git-internals noise filter (audit A)
    // ---------------------------------------------------------------------------

    fn internal_roots_for(dir: &Path) -> Vec<std::path::PathBuf> {
        vec![dir.join(".git")]
    }

    /// Every transient the audit names must classify as noise when it lives
    /// inside the git directory.
    #[test]
    fn noise_predicate_drops_git_internal_transients() {
        let tmp = TempDir::new().unwrap();
        let git_dir = tmp.path().join(".git");
        std::fs::create_dir_all(git_dir.join("refs").join("heads")).unwrap();
        let roots = internal_roots_for(tmp.path());

        let noisy = [
            git_dir.join("index.lock"),
            git_dir.join("packed-refs.lock"),
            git_dir.join("config.lock"),
            git_dir.join("COMMIT_EDITMSG"),
            git_dir.join("COMMIT_EDITMSG.lock"),
            git_dir.join("ORIG_HEAD"),
            git_dir.join("FETCH_HEAD"),
            git_dir.join("MERGE_MSG"),
            git_dir.join("gc.log"),
            git_dir.join("gc.log.1.gz"),
            // core.splitIndex: every index read touches it to keep it alive.
            git_dir.join("sharedindex.2f8c39da4be50ca7706b3462cfdccc83fddd326d"),
        ];
        for path in noisy {
            std::fs::write(&path, b"x").unwrap();
            assert!(
                is_git_internal_noise(&path, &roots),
                "{} must be filtered as git-internal noise",
                path.display()
            );
            // Deletion churn (path already gone) stays noise too.
            std::fs::remove_file(&path).unwrap();
            assert!(
                is_git_internal_noise(&path, &roots),
                "deleted {} (lockfile lifecycle tail) must stay filtered",
                path.display()
            );
        }
    }

    /// `core.fsmonitor=true` makes every `git status` write a cookie under
    /// `.git/fsmonitor--daemon/cookies/`, so a refresh announced its own
    /// `repo-changed` and ran again about once a second. The daemon's state
    /// directory and socket are noise; a ref or a worktree file that merely
    /// shares the name is not.
    #[test]
    fn fsmonitor_daemon_writes_are_noise_but_lookalikes_are_not() {
        let tmp = TempDir::new().unwrap();
        let git_dir = tmp.path().join(".git");
        let cookies = git_dir.join("fsmonitor--daemon").join("cookies");
        std::fs::create_dir_all(&cookies).unwrap();
        std::fs::create_dir_all(git_dir.join("refs").join("heads")).unwrap();
        let roots = internal_roots_for(tmp.path());
        let rules = IgnoreRules::builtin();

        let daemon = [
            cookies.join("12345-0"),
            cookies.clone(),
            git_dir.join("fsmonitor--daemon"),
            git_dir.join("fsmonitor--daemon.ipc"),
        ];
        for path in &daemon {
            assert!(
                is_git_internal_noise(path, &roots),
                "{} is fsmonitor bookkeeping",
                path.display()
            );
        }
        let storm = daemon
            .iter()
            .fold(notify::Event::new(notify::EventKind::Any), |event, path| {
                event.add_path(path.clone())
            });
        assert!(
            !event_has_signal(&storm, &roots, tmp.path(), None, &rules),
            "a batch of cookie writes must not schedule a refresh"
        );

        for path in [
            git_dir.join("refs").join("heads").join("fsmonitor--daemon"),
            git_dir.join("index"),
            tmp.path().join("fsmonitor--daemon"),
        ] {
            let event = notify::Event::new(notify::EventKind::Any).add_path(path.clone());
            assert!(
                event_has_signal(&event, &roots, tmp.path(), None, &rules),
                "{} still moves the repository",
                path.display()
            );
        }
    }

    /// The main worktree's `.git` IS the common directory, and it is watched
    /// recursively, so it also sees every linked worktree's daemon under
    /// `worktrees/<name>/`. Relative to that root the first component is
    /// `worktrees`, and a `git status` in any linked worktree — an agent's, or
    /// a second tab — refreshed the main tab. That worktree's own HEAD and
    /// index still count, as before.
    #[test]
    fn a_linked_worktrees_fsmonitor_writes_are_noise_to_the_main_worktree() {
        let tmp = TempDir::new().unwrap();
        let common = tmp.path().join(".git");
        let linked = common.join("worktrees").join("agent-1");
        let cookies = linked.join("fsmonitor--daemon").join("cookies");
        std::fs::create_dir_all(&cookies).unwrap();
        let roots = vec![common.clone()];
        let rules = IgnoreRules::builtin();
        for path in [
            cookies.join("4242-7"),
            cookies.clone(),
            linked.join("fsmonitor--daemon"),
            linked.join("fsmonitor--daemon.ipc"),
        ] {
            assert!(
                is_git_internal_noise(&path, &roots),
                "{} is a linked worktree's fsmonitor bookkeeping",
                path.display()
            );
        }
        for path in [
            linked.join("HEAD"),
            linked.join("index"),
            common.join("worktrees").join("fsmonitor--daemon"),
            common.join("refs").join("heads").join("worktrees"),
        ] {
            let event = notify::Event::new(notify::EventKind::Any).add_path(path.clone());
            assert!(
                event_has_signal(&event, &roots, tmp.path(), None, &rules),
                "{} still moves the repository",
                path.display()
            );
        }
    }

    /// The universal form of the fsmonitor fix: run the whole read set one
    /// refresh performs, under every index and monitor feature that can make
    /// git write while it reads, and require that none of it is announced as
    /// a change. A new self-inflicted write — from any reader, under any of
    /// these configs — fails here instead of becoming the next storm.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_full_refresh_announces_no_change_under_any_index_feature() {
        use crate::engine::git_reader::GitReader;
        use std::process::Command;
        struct StopDaemon(PathBuf);
        impl Drop for StopDaemon {
            fn drop(&mut self) {
                let _ = Command::new("git")
                    .args(["fsmonitor--daemon", "stop"])
                    .current_dir(&self.0)
                    .output_locked();
            }
        }
        let git = |root: &Path, args: &[&str]| {
            let out = Command::new("git")
                .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
                .args(args)
                .current_dir(root)
                .output_locked()
                .expect("git");
            assert!(
                out.status.success(),
                "git {args:?}: {}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
        };
        // Every feature is covered, but `core.splitIndex` and
        // `feature.manyFiles` never share a repository: together they break
        // git's own add-then-commit (git 2.54, Apple Git-157 — the staged
        // file reads as untracked), which says nothing about GitPulse.
        let configs: [&[(&str, &str)]; 3] = [
            &[],
            &[
                ("core.fsmonitor", "true"),
                ("core.untrackedCache", "true"),
                ("feature.manyFiles", "true"),
                ("index.version", "4"),
            ],
            &[
                ("core.fsmonitor", "true"),
                ("core.untrackedCache", "true"),
                ("core.splitIndex", "true"),
            ],
        ];
        for config in configs {
            let temp = TempDir::new().unwrap();
            let (rx, stop, root) = spawn_loop(temp.path());
            let _daemon = StopDaemon(root.clone());
            crate::test_support::trust_repo(&root);
            for (key, value) in config {
                git(&root, &["config", key, value]);
            }
            std::fs::write(root.join("tracked.txt"), "one\n").unwrap();
            git(&root, &["add", "tracked.txt"]);
            git(&root, &["commit", "-m", "seed"]);
            git(&root, &["tag", "v1"]);
            git(&root, &["branch", "topic"]);
            std::fs::write(root.join("tracked.txt"), "two\n").unwrap();
            git(&root, &["stash", "push", "-m", "parked"]);
            std::fs::write(root.join("tracked.txt"), "three\n").unwrap();
            std::fs::write(root.join("staged.txt"), "s\n").unwrap();
            git(&root, &["add", "staged.txt"]);
            std::fs::write(root.join("untracked.txt"), "u\n").unwrap();
            let path = root.to_string_lossy().into_owned();
            let refresh = || {
                GitReader::get_status(&path).expect("status");
                GitReader::list_branches(&path).expect("branches");
                GitReader::list_tags(&path).expect("tags");
                GitReader::branch_stats(&path).expect("stats");
                crate::engine::stash::list(&path).expect("stash");
                crate::engine::repo_op::detect(&root).expect("operation");
                GitReader::head_id(&path).expect("head");
                GitReader::default_branch_name(&path).expect("default branch");
                GitReader::read_commit_history_paged(
                    &path,
                    0,
                    50,
                    None,
                    None,
                    crate::graph::RefScope::Named,
                )
                .expect("history");
                crate::graph::list_ref_decorations(&path, crate::graph::RefScope::Named)
                    .expect("refs");
            };
            // First refresh outside the window: it may start a daemon or
            // build a cache that later refreshes only read.
            refresh();
            prime_watcher(&rx, &root, PRIME_DEADLINE);
            for _ in 0..3 {
                refresh();
                thread::sleep(Duration::from_millis(100));
            }
            match rx.recv_timeout(DEBOUNCE_MAX_WAIT + Duration::from_millis(500)) {
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                other => panic!("a refresh under {config:?} announced repo-changed: {other:?}"),
            }
            let live_deadline = Instant::now() + PRIME_DEADLINE;
            let mut n = 0u32;
            loop {
                std::fs::write(root.join(format!("after-refresh-{n}.txt")), "x").unwrap();
                n += 1;
                if rx
                    .recv_timeout(DEBOUNCE_QUIET + Duration::from_millis(200))
                    .is_ok()
                {
                    break;
                }
                assert!(
                    Instant::now() < live_deadline,
                    "the watcher went silent under {config:?}, so the quiet window proves nothing"
                );
            }
            stop.store(true, Ordering::SeqCst);
        }
    }

    /// The loop itself, end to end: GitPulse's own read under fsmonitor must
    /// not announce a change. Proven live afterwards, so a dead watcher cannot
    /// pass this by staying silent.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_status_read_under_fsmonitor_does_not_announce_itself() {
        use std::process::Command;
        struct StopDaemon(PathBuf);
        impl Drop for StopDaemon {
            fn drop(&mut self) {
                let _ = Command::new("git")
                    .args(["fsmonitor--daemon", "stop"])
                    .current_dir(&self.0)
                    .output_locked();
            }
        }
        let status = |root: &Path| {
            let out = Command::new("git")
                .args(["status", "--porcelain=v2"])
                .env("GIT_OPTIONAL_LOCKS", "0")
                .current_dir(root)
                .output_locked()
                .expect("git status");
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        let temp = TempDir::new().unwrap();
        let (rx, stop, root) = spawn_loop(temp.path());
        let _daemon = StopDaemon(root.clone());
        let enabled = Command::new("git")
            .args(["config", "core.fsmonitor", "true"])
            .current_dir(&root)
            .output_locked()
            .expect("git config");
        assert!(enabled.status.success());
        // Start the daemon outside the measured window; its first run creates
        // the socket and state directory.
        status(&root);
        assert!(
            root.join(".git")
                .join("fsmonitor--daemon")
                .join("cookies")
                .is_dir(),
            "this git did not start a built-in fsmonitor daemon"
        );
        prime_watcher(&rx, &root, PRIME_DEADLINE);

        for _ in 0..3 {
            status(&root);
            thread::sleep(Duration::from_millis(150));
        }
        match rx.recv_timeout(DEBOUNCE_MAX_WAIT + Duration::from_millis(500)) {
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            other => panic!("git status under fsmonitor announced repo-changed: {other:?}"),
        }

        let live_deadline = Instant::now() + PRIME_DEADLINE;
        let mut n = 0u32;
        loop {
            std::fs::write(root.join(format!("after-status-{n}.txt")), "x").unwrap();
            n += 1;
            if rx
                .recv_timeout(DEBOUNCE_QUIET + Duration::from_millis(200))
                .is_ok()
            {
                break;
            }
            assert!(
                Instant::now() < live_deadline,
                "the watcher went silent, so the quiet window above proves nothing"
            );
        }
        stop.store(true, Ordering::SeqCst);
    }

    /// Ref moves, real index/HEAD/packed-refs writes and directories that
    /// merely share a noise-shaped name must all survive the filter; files
    /// OUTSIDE the git directories (worktree top level, e.g. Cargo.lock) are
    /// never classified as noise regardless of their name.
    #[test]
    fn noise_predicate_keeps_refs_real_state_dirs_and_worktree_files() {
        let tmp = TempDir::new().unwrap();
        let git_dir = tmp.path().join(".git");
        std::fs::create_dir_all(git_dir.join("refs").join("heads")).unwrap();
        let roots = internal_roots_for(tmp.path());

        let meaningful = [
            git_dir.join("index"),
            git_dir.join("HEAD"),
            git_dir.join("packed-refs"),
            git_dir.join("refs").join("heads").join("main"),
            // A branch literally named like a noise file lives under refs/.
            git_dir.join("refs").join("heads").join("ORIG_HEAD"),
            git_dir.join("refs").join("heads").join("topic.lock"),
        ];
        for path in meaningful {
            std::fs::write(&path, b"x").unwrap();
            assert!(
                !is_git_internal_noise(&path, &roots),
                "{} carries repository state and must not be filtered",
                path.display()
            );
        }

        // Directory carve-out: a directory sharing a noise name is kept.
        let noise_dir = git_dir.join("gc.log.old");
        std::fs::create_dir_all(&noise_dir).unwrap();
        assert!(
            !is_git_internal_noise(&noise_dir, &roots),
            "directory {} must not be dropped by the leaf-name rules",
            noise_dir.display()
        );

        // Scope check: identically-named entries outside the git dirs are
        // tracked-content territory (Cargo.lock, yarn.lock, ...) and keep
        // firing repo-changed.
        let worktree_lock = tmp.path().join("Cargo.lock");
        std::fs::write(&worktree_lock, b"x").unwrap();
        assert!(!is_git_internal_noise(&worktree_lock, &roots));
    }

    /// Live-index vacuums, ledgers, and sibling indexer writes under these
    /// directory names must not open the debounce window. Source files and
    /// dependency lockfiles still count as signal.
    #[test]
    fn noise_predicate_drops_generated_state_directories() {
        let tmp = TempDir::new().unwrap();
        let roots = internal_roots_for(tmp.path());
        let noisy = [
            tmp.path().join(".devcouncil"),
            tmp.path()
                .join(".devcouncil")
                .join("codeintel")
                .join("devmap.sqlite"),
            tmp.path().join(".devmap").join("store.sqlite"),
            tmp.path().join(".gitnexus").join("graph.json"),
        ];
        for path in noisy {
            assert!(
                is_generated_state_noise_in(&path, tmp.path()),
                "{} must be generated-state noise",
                path.display()
            );
            let event = notify::Event::new(notify::EventKind::Any).add_path(path);
            assert!(
                !event_has_signal(&event, &roots, tmp.path(), None, &IgnoreRules::builtin()),
                "generated-state-only events must not count as signal"
            );
        }

        let signal = [
            tmp.path().join("src").join("foo.rs"),
            tmp.path().join("Cargo.lock"),
        ];
        for path in signal {
            assert!(
                !is_generated_state_noise_in(&path, tmp.path()),
                "{}",
                path.display()
            );
            let event = notify::Event::new(notify::EventKind::Any).add_path(path);
            assert!(
                event_has_signal(&event, &roots, tmp.path(), None, &IgnoreRules::builtin()),
                "worktree content must still count as signal"
            );
        }

        let mixed = notify::Event::new(notify::EventKind::Any)
            .add_path(tmp.path().join(".devcouncil").join("codeintel"))
            .add_path(tmp.path().join("src").join("lib.rs"));
        assert!(
            event_has_signal(&mixed, &roots, tmp.path(), None, &IgnoreRules::builtin()),
            "a real worktree path in a mixed event must keep the signal"
        );

        let empty = notify::Event::new(notify::EventKind::Any);
        assert!(
            event_has_signal(&empty, &roots, tmp.path(), None, &IgnoreRules::builtin()),
            "unclassifiable empty-path events must fail open as signal"
        );

        let named = tmp.path().join(".devcouncil");
        let named_source = named.join("src").join("lib.rs");
        let named_store = named
            .join(".devcouncil")
            .join("codeintel")
            .join("devmap.sqlite");
        assert!(
            !is_generated_state_noise_in(&named_source, &named),
            "source inside a worktree named .devcouncil must stay signal"
        );
        assert!(
            is_generated_state_noise_in(&named_store, &named),
            "nested generated-state under that worktree must stay noise"
        );
        let named_source_event = notify::Event::new(notify::EventKind::Any).add_path(named_source);
        assert!(
            event_has_signal(
                &named_source_event,
                &roots,
                &named,
                None,
                &IgnoreRules::builtin()
            ),
            "a worktree named .devcouncil must still refresh on source edits"
        );

        let folded = [
            tmp.path()
                .join(".DevCouncil")
                .join("codeintel")
                .join("devmap.sqlite"),
            tmp.path().join(".GITNEXUS").join("graph.json"),
            tmp.path().join(".DevMap").join("store.sqlite"),
        ];
        for path in folded {
            assert!(
                is_generated_state_noise_in(&path, tmp.path()),
                "case-folded generated-state spelling must stay noise: {}",
                path.display()
            );
        }

        let not_generated = [
            tmp.path().join("notes.devcouncil.md"),
            tmp.path().join("devcouncil").join("src.rs"),
            tmp.path().join(".devcouncil.bak"),
        ];
        for path in not_generated {
            assert!(
                !is_generated_state_noise_in(&path, tmp.path()),
                "lookalike path must still count as signal: {}",
                path.display()
            );
        }
    }

    #[test]
    fn generated_noise_does_not_hide_source_under_named_ancestors() {
        let repo = Path::new("/workspace/.devmap/project");
        for path in [
            repo.join("src/lib.rs"),
            repo.join("src/.gitnexus/fixture.json"),
            repo.join(".git/refs/heads/.devcouncil/topic"),
        ] {
            assert!(
                !is_generated_state_noise_in(&path, repo),
                "source/ref was hidden: {}",
                path.display()
            );
        }
    }

    #[test]
    fn generated_noise_never_infers_identity_from_an_arbitrary_prefix_or_parent_escape() {
        let repo = Path::new("/workspace/repo");
        for path in [
            Path::new("/unrelated/workspace/repo/.devcouncil/index"),
            Path::new("/workspace/repo/.devcouncil/../src/lib.rs"),
            Path::new("/workspace/repo/.devmap/../../outside"),
        ] {
            assert!(
                !is_generated_state_noise_in(path, repo),
                "unexamined path hidden: {}",
                path.display()
            );
        }
    }

    // Real aliases must be established on disk. Arbitrary prepended path
    // components (for example /private/var/var) are not aliases of /var.
    #[test]
    #[cfg(unix)]
    fn generated_state_noise_survives_path_aliasing() {
        let temp = TempDir::new().unwrap();
        let worktree = temp.path().join(".devcouncil/project");
        std::fs::create_dir_all(&worktree).unwrap();
        let alias = temp.path().join("alias");
        std::os::unix::fs::symlink(&worktree, &alias).unwrap();
        for (relative, noise) in [
            (".devcouncil/codeintel/deleted.sqlite", true),
            (".DevCouncil", true),
            (".devmap/store", true),
            ("src/lib.rs", false),
            ("src/.gitnexus/fixture.json", false),
        ] {
            assert_eq!(
                is_generated_state_noise_in(&alias.join(relative), &worktree),
                noise,
                "{relative}"
            );
            assert_eq!(
                is_generated_state_noise_in(&worktree.join(relative), &alias),
                noise,
                "reverse {relative}"
            );
        }
        std::fs::remove_file(&alias).unwrap();
        std::fs::create_dir(&alias).unwrap();
        assert!(
            !is_generated_state_noise_in(&alias.join(".devcouncil/store"), &worktree),
            "a replaced alias must not retain stale identity"
        );
    }

    fn lexical_worktree() -> &'static Path {
        // Relative `.devcouncil/…` events are classified only when the worktree
        // is absolute. `/workspace/repo` is not absolute on Windows, so the
        // relative-event branch never ran and the storm test failed closed.
        #[cfg(windows)]
        {
            Path::new(r"C:\workspace\repo")
        }
        #[cfg(not(windows))]
        {
            Path::new("/workspace/repo")
        }
    }

    fn unrelated_generated_state(i: usize) -> PathBuf {
        #[cfg(windows)]
        {
            PathBuf::from(format!(
                r"C:\unrelated\workspace\repo\.devcouncil\n{i}.sqlite"
            ))
        }
        #[cfg(not(windows))]
        {
            PathBuf::from(format!("/unrelated/workspace/repo/.devcouncil/n{i}.sqlite"))
        }
    }

    #[test]
    fn relative_generated_state_event_is_noise_only_against_an_absolute_worktree() {
        assert!(
            !is_generated_state_noise_in(
                Path::new(".devcouncil/deleted.sqlite"),
                Path::new("workspace/repo")
            ),
            "a relative worktree cannot classify a relative event — that is the Windows /workspace/repo failure"
        );
        assert!(is_generated_state_noise_in(
            Path::new(".devcouncil/deleted.sqlite"),
            lexical_worktree()
        ));
    }

    #[test]
    fn generated_state_alias_storm_keeps_source_signal() {
        let worktree = lexical_worktree();
        for i in 0..1_000 {
            assert!(is_generated_state_noise_in(
                &worktree.join(format!(".devcouncil/n{i}.sqlite")),
                worktree
            ));
            assert!(
                is_generated_state_noise_in(Path::new(".devcouncil/deleted.sqlite"), worktree),
                "relative generated-state events must classify against an absolute worktree"
            );
            assert!(!is_generated_state_noise_in(
                &worktree.join(format!("src/n{i}.rs")),
                worktree
            ));
            assert!(!is_generated_state_noise_in(
                &unrelated_generated_state(i),
                worktree
            ));
        }
    }

    /// Symlinked worktree: FSEvents may deliver the physical path while the
    /// watch key is the canonical one (or the reverse). Classification must
    /// still agree after one existing-ancestor canonicalize.
    #[test]
    #[cfg(unix)]
    fn generated_state_noise_matches_symlinked_worktree_alias() {
        let temp = TempDir::new().unwrap();
        let physical = temp.path().join("physical-repo");
        std::fs::create_dir_all(physical.join(".devcouncil")).unwrap();
        let link = temp.path().join("linked-repo");
        std::os::unix::fs::symlink(&physical, &link).unwrap();
        let canonical = physical.canonicalize().unwrap();
        let via_link = link.join(".devcouncil").join("devmap.sqlite");
        assert!(
            is_generated_state_noise_cached(&via_link, &canonical, Some(&canonical)),
            "state under a symlink spelling of the worktree must stay noise"
        );
        assert!(
            !is_generated_state_noise_cached(
                &link.join("src").join("main.rs"),
                &canonical,
                Some(&canonical)
            ),
            "source under the symlink spelling must stay signal"
        );
    }

    /// `$DEVMAP_HOME` outside the worktree is not watched as a top-level
    /// state dir name. Writes there must not be classified as worktree noise
    /// via a shared suffix, and in-tree `.devcouncil` still is.
    #[test]
    fn generated_state_noise_ignores_external_devmap_home_suffix() {
        let temp = TempDir::new().unwrap();
        let worktree = temp.path().join("repo");
        let home = temp.path().join("devmap-home");
        std::fs::create_dir_all(worktree.join(".devcouncil")).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        let external = home.join("codeintel").join("devmap.sqlite");
        assert!(
            !is_generated_state_noise_in(&external, &worktree),
            "DEVMAP_HOME outside the worktree must not be inferred as noise"
        );
        assert!(is_generated_state_noise_in(
            &worktree.join(".devcouncil").join("store.sqlite"),
            &worktree
        ));
    }

    #[test]
    fn generated_state_mixed_case_storm_stays_noise() {
        let worktree = Path::new("/workspace/repo");
        let spellings = [
            ".devcouncil",
            ".DevCouncil",
            ".DEVCOUNCIL",
            ".devmap",
            ".DevMap",
            ".gitnexus",
            ".GITNEXUS",
        ];
        for i in 0..10_000 {
            let name = spellings[i % spellings.len()];
            assert!(
                is_generated_state_noise_in(
                    &worktree.join(format!("{name}/n{i}.sqlite")),
                    worktree
                ),
                "{name} at {i}"
            );
        }
    }

    /// Linked-worktree internal-root storms used to pay up to 65 realpath
    /// calls per event. Classification of 10k common-dir paths must stay
    /// cheap, and a `refs/heads` write must still count as signal.
    #[test]
    #[cfg(unix)]
    fn linked_worktree_internal_root_storm_is_cheap_and_refs_still_signal() {
        let (_main, _work_parent, work_path) = init_linked_worktree();
        let worktree = work_path.canonicalize().unwrap();
        let gitfile = std::fs::read_to_string(work_path.join(".git")).unwrap();
        let work_git = gitfile
            .lines()
            .find_map(|l| l.strip_prefix("gitdir: "))
            .expect("gitfile gitdir line")
            .trim()
            .to_string();
        let work_git_path = PathBuf::from(&work_git);
        let common = work_git_path
            .ancestors()
            .nth(2)
            .expect("common dir above worktrees/<name>")
            .to_path_buf();
        let internal_roots = vec![work_git_path, common.clone()];
        let worktree_canonical = worktree.clone();

        let started = Instant::now();
        for i in 0..10_000 {
            let path = common.join(format!("objects/pack/tmp-{i}.pack"));
            let event = notify::Event::new(notify::EventKind::Any).add_path(path);
            // Pack files are not leaf-noise names and sit under internal_roots,
            // so they are signal — the point is the classification stays cheap.
            let _ = event_has_signal(
                &event,
                &internal_roots,
                &worktree,
                Some(&worktree_canonical),
                &IgnoreRules::builtin(),
            );
        }
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_millis(750),
            "10k linked-worktree internal-root classifications took {elapsed:?} (alias walk regression)"
        );

        let refs_event = notify::Event::new(notify::EventKind::Any)
            .add_path(common.join("refs").join("heads").join("feature"));
        assert!(
            event_has_signal(
                &refs_event,
                &internal_roots,
                &worktree,
                Some(&worktree_canonical),
                &IgnoreRules::builtin(),
            ),
            "refs/heads under the common dir must still emit for a linked worktree"
        );
        let lock_event =
            notify::Event::new(notify::EventKind::Any).add_path(common.join("index.lock"));
        assert!(
            !event_has_signal(
                &lock_event,
                &internal_roots,
                &worktree,
                Some(&worktree_canonical),
                &IgnoreRules::builtin(),
            ),
            "index.lock under the common dir must remain git-internal noise"
        );
    }

    #[test]
    fn generated_state_storm_is_quiet_but_source_edits_still_emit() {
        let dir = TempDir::new().unwrap();
        for name in [".devcouncil", ".devmap", ".gitnexus"] {
            std::fs::create_dir_all(dir.path().join(name)).unwrap();
        }
        let (rx, stop, canonical) = spawn_loop(dir.path());
        struct StopOnDrop(Arc<AtomicBool>);
        impl Drop for StopOnDrop {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let _stop = StopOnDrop(stop);
        prime_watcher(&rx, &canonical, PRIME_DEADLINE);
        await_watcher_quiescence(&rx);
        for i in 0..120 {
            for name in [".devcouncil", ".devmap", ".gitnexus"] {
                std::fs::write(canonical.join(name).join("index.tmp"), i.to_string()).unwrap();
            }
            thread::sleep(Duration::from_millis(25));
        }
        assert!(
            rx.recv_timeout(DEBOUNCE_MAX_WAIT + Duration::from_secs(1))
                .is_err(),
            "generated index writes fed back into repo-changed"
        );
        await_watcher_quiescence(&rx);
        let alive_deadline = Instant::now() + PRIME_DEADLINE;
        let mut n = 0u32;
        loop {
            std::fs::write(
                canonical.join(format!("after-generated-storm-{n}.rs")),
                "fn x() {}",
            )
            .unwrap();
            n += 1;
            if rx
                .recv_timeout(DEBOUNCE_QUIET + Duration::from_millis(200))
                .is_ok()
            {
                break;
            }
            assert!(
                Instant::now() < alive_deadline,
                "a real worktree file must still emit after the generated-state storm ({n} probes)"
            );
        }
    }

    /// Regression (audit A): a continuous stream of pure git-internals noise
    /// (lockfile create/delete cycles, message-file edits, gc logs) used to
    /// open the debounce window and force a full refresh every
    /// DEBOUNCE_MAX_WAIT forever. Filtered paths must never accumulate into
    /// an emission, while the pipeline stays alive for real events.
    #[test]
    fn watch_loop_pure_noise_stream_emits_nothing_and_stays_alive() {
        let temp = TempDir::new().unwrap();
        let (rx, stop, root) = spawn_loop(temp.path());
        let git_dir = root.join(".git");
        prime_watcher(&rx, &root, PRIME_DEADLINE);

        // Pump ONLY noise for well past the anti-starvation bound (2s max
        // wait + settle margin): an unfiltered event would be forced out as
        // an emission within ~2s of arriving, so 3s of pumping plus polling
        // the receiver throughout is enough to catch a broken filter.
        let pump_deadline = Instant::now() + Duration::from_secs(3);
        let mut i = 0u32;
        let mut leaked = None;
        while Instant::now() < pump_deadline {
            for name in [
                "index.lock",
                "COMMIT_EDITMSG",
                "ORIG_HEAD",
                "FETCH_HEAD",
                "MERGE_MSG",
                "gc.log.9",
            ] {
                let _ = std::fs::write(git_dir.join(name), format!("noise-{i}"));
            }
            i += 1;
            let _ = std::fs::remove_file(git_dir.join("index.lock"));
            match rx.recv_timeout(Duration::from_millis(50)) {
                Ok(path) => {
                    leaked = Some(path);
                    break;
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        assert!(
            leaked.is_none(),
            "git-internals noise must not trigger repo-changed (got {leaked:?})"
        );

        // Drain any straggler deliveries racing the OS backend, then prove
        // the loop is still alive with a real worktree edit (retry-bounded).
        await_watcher_quiescence(&rx);
        let alive_deadline = Instant::now() + PRIME_DEADLINE;
        let mut n = 0u32;
        loop {
            std::fs::write(root.join(format!("post-noise-alive-{n}.txt")), "x").unwrap();
            n += 1;
            if rx
                .recv_timeout(DEBOUNCE_QUIET + Duration::from_millis(200))
                .is_ok()
            {
                break;
            }
            assert!(
                Instant::now() < alive_deadline,
                "real edits must still trigger after the noise gate ({n} probes)"
            );
        }
        stop.store(true, Ordering::SeqCst);
    }

    /// Mixed burst: noise paths next to a genuine `.git/index` write settle
    /// into exactly one emission — the noise half is swallowed, the state
    /// half still debounces like any real change.
    #[test]
    fn watch_loop_mixed_noise_and_index_write_emits_exactly_once() {
        let temp = TempDir::new().unwrap();
        let (rx, stop, root) = spawn_loop(temp.path());
        let git_dir = root.join(".git");
        prime_watcher(&rx, &root, PRIME_DEADLINE);

        let burst_deadline = Instant::now() + PRIME_DEADLINE;
        let mut attempt = 0u32;
        loop {
            assert!(
                Instant::now() < burst_deadline,
                "no cleanly coalesced mixed burst within {PRIME_DEADLINE:?} ({attempt} attempts)"
            );
            for i in 0..10 {
                let _ = std::fs::write(
                    git_dir.join("index.lock"),
                    format!("transient lock {attempt}-{i}"),
                );
                let _ = std::fs::remove_file(git_dir.join("index.lock"));
                let _ = std::fs::write(git_dir.join("COMMIT_EDITMSG"), "wip");
            }
            // The one significant event: a real index write.
            std::fs::write(git_dir.join("index"), b"mixed-burst-index").unwrap();
            attempt += 1;

            // Exactly one settled callback for the whole burst.
            let Ok(first) = rx.recv_timeout(Duration::from_secs(10)) else {
                continue; // backend delivered nothing yet: retry
            };
            assert!(!first.is_empty());

            // Quiet window well past the 400ms settle: no further callbacks
            // may arrive, because the noise half produced nothing to debounce.
            match rx.recv_timeout(Duration::from_millis(900)) {
                Ok(_) => {
                    // Stray late delivery split the burst: quiesce and retry.
                    await_watcher_quiescence(&rx);
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => break,
                Err(e) => panic!("channel failed during quiet window: {e}"),
            }
        }
        stop.store(true, Ordering::SeqCst);
    }

    /// The leaf-name rules target transient FILES: a directory whose name
    /// matches the deny list (`gc.log*`, `*.lock`, ...) must still fire
    /// repo-changed when it appears inside the git directory.
    #[test]
    fn watch_loop_directory_named_like_noise_still_triggers() {
        let temp = TempDir::new().unwrap();
        let (rx, stop, root) = spawn_loop(temp.path());
        let git_dir = root.join(".git");
        prime_watcher(&rx, &root, PRIME_DEADLINE);

        let dir_deadline = Instant::now() + PRIME_DEADLINE;
        let mut n = 0u32;
        loop {
            std::fs::create_dir_all(git_dir.join(format!("gc.log.attempt-{n}"))).unwrap();
            n += 1;
            if rx
                .recv_timeout(DEBOUNCE_QUIET + Duration::from_millis(200))
                .is_ok()
            {
                break;
            }
            assert!(
                Instant::now() < dir_deadline,
                "directory creation inside .git must not be swallowed by the noise filter \
                 ({n} attempts within {PRIME_DEADLINE:?})"
            );
        }
        stop.store(true, Ordering::SeqCst);
    }
}
