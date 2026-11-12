/// The door `benches/watcher.rs` measures the event classifier through.
#[doc(hidden)]
pub mod bench_seam;
pub mod debouncer;
pub(crate) mod filter;
#[cfg(test)]
mod registration_timing;
pub(crate) mod skip;
#[cfg(all(test, unix))]
mod storm;

pub use debouncer::RepoFileWatcher;

use crate::engine::git_cli::{
    git_text_with_timeout, resolve_git_common_dir, resolve_git_dir, validate_repo,
};
use crate::engine::git_reader::FileStatus;
use crate::ipc_error::CommandError;
use notify::{Event, EventKind};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};

/// Native watch sessions held at once. The frontend pools watches by recency
/// (src/lib/repos/watchPool.ts, `WATCH_POOL_SIZE`) and stays strictly below
/// this, so an eviction can admit the newcomer before the evicted session has
/// finished retiring, and one leaked slot cannot refuse the repository a person
/// just opened. Open tabs are no longer bounded by it.
pub const MAX_WATCHES: usize = 32;

/// What a watch loop is woken with: a path-less event.
type WakeSender = std::sync::mpsc::SyncSender<Result<Event, notify::Error>>;

/// Ends a watch loop, and wakes it to notice.
///
/// The loop blocks on its event channel for as long as nothing happens, which
/// is most of the time, so a flag alone would be seen at the next event or the
/// next heartbeat. Setting it therefore also queues a path-less event on that
/// channel through a clone of its sender; the loop checks the flag as soon as
/// anything arrives. Every way of stopping a loop goes through [`Self::stop`]
/// so there is no way to set the flag and forget the wake.
pub(crate) struct StopSignal {
    stopped: AtomicBool,
    wake: Mutex<Option<WakeSender>>,
}

impl StopSignal {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            stopped: AtomicBool::new(false),
            wake: Mutex::new(None),
        })
    }

    /// Gives the signal the channel to wake. A signal already stopped wakes it
    /// now, so a stop that raced the loop's start is not lost.
    pub(crate) fn arm(&self, wake: WakeSender) {
        let mut slot = crate::supervisor::lock_or_recover(&self.wake);
        *slot = Some(wake);
        if self.stopped.load(Ordering::SeqCst) {
            Self::send_wake(slot.as_ref());
        }
    }

    pub(crate) fn stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        Self::send_wake(crate::supervisor::lock_or_recover(&self.wake).as_ref());
    }

    pub(crate) fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }

    /// A full queue is a loop with events to read, which it will check the flag
    /// on; a gone receiver is a loop that has already ended.
    fn send_wake(wake: Option<&WakeSender>) {
        if let Some(wake) = wake {
            let _ = wake.try_send(Ok(Event::new(EventKind::Other)));
        }
    }
}

pub struct WatchSession {
    stop: Arc<StopSignal>,
    retired: Option<std::sync::mpsc::Receiver<Result<(), String>>>,
    /// Path spellings that identified this session when it was created:
    /// the canonical map key plus whatever raw path the caller supplied.
    /// Kept so `unwatch` can still resolve the slot after the watched
    /// directory disappears and canonicalization starts failing (which
    /// would otherwise leak one of MAX_WATCHES slots forever).
    aliases: Vec<String>,
    /// The callback filter and the poll-skip state it feeds; see [`skip`].
    /// `None` only for a slot reserved without a backend.
    filter: Option<Arc<filter::EventFilter>>,
}

impl Drop for WatchSession {
    fn drop(&mut self) {
        self.stop.stop();
    }
}

#[derive(Clone, Default)]
pub struct WatcherState {
    /// Shared with each watch thread so it can reap its own session when the
    /// repository vanishes under it (audit D1). An Arc keeps that handoff
    /// cheap while every other caller keeps using `&WatcherState`.
    sessions: std::sync::Arc<Mutex<HashMap<String, WatchSession>>>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RepoChangedPayload {
    pub path: String,
    /// What moved since the previous emission, so a view that depends on
    /// none of it can skip its refresh.
    pub change: RepoChange,
}

/// What a settled change touched, read from the same paths the noise gate
/// admitted — never a second admission rule, which is how a self-echo loop
/// would come back.
///
/// On Linux and Windows the worktree watch is non-recursive, so `paths` names
/// the top-level entries that fired, not every file below them; deeper edits
/// surface as [`ChangeKind::Index`] or [`ChangeKind::Refs`] writes. On macOS
/// the watch is recursive and `paths` names the files themselves. A consumer
/// must read `paths` as "at least these", never as "only these": it is bounded
/// by [`MAX_CHANGED_PATHS`] everywhere.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct RepoChange {
    /// Sorted and deduplicated.
    pub kinds: Vec<ChangeKind>,
    /// Worktree-relative, `/`-separated, sorted; at most [`MAX_CHANGED_PATHS`].
    pub paths: Vec<String>,
    /// More worktree paths fired than `paths` holds.
    pub paths_truncated: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    /// `HEAD`, `refs/`, `packed-refs` or a reflog of this checkout.
    Refs,
    /// This checkout's index. Also written by every git command that
    /// rewrites tracked files (checkout, reset, merge, stash), so it must be
    /// read as "the worktree may have changed".
    Index,
    /// Repository configuration.
    Config,
    /// `info/` — exclude rules and sparse-checkout patterns.
    Ignore,
    /// The object store.
    Objects,
    /// Anything else inside a git directory: merge/rebase state, another
    /// linked worktree's private directory, hooks, shallow, modules.
    GitState,
    /// A worktree entry, named in [`RepoChange::paths`].
    Worktree,
    /// A worktree entry that is, or may contain, a Markdown document: a
    /// Markdown name as the document vault reads it, a directory, or a removed
    /// entry with no extension (it may have been one). Decided here so the
    /// extension list has one owner, `markdev_vault`.
    Documents,
    /// The backend could not say what changed (an error, or an event naming
    /// no path). Every view must treat it as touching everything.
    Unknown,
}

pub const MAX_CHANGED_PATHS: usize = 64;

/// Accumulates one debounce window's [`RepoChange`].
#[derive(Default)]
struct ChangeAccumulator {
    kinds: std::collections::BTreeSet<ChangeKind>,
    paths: std::collections::BTreeSet<String>,
    truncated: bool,
}

impl ChangeAccumulator {
    fn kind(&mut self, kind: ChangeKind) {
        self.kinds.insert(kind);
    }

    fn worktree(&mut self, relative: &Path) {
        self.kinds.insert(ChangeKind::Worktree);
        let spelled = relative
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        if self.paths.contains(&spelled) {
            return;
        }
        if self.paths.len() >= MAX_CHANGED_PATHS {
            self.truncated = true;
        } else {
            self.paths.insert(spelled);
        }
    }

    fn take(&mut self) -> RepoChange {
        let taken = std::mem::take(self);
        RepoChange {
            kinds: taken.kinds.into_iter().collect(),
            paths: taken.paths.into_iter().collect(),
            paths_truncated: taken.truncated,
        }
    }
}

/// See [`ChangeKind::Documents`]. Fails toward "may": an entry whose kind
/// cannot be read and that has no extension is assumed to be a directory.
fn may_hold_documents(path: &Path, relative: &Path) -> bool {
    if markdev_vault::note::has_markdown_extension(&relative.to_string_lossy()) {
        return true;
    }
    // Followed: a symlinked docs directory holds documents too.
    match std::fs::metadata(path) {
        Ok(meta) => meta.is_dir(),
        Err(_) => relative.extension().is_none(),
    }
}

/// The class of a git-directory path the noise gate already admitted.
///
/// `internal_roots[0]` is this checkout's own git directory. A path inside it
/// is classified by name. A path that is only under the common directory is
/// shared state (refs, objects, config) — except under `worktrees/<name>/`,
/// another linked worktree's private directory, which moves this
/// repository's worktree list and never this checkout's files. A linked
/// checkout's own directory is that shape too, but it is `internal_roots[0]`
/// and matched first, so the rule only ever sees its siblings.
fn git_path_kind(path: &Path, internal_roots: &[PathBuf]) -> ChangeKind {
    let Some((relative, is_own)) = internal_roots.iter().enumerate().find_map(|(i, root)| {
        path.strip_prefix(root)
            .ok()
            .map(|relative| (relative, i == 0))
    }) else {
        return ChangeKind::GitState;
    };
    match relative
        .components()
        .next()
        .and_then(|c| c.as_os_str().to_str())
    {
        Some("worktrees") => ChangeKind::GitState,
        Some("HEAD" | "packed-refs" | "refs" | "logs") => ChangeKind::Refs,
        // The common directory's `index` is the main checkout's, not ours.
        Some("index") if is_own => ChangeKind::Index,
        Some("config" | "config.worktree") => ChangeKind::Config,
        Some("info") => ChangeKind::Ignore,
        Some("objects") => ChangeKind::Objects,
        _ => ChangeKind::GitState,
    }
}

impl WatcherState {
    fn lock_sessions(&self) -> Result<MutexGuard<'_, HashMap<String, WatchSession>>, String> {
        Ok(crate::supervisor::lock_or_recover(&self.sessions))
    }

    /// The filter of the watch on `key` (a canonical repository path). A path
    /// with no session (never watched, failed to register, parked, evicted,
    /// unwatched, or reaped by its own loop) has none, which is the same answer
    /// as a watch that cannot vouch: the caller reads.
    fn filter_for(&self, key: &str) -> Option<Arc<filter::EventFilter>> {
        let guard = self.lock_sessions().ok()?;
        guard.get(key)?.filter.clone()
    }

    /// Drops a reserved slot only if it is still the session we inserted.
    /// Used when thread spawn fails after the sessions lock has been released.
    fn abandon_watch_slot(&self, key: &str, stop: &Arc<StopSignal>) -> Result<(), String> {
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
) -> Result<Option<Arc<StopSignal>>, String> {
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
    let stop = StopSignal::new();
    sessions.insert(
        key.to_string(),
        WatchSession {
            stop: stop.clone(),
            retired,
            aliases,
            filter: None,
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
/// How many ignored FILES (not directories) one watch will remember, and how
/// many bytes of their names. Past either bound the rest are simply not
/// recorded: an unrecorded ignored file is forwarded, which costs a refresh and
/// is never wrong.
const MAX_IGNORE_FILES: usize = 4096;
const MAX_IGNORE_FILE_BYTES: usize = 256 * 1024;
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
pub(crate) struct IgnoreRules {
    /// Folded, `/`-separated directory prefixes (`scratch`, `pkg/generated`).
    prefixes: HashSet<String>,
    /// Ignored files the listing printed one by one (a nested
    /// `.claude/settings.local.json`, `lcov.info`, a `.DS_Store`), exactly as
    /// printed and relative to the worktree root. A write to one of these is
    /// not forwarded by the recursive watch's callback filter; see
    /// [`filter::EventFilter::admit`]. Bounded by [`MAX_IGNORE_FILES`] and
    /// [`MAX_IGNORE_FILE_BYTES`].
    files: HashSet<String>,
    /// `None` only when the query finished and the prefix list was complete.
    load_error: Option<String>,
    /// True when the query finished and the list was cut at
    /// [`MAX_IGNORE_PREFIXES`]. That is a final, partial answer: asking again
    /// returns the same cut list, so [`rules_due`] does not.
    truncated: bool,
}

impl IgnoreRules {
    /// Rules with no repository-specific prefixes: the built-in build-dir names
    /// only. What the callback filter uses until the loop publishes its first
    /// load; the loop itself always goes through [`IgnoreRules::load`], whose
    /// failure path records its reason instead of returning an empty set that
    /// looks complete.
    fn builtin() -> Self {
        Self {
            prefixes: HashSet::new(),
            files: HashSet::new(),
            load_error: None,
            truncated: false,
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
                let files = parse_ignored_files(&stdout);
                if truncated {
                    let error = format!(
                        "ignore directory list truncated at {MAX_IGNORE_PREFIXES}; further ignored directories were not recorded"
                    );
                    log::warn!(target: "watcher", "{}: {error}", repo.display());
                    Self {
                        prefixes,
                        files,
                        load_error: Some(error),
                        truncated: true,
                    }
                } else {
                    Self {
                        prefixes,
                        files,
                        load_error: None,
                        truncated: false,
                    }
                }
            }
            Err(error) => {
                log::warn!(
                    target: "watcher",
                    "gitignore query failed for {}: {error}",
                    repo.display(), error = error.message()
                );
                Self {
                    prefixes: HashSet::new(),
                    files: HashSet::new(),
                    load_error: Some(error.into_message()),
                    truncated: false,
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

/// The ignored files the listing printed one by one (the entries that do not
/// end in a separator), up to [`MAX_IGNORE_FILES`] and
/// [`MAX_IGNORE_FILE_BYTES`] of names. Nothing past a bound is recorded.
fn parse_ignored_files(stdout: &str) -> HashSet<String> {
    let mut files = HashSet::new();
    let mut bytes = 0usize;
    for field in stdout.split('\0') {
        if field.is_empty() || field.ends_with('/') || field.ends_with('\\') {
            continue;
        }
        let name = field.trim_start_matches("./");
        if files.len() >= MAX_IGNORE_FILES || bytes + name.len() > MAX_IGNORE_FILE_BYTES {
            break;
        }
        bytes += name.len();
        files.insert(name.to_string());
    }
    files
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
    rules.load_error.is_some() && !rules.truncated && elapsed >= IGNORE_RETRY
}

/// What makes the ignore list worth asking for again: a rule file edit, or a
/// directory that appeared in the worktree (a build's new `dist/` is not in the
/// list until it is reloaded, and until then its writes are forwarded).
///
/// Deliberately NOT here: any write to `index`, `HEAD` or `config`. A terminal
/// `git status` rewrites the index, and a reload per index write is a spawn per
/// git command anyone runs. The list only decides which events are forwarded
/// for an immediate refresh, never what status shows, so it may be a little
/// stale. Reloads are coalesced to one per [`SCAN_COALESCE`].
///
/// The directory trigger exists only for the recursive watch (`recursive`):
/// where the root watch is non-recursive (Linux, Windows) a new directory
/// delivers nothing below it, so there is nothing for a stale list to hide and
/// no reason to spend a `git ls-files` on it.
fn event_may_change_ignore_rules(
    event: &Event,
    internal_roots: &[PathBuf],
    recursive: bool,
) -> bool {
    let rule_file =
        event.paths.iter().any(
            |path| match path.file_name().and_then(|name| name.to_str()) {
                Some(".gitignore") => true,
                Some("exclude") => path.parent().is_some_and(|parent| parent.ends_with("info")),
                _ => false,
            },
        );
    rule_file
        || (recursive
            && matches!(
                event.kind,
                EventKind::Create(notify::event::CreateKind::Folder)
            )
            && event
                .paths
                .iter()
                .any(|path| !internal_roots.iter().any(|root| path.starts_with(root))))
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
    let mut ignored = ChangeAccumulator::default();
    record_event_signal(
        event,
        internal_roots,
        worktree,
        worktree_canonical,
        rules,
        &mut ignored,
    )
}

/// [`event_has_signal`]'s one implementation: also records, into `change`,
/// the class of every path it admits — and only of those.
fn record_event_signal(
    event: &Event,
    internal_roots: &[PathBuf],
    worktree: &Path,
    worktree_canonical: Option<&Path>,
    rules: &IgnoreRules,
    change: &mut ChangeAccumulator,
) -> bool {
    // The backend says events were lost (FSEvents MustScanSubDirs, inotify
    // Q_OVERFLOW): whatever changed is unknowable, and these name the git dir
    // or worktree root — paths the noise rules below would drop. Refresh
    // everything, whatever the paths say.
    if event.need_rescan() {
        change.kind(ChangeKind::Unknown);
        return true;
    }
    // Reads move nothing. inotify reports opens and closes (the app's own
    // `git status` opens the index and every tracked file), so counting them
    // made each refresh announce itself and run again. A write is always
    // reported as a modify of its own, so nothing real is lost.
    if matches!(event.kind, EventKind::Access(_)) {
        return false;
    }
    if event.paths.is_empty() {
        change.kind(ChangeKind::Unknown);
        return true;
    }
    let mut significant = false;
    for path in &event.paths {
        if is_git_internal_noise(path, internal_roots) {
            continue;
        }
        // Linked-worktree ref/object writes live under the common git dir,
        // outside the worktree checkout. They are never `.devcouncil` noise
        // and must not trigger the path-alias canonicalize walk.
        if internal_roots.iter().any(|root| path.starts_with(root)) {
            change.kind(git_path_kind(path, internal_roots));
            significant = true;
            continue;
        }
        if is_generated_state_noise_cached(path, worktree, worktree_canonical) {
            continue;
        }
        match worktree_relative(path, worktree, worktree_canonical) {
            Some(relative) => {
                if !is_build_or_ignored_path(&relative, rules) {
                    change.worktree(&relative);
                    if may_hold_documents(path, &relative) {
                        change.kind(ChangeKind::Documents);
                    }
                    significant = true;
                }
            }
            // The worktree directory itself is what FSEvents reports when a
            // storm coalesces. It names no file. A `..` path still fails open:
            // refusing to classify an escape is not the same as calling it noise.
            None => {
                if !is_worktree_root(path, worktree, worktree_canonical) {
                    change.kind(ChangeKind::Unknown);
                    significant = true;
                }
            }
        }
    }
    significant
}

/// True when `path` is generated per-worktree state that GitPulse (or a
/// sibling indexer) writes as a *consequence* of a refresh, never as the
/// worktree change that should start one.
///
/// Live index vacuums, DevCouncil ledgers, and GitNexus indexes live under
/// `.devcouncil`, `.devmap`, and `.gitnexus`. A worktree watch is
/// non-recursive on Linux and Windows, but those top-level directories still
/// fire there, and on macOS the watch is recursive so their nested writes
/// arrive too. Ledger appends already have their own
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
    // is watched recursively, so whatever changed delivers its own event. On
    // Windows the worktree-root watch is non-recursive and
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
    if is_teardown_aside_path(path, internal_roots) {
        return true;
    }
    if is_other_checkout_state(path, internal_roots) {
        return true;
    }
    is_noise_leaf_name(path)
}

/// State that belongs to another checkout of the same repository, seen only
/// because the common directory is watched for shared refs.
///
/// `git add` in a sibling worktree rewrites `<common>/worktrees/<name>/index`
/// (and its `logs/`), and a commit in the main checkout moves the common
/// `HEAD`, `index` and `logs/` — none of it changes this checkout's files,
/// index or branch, yet N agents running `git add` made N full refreshes in
/// every other worktree. `refs/` stays shared (a branch moved by anyone shows
/// in this checkout's branch list), and so does a sibling's own `HEAD` and the
/// existence of its directory, which move this repository's worktree list.
/// `internal_roots[0]` is this checkout's own git directory.
fn is_other_checkout_state(path: &Path, internal_roots: &[PathBuf]) -> bool {
    let Some((relative, is_own)) = internal_roots.iter().enumerate().find_map(|(i, root)| {
        path.strip_prefix(root)
            .ok()
            .map(|relative| (relative, i == 0))
    }) else {
        return false;
    };
    let mut components = relative
        .components()
        .map(|c| c.as_os_str().to_str().unwrap_or(""));
    match (components.next(), components.next(), components.next()) {
        // `worktrees/<name>/<index|logs>/...`: a sibling's private files. For
        // a linked checkout `internal_roots[0]` is its own `worktrees/<name>`,
        // so its own `index` strips to a first component of `index` instead.
        (Some("worktrees"), Some(_), Some("index" | "logs")) => true,
        // The common directory's own checkout state (`is_own` is false only
        // for the shared directory behind a linked worktree).
        (Some("HEAD" | "index" | "logs"), _, _) => !is_own,
        _ => false,
    }
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

/// `gitpulse-teardown/` directly below a git directory (the common one, for a
/// linked worktree): where a worktree teardown moves the lane to delete it
/// (`engine::worktree::TEARDOWN_DIR`). The delete is one unlink per file, tens
/// of thousands of them, in a directory that is watched recursively and that
/// no git state lives in.
fn is_teardown_aside_path(path: &Path, internal_roots: &[PathBuf]) -> bool {
    internal_roots.iter().any(|root| {
        path.strip_prefix(root).is_ok_and(|relative| {
            relative
                .components()
                .next()
                .is_some_and(|c| c.as_os_str() == crate::engine::worktree::TEARDOWN_DIR)
        })
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

/// How often a loop with nothing to do looks anyway. It is the bound on the
/// two things no event announces: a repository that vanished without a
/// filesystem event reaching us, and a backend that died. Everything else wakes
/// the loop by sending to it.
const IDLE_HEARTBEAT: Duration = Duration::from_secs(30);
/// How soon a repository that has just been missed is looked for again, so
/// that the loop's three consecutive misses take as long as they ever did
/// rather than three heartbeats.
const DEAD_RECHECK: Duration = Duration::from_millis(200);

/// When a failed ignore-rules query is next due for its retry, if one is.
fn rules_retry_at(rules: &IgnoreRules, last_load: Instant) -> Option<Instant> {
    (rules.load_error.is_some() && !rules.truncated).then(|| last_load + IGNORE_RETRY)
}

/// What the loop is waiting for, as plain values so the wait is a function of
/// them and nothing else.
struct WaitPlan {
    now: Instant,
    pending: bool,
    last_event: Instant,
    first_pending: Option<Instant>,
    last_scan: Option<Instant>,
    dead_misses: u32,
    rules_retry_at: Option<Instant>,
}

/// How long the loop may sleep before something it owns is due.
///
/// The earliest of: the pending refresh becoming emittable (quiet for
/// [`DEBOUNCE_QUIET`] or [`DEBOUNCE_MAX_WAIT`] old, but not before
/// [`SCAN_COALESCE`] after the last scan, exactly [`scan_is_due`]'s rule), the
/// ignore-rules retry, a repository that was just missed being looked at
/// again, and [`IDLE_HEARTBEAT`]. An event or a stop ends the wait sooner by
/// arriving.
fn next_wait(plan: &WaitPlan) -> Duration {
    let mut wait = IDLE_HEARTBEAT;
    if plan.dead_misses > 0 {
        wait = wait.min(DEAD_RECHECK);
    }
    if plan.pending {
        let quiet_at = plan.last_event + DEBOUNCE_QUIET;
        let emit_at = plan
            .first_pending
            .map_or(quiet_at, |first| quiet_at.min(first + DEBOUNCE_MAX_WAIT));
        let due_at = plan
            .last_scan
            .map_or(emit_at, |scan| emit_at.max(scan + SCAN_COALESCE));
        wait = wait.min(due_at.saturating_duration_since(plan.now));
    }
    if let Some(at) = plan.rules_retry_at {
        wait = wait.min(at.saturating_duration_since(plan.now));
    }
    wait
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
    /// Times the loop's wait ran out with nothing to read. An idle watch
    /// should add none for as long as it is idle; a test reads this to say so.
    timer_wakes: Arc<std::sync::atomic::AtomicU64>,
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
    stop: Arc<StopSignal>,
    sessions: Option<std::sync::Arc<Mutex<HashMap<String, WatchSession>>>>,
    session_stop: Arc<StopSignal>,
    on_change: F,
) -> WatchLoopExit
where
    F: Fn(RepoChangedPayload),
{
    let WatchLoopContext {
        git_dir,
        internal_roots,
        emit_path: path,
        timer_wakes,
    } = ctx;
    let mut pending = false;
    let mut last_event = Instant::now();
    let mut first_pending: Option<Instant> = None;
    let mut last_scan: Option<Instant> = None;
    // What the pending window has touched; taken by each emission.
    let mut change = ChangeAccumulator::default();
    let emit = |change: &mut ChangeAccumulator| RepoChangedPayload {
        path: path.clone(),
        change: change.take(),
    };
    let filter = watcher.filter();
    // The rule set the callback filters with is the one this loop classifies
    // with: one load, published to both.
    let load_rules = || {
        let loaded = Arc::new(IgnoreRules::load(Path::new(&path)));
        filter.publish_rules(Arc::clone(&loaded));
        loaded
    };
    let mut rules = load_rules();
    let mut last_rules_load = Instant::now();
    // A rule file or a new directory was seen since the last load. Remembered
    // until the coalesce window allows the reload, so an edit made just after a
    // load is not lost.
    let mut reload_wanted = false;
    // When the watched git directory is deleted (repo moved/removed), notify
    // keeps delivering remove/error events forever, and the settle timer would
    // still fire one last `repo-changed` for the corpse. Liveness is therefore
    // checked at the exact point of emission: a dead path can never be
    // announced, and after the miss counter confirms it stays dead, the loop
    // exits and reaps its own session instead of hammering a dead path.
    const DEAD_MISSES: u32 = 3;
    let mut dead_misses: u32 = 0;
    let mut exit = WatchLoopExit::Stopped;
    'outer: while !stop.is_stopped() {
        if !git_dir.exists() {
            dead_misses += 1;
            if dead_misses >= DEAD_MISSES {
                exit = WatchLoopExit::DeadRepo;
                break;
            }
        } else {
            dead_misses = 0;
        }
        // Events the bounded queue dropped are unknowable: one full refresh,
        // like a backend `Rescan`.
        if watcher.take_overflow() {
            change.kind(ChangeKind::Unknown);
            if !pending {
                first_pending = Some(Instant::now());
            }
            pending = true;
            last_event = Instant::now();
        }
        let wait = next_wait(&WaitPlan {
            now: Instant::now(),
            pending,
            last_event,
            first_pending,
            last_scan,
            dead_misses,
            rules_retry_at: [
                rules_retry_at(&rules, last_rules_load),
                reload_wanted.then(|| last_rules_load + SCAN_COALESCE),
            ]
            .into_iter()
            .flatten()
            .min(),
        });
        match watcher.receiver.recv_timeout(wait) {
            // A stop wakes the loop with a path-less event; it is not a change.
            Ok(_) if stop.is_stopped() => break,
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
                reload_wanted |=
                    event_may_change_ignore_rules(&event, &internal_roots, filter.recursive());
                let mut significant = record_event_signal(
                    &event,
                    &internal_roots,
                    worktree,
                    worktree_canonical.as_deref(),
                    &rules,
                    &mut change,
                );
                for leftover in watcher.receiver.try_iter() {
                    significant |= match leftover {
                        Ok(event) => {
                            reload_wanted |= event_may_change_ignore_rules(
                                &event,
                                &internal_roots,
                                filter.recursive(),
                            );
                            record_event_signal(
                                &event,
                                &internal_roots,
                                worktree,
                                worktree_canonical.as_deref(),
                                &rules,
                                &mut change,
                            )
                        }
                        Err(_) => {
                            change.kind(ChangeKind::Unknown);
                            true
                        }
                    };
                }
                if rules_due(&rules, last_rules_load, Instant::now(), reload_wanted) {
                    rules = load_rules();
                    last_rules_load = Instant::now();
                    reload_wanted = false;
                }
                // Noise never opens or extends the pending window, but it
                // must not stop one already open from being served: the
                // emission check below runs for every event, or 5 Hz
                // `index.lock` / fsmonitor churn postponed a real change
                // forever (the receive never times out, so the timeout arm's
                // check never ran).
                if significant {
                    if !pending {
                        first_pending = Some(Instant::now());
                    }
                    pending = true;
                    last_event = Instant::now();
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
                    on_change(emit(&mut change));
                    pending = false;
                    first_pending = None;
                    last_scan = Some(Instant::now());
                }
            }
            // A notify backend error carries no classifiable path; per the
            // fail-open rule above it counts as signal — open or extend the
            // pending window so the missed-events repo still refreshes.
            Ok(Err(_)) => {
                change.kind(ChangeKind::Unknown);
                if !pending {
                    first_pending = Some(Instant::now());
                }
                pending = true;
                last_event = Instant::now();
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                timer_wakes.fetch_add(1, Ordering::Relaxed);
                // The channel's own sender stays open here (the stop signal
                // wakes through it), so a backend that died cannot announce
                // it by disconnecting: its callback going away is the sign.
                if !watcher.backend_alive() {
                    exit = WatchLoopExit::EventStreamClosed;
                    break;
                }
                if rules_due(&rules, last_rules_load, Instant::now(), reload_wanted) {
                    rules = load_rules();
                    last_rules_load = Instant::now();
                    reload_wanted = false;
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
                    on_change(emit(&mut change));
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

/// Emitted once when a watched repository's git directory is confirmed gone
/// (the checkout was deleted or moved). The watch has already ended; nothing
/// further arrives for `path` until it is watched again.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RepoGonePayload {
    pub path: String,
}

/// Debounced git-directory watcher that emits `repo-changed` after writes
/// settle, and `repo-gone` once if the repository disappears.
///
/// Concurrent watches are keyed by canonical repository path. Re-watching an
/// existing path is idempotent. Returns the canonical path used as the map key.
pub fn start_watch(
    app: AppHandle,
    state: &WatcherState,
    repo_path: String,
) -> Result<String, CommandError> {
    let gone_app = app.clone();
    start_watch_observed(
        state,
        repo_path,
        move |payload| {
            if let Err(e) = app.emit("repo-changed", &payload) {
                log::warn!(target: "watcher", "repo-changed emit failed for {}: {e}", payload.path);
            }
        },
        move |payload| {
            if let Err(e) = gone_app.emit("repo-gone", &payload) {
                log::warn!(target: "watcher", "repo-gone emit failed for {}: {e}", payload.path);
            }
        },
    )
}

/// [`start_watch_observed`] for a caller that does not need `repo-gone`.
#[cfg(test)]
pub(crate) fn start_watch_inner<F>(
    state: &WatcherState,
    repo_path: String,
    on_change: F,
) -> Result<String, CommandError>
where
    F: Fn(RepoChangedPayload) + Send + 'static,
{
    start_watch_observed(state, repo_path, on_change, |_| {})
}

/// [`start_watch_inner`] plus `on_gone`, called once, after the session is
/// reaped, when the loop ends because the repository disappeared. Without it
/// the UI never learned: a dead path is deliberately never announced as a
/// change, so a tab on a deleted worktree stayed open, held a tab slot, and
/// was re-reported by every later workspace sync.
pub(crate) fn start_watch_observed<F, G>(
    state: &WatcherState,
    repo_path: String,
    on_change: F,
    on_gone: G,
) -> Result<String, CommandError>
where
    F: Fn(RepoChangedPayload) + Send + 'static,
    G: FnOnce(RepoGonePayload) + Send + 'static,
{
    #[cfg(test)]
    eprintln!("watch setup {repo_path}: validate repository");
    let canonical = validate_repo(&repo_path)?;
    let key = canonical.to_string_lossy().into_owned();
    #[cfg(test)]
    eprintln!("watch setup {repo_path}: resolve git directory");
    let git_dir = resolve_git_dir(&canonical)?;
    // Bare repos have no separate worktree root (git dir == repo); a normal
    // checkout and a linked worktree both do. The worktree watch is what makes
    // unstaged edits fire `repo-changed` (recursive on macOS, top-level
    // entries elsewhere; see `RepoFileWatcher::watch_repo`).
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
            return Err(format!("Too many watched repositories (max {MAX_WATCHES})").into());
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
                "linked-worktree ref watching degraded for {key}: {e}", e = e.message()
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
            Some(stop) => {
                if let Some(session) = guard.get_mut(&key) {
                    session.filter = Some(watcher.filter());
                }
                stop
            }
        }
    };
    // Before the loop exists: a stop that lands in between is replayed here.
    stop.arm(watcher.waker());

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
    if let Err(e) = crate::supervisor::spawn_once("gitpulse-fs-watch", "file watcher", move || {
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
                    timer_wakes: Default::default(),
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
                // One announcement, not a change: nothing may refresh a
                // path that no longer exists.
                on_gone(RepoGonePayload {
                    path: emit_path.clone(),
                });
            }
            Ok(WatchLoopExit::EventStreamClosed) => {
                log::warn!(
                    target: "watcher",
                    "watch event stream closed unexpectedly for {emit_path}; \
                     UI refresh for this repo is dead until it is re-watched"
                );
            }
            Err(panic) => {
                crate::supervisor::note_stopped("file watcher", &*panic);
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
    }) {
        let _ = state.abandon_watch_slot(&key, &stop);
        return Err(format!("Failed to start watcher: {}", e).into());
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

/// The status of `repo_path`: `read`'s answer, or, when `may_reuse` and the
/// watch saw no event of any kind since the read that produced it began, that
/// earlier answer with no git spawned. See [`skip`] for the rule and for what a
/// watch must do to keep earning it.
///
/// A read is always kept when the watch can vouch, whoever asked, so a refresh
/// that followed an event also serves the poll that comes after it; only a
/// caller that says `may_reuse` is ever served one.
pub(crate) fn status_through_watch<F>(
    state: &WatcherState,
    repo_path: &str,
    may_reuse: bool,
    read: F,
) -> Result<Vec<FileStatus>, CommandError>
where
    F: FnOnce() -> Result<Vec<FileStatus>, CommandError>,
{
    let filter = validate_repo(repo_path)
        .ok()
        .and_then(|repo| state.filter_for(&repo.to_string_lossy()));
    let Some(filter) = filter else {
        return read();
    };
    let skip = filter.skip();
    let started = Instant::now();
    let begun = skip.begin(started, may_reuse);
    if let Some(rows) = begun.served {
        return Ok(rows.as_ref().clone());
    }
    let rows = read()?;
    if let Some(epoch) = begun.keep_under {
        skip.settle(epoch, &rows, started);
    }
    Ok(rows)
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
        session.stop.stop();
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
    use std::thread;
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
            change: RepoChange {
                kinds: vec![ChangeKind::Refs, ChangeKind::GitState, ChangeKind::Worktree],
                paths: vec!["docs/a.md".into()],
                paths_truncated: false,
            },
        };
        let value = serde_json::to_value(&payload).expect("serialize");
        assert_eq!(
            value,
            serde_json::json!({
                "path": "/tmp/example-repo",
                "change": {
                    "kinds": ["refs", "git_state", "worktree"],
                    "paths": ["docs/a.md"],
                    "paths_truncated": false,
                },
            })
        );
    }

    #[test]
    fn git_paths_are_classified_by_what_they_move() {
        let git = PathBuf::from("/repo/.git");
        let own = std::slice::from_ref(&git);
        for (relative, kind) in [
            ("HEAD", ChangeKind::Refs),
            ("refs/heads/main", ChangeKind::Refs),
            ("packed-refs", ChangeKind::Refs),
            ("logs/HEAD", ChangeKind::Refs),
            ("index", ChangeKind::Index),
            ("config", ChangeKind::Config),
            ("info/exclude", ChangeKind::Ignore),
            ("objects/ab/cdef", ChangeKind::Objects),
            ("MERGE_HEAD", ChangeKind::GitState),
            ("rebase-merge/done", ChangeKind::GitState),
            // A linked worktree's private directory, seen from the main one.
            ("worktrees/feature/HEAD", ChangeKind::GitState),
            ("worktrees/feature/index", ChangeKind::GitState),
        ] {
            assert_eq!(git_path_kind(&git.join(relative), own), kind, "{relative}");
        }

        // A linked worktree: its own private dir first, the common dir second.
        let private = git.join("worktrees/feature");
        let linked = [private.clone(), git.clone()];
        for (path, kind) in [
            (private.join("index"), ChangeKind::Index),
            (private.join("HEAD"), ChangeKind::Refs),
            (git.join("refs/heads/feature"), ChangeKind::Refs),
            (git.join("objects/ab/cdef"), ChangeKind::Objects),
            // The main checkout's index, and a sibling's private state, move
            // this repository's worktree list, never this checkout's files.
            (git.join("index"), ChangeKind::GitState),
            (git.join("worktrees/other/index"), ChangeKind::GitState),
            (git.join("worktrees/other/HEAD"), ChangeKind::GitState),
        ] {
            assert_eq!(git_path_kind(&path, &linked), kind, "{}", path.display());
        }
    }

    /// The classes come from the paths the noise gate admitted and from no
    /// others. A noise-only event — the fsmonitor cookie and split-index
    /// touches every `git status` makes — must record nothing, or a refresh
    /// would announce itself again.
    #[test]
    fn only_admitted_paths_are_recorded() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let git = root.join(".git");
        std::fs::create_dir_all(&git).unwrap();
        let roots = vec![git.clone()];
        let rules = IgnoreRules::load(&root);
        let event = |paths: Vec<PathBuf>| {
            let mut event = Event::new(notify::EventKind::Any);
            event.paths = paths;
            event
        };

        let mut change = ChangeAccumulator::default();
        let noise = event(vec![
            git.join("index.lock"),
            git.join("fsmonitor--daemon/cookies/1234-0"),
            git.join("sharedindex.0123456789abcdef"),
            root.join(".devcouncil/codeintel/devmap.sqlite"),
            root.clone(),
        ]);
        assert!(!record_event_signal(
            &noise,
            &roots,
            &root,
            Some(&root),
            &rules,
            &mut change
        ));
        assert_eq!(change.take(), RepoChange::default());

        let mixed = event(vec![
            git.join("index.lock"),
            git.join("refs/heads/main"),
            root.join("README.md"),
        ]);
        assert!(record_event_signal(
            &mixed,
            &roots,
            &root,
            Some(&root),
            &rules,
            &mut change
        ));
        assert_eq!(
            change.take(),
            RepoChange {
                kinds: vec![
                    ChangeKind::Refs,
                    ChangeKind::Worktree,
                    ChangeKind::Documents
                ],
                paths: vec!["README.md".into()],
                paths_truncated: false,
            }
        );

        // Only an entry that is, or may hold, a document says so.
        std::fs::write(root.join("main.rs"), "fn main() {}\n").unwrap();
        std::fs::create_dir(root.join("guides")).unwrap();
        for (entry, documents) in [
            ("main.rs", false),
            ("NOTES.MDX", true),
            ("guides", true),
            // Removed before the event was read: no extension, may have
            // been a directory of notes.
            ("gone", true),
            ("gone.rs", false),
        ] {
            let source = event(vec![root.join(entry)]);
            assert!(record_event_signal(
                &source,
                &roots,
                &root,
                Some(&root),
                &rules,
                &mut change
            ));
            assert_eq!(
                change.take().kinds.contains(&ChangeKind::Documents),
                documents,
                "{entry}"
            );
        }

        // No path: the backend cannot say, so every view must refresh.
        assert!(record_event_signal(
            &event(vec![]),
            &roots,
            &root,
            Some(&root),
            &rules,
            &mut change
        ));
        assert_eq!(change.take().kinds, vec![ChangeKind::Unknown]);
    }

    #[test]
    fn changed_paths_are_bounded_and_say_so() {
        let mut change = ChangeAccumulator::default();
        for i in 0..MAX_CHANGED_PATHS + 6 {
            change.worktree(Path::new(&format!("file-{i:03}")));
        }
        // A repeat of a kept path is not an overflow.
        change.worktree(Path::new("file-000"));
        let taken = change.take();
        assert_eq!(taken.paths.len(), MAX_CHANGED_PATHS);
        assert!(taken.paths_truncated);
        assert_eq!(taken.kinds, vec![ChangeKind::Worktree]);
        // Taking resets the window.
        assert_eq!(change.take(), RepoChange::default());
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
            err.contains("24") || err.message().to_lowercase().contains("too many"),
            "cap error should mention the limit, got: {err}",
            err = err.message()
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
                timer_wakes: Default::default(),
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
                timer_wakes: Default::default(),
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
        assert!(!replacement.is_stopped());
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
            while !(one.is_stopped() && two.is_stopped()) {
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
    fn spawn_loop(
        dir: &Path,
    ) -> (
        std::sync::mpsc::Receiver<RepoChangedPayload>,
        Arc<StopSignal>,
        PathBuf,
    ) {
        let (rx, stop, root, _) = spawn_loop_observed(dir);
        (rx, stop, root)
    }

    /// [`spawn_loop`] that also hands back the callback filter, for a test that
    /// counts what it classified and how often the rules were reloaded.
    fn spawn_loop_observed(
        dir: &Path,
    ) -> (
        std::sync::mpsc::Receiver<RepoChangedPayload>,
        Arc<StopSignal>,
        PathBuf,
        Arc<filter::EventFilter>,
    ) {
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

        let stop = StopSignal::new();
        stop.arm(watcher.waker());
        let filter = watcher.filter();
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
                        timer_wakes: Default::default(),
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
        (rx, stop, canonical, filter)
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
    /// pipeline delivers a callback that NAMES one of them, then quiesces via
    /// [`await_watcher_quiescence`] so the caller starts from an empty,
    /// genuinely idle channel.
    ///
    /// Why a retry loop instead of one write plus one long recv_timeout:
    /// FSEvents gives no delivery guarantee for writes racing watch-stream
    /// installation (each `watch()` call recreates the stream with a fresh
    /// SinceNow epoch), so a single priming write can be silently lost and
    /// flake the test. Retrying fresh probe names until delivery — the cure
    /// already proven in `dead_repo_watch_stops_emitting_and_reaps_session` —
    /// makes priming deterministic under load.
    ///
    /// Why the callback must name a probe rather than be any callback: the
    /// caller's own setup (commit, stash, file writes) is announced too, and
    /// when FSEvents delivers late that announcement is the first callback.
    /// Treating it as proof of priming let the 2.1 s silence pass while the
    /// probe, and anything queued behind the setup, was still in flight, and
    /// those events then landed inside the caller's asserted-quiet window.
    ///
    /// Ordering, stated by evidence. Verified in the source: the backend
    /// walks each FSEvents batch in array order and ignores the event ids
    /// (`framework/notify/src/fsevent.rs` `callback_impl`), `forward_event`
    /// feeds a FIFO channel, and one loop thread debounces it. ASSUMED, not
    /// established here: that fseventsd delivers batches in event-id order
    /// across batches and across the `.git` and worktree-root paths of the one
    /// stream. Excluded: a full queue drops events and the
    /// loop answers with a later full refresh, so under overflow an earlier
    /// write can surface after the probe. Under those assumptions a callback
    /// naming a probe means every earlier write was already delivered; even
    /// if they fail, waiting for a named probe is strictly tighter than
    /// accepting any callback. A probe pushed out of `paths` by
    /// `paths_truncated` (more than [`MAX_CHANGED_PATHS`] root paths in one
    /// window) makes priming time out loudly, never pass silently.
    ///
    /// Once any probe is announced the stream is known live, so writing stops
    /// and the wait is for the LAST probe written: probes written while
    /// delivery lagged are still in flight behind the first announced one.
    ///
    /// Panics, naming the attempt count and every path it did see, only if the
    /// last probe is never announced within `deadline`.
    fn prime_watcher(
        rx: &std::sync::mpsc::Receiver<RepoChangedPayload>,
        probe_dir: &Path,
        deadline: Duration,
    ) {
        prime_watcher_with(rx, deadline, |name| {
            std::fs::write(probe_dir.join(name), "x").expect("write priming probe");
        });
    }

    /// [`prime_watcher`] with the probe write injected, so the keyed wait can
    /// be driven by a plain channel in a test.
    fn prime_watcher_with(
        rx: &std::sync::mpsc::Receiver<RepoChangedPayload>,
        deadline: Duration,
        write_probe: impl FnMut(&str),
    ) {
        write_until_announced(rx, deadline, "watcher-prime", write_probe, names_probe);
        await_watcher_quiescence(rx);
    }

    /// The usual announcement test: the payload lists the probe file.
    fn names_probe(payload: &RepoChangedPayload, probe: &str) -> bool {
        payload.change.paths.iter().any(|p| p == probe)
    }

    /// The one owner of "write, then wait for the pipeline to announce it".
    /// Writes `{stem}-{n}.txt` probes through `write_probe` until
    /// `announces(payload, probe)` holds for one, then stops writing and waits
    /// for the announcement of the LAST probe written. Every other callback
    /// (the caller's setup, earlier probes) is consumed rather than mistaken
    /// for the answer. Returns the payload that announced the last probe.
    ///
    /// Bounded by `deadline` and the per-probe window the loops always used;
    /// panics naming the probe count and every path seen on timeout or if the
    /// loop hangs up. A loop whose next step asserts silence or inspects the
    /// payload must come through here; a pure liveness probe, where any
    /// callback is a correct answer, need not.
    fn write_until_announced(
        rx: &std::sync::mpsc::Receiver<RepoChangedPayload>,
        deadline: Duration,
        stem: &str,
        mut write_probe: impl FnMut(&str),
        announces: impl Fn(&RepoChangedPayload, &str) -> bool,
    ) -> RepoChangedPayload {
        let overall = Instant::now() + deadline;
        let mut probes: Vec<String> = Vec::new();
        let mut seen = std::collections::BTreeSet::<String>::new();
        // Once any probe is announced the stream is proven live: stop writing
        // and wait for the LAST probe written. Probes written during a slow
        // delivery are still in flight behind the first announced one, and
        // would otherwise land after the caller's silence window.
        //
        // The exception is a silent pipeline: if nothing at all arrives for
        // two probe windows and the last probe is still unnamed, that probe
        // was lost, so a fresh one is written and keyed on instead of waiting
        // out the deadline. Two windows, not one: probes written one window
        // apart are delivered one window apart, so a single window cannot tell
        // a lost probe from a spaced one, and re-probing there would feed the
        // stream it is waiting on.
        let window = DEBOUNCE_QUIET + Duration::from_millis(200);
        let mut live = false;
        loop {
            let name = format!("{stem}-{}.txt", probes.len());
            write_probe(&name);
            probes.push(name);
            let silence = |live: bool| if live { window * 2 } else { window };
            let mut slice_ends = (Instant::now() + silence(live)).min(overall);
            while let Some(left) = slice_ends.checked_duration_since(Instant::now()) {
                match rx.recv_timeout(left) {
                    Ok(payload) => {
                        if probes.last().is_some_and(|last| announces(&payload, last)) {
                            return payload;
                        }
                        live |= probes.iter().any(|probe| announces(&payload, probe));
                        seen.extend(payload.change.paths);
                        if live {
                            // Payloads still flowing: not silent yet.
                            slice_ends = (Instant::now() + silence(live)).min(overall);
                        }
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => break,
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => panic!(
                        "watch loop hung up waiting for a `{stem}` probe after {} writes; \
                         saw paths {seen:?}",
                        probes.len()
                    ),
                }
            }
            assert!(
                Instant::now() < overall,
                "watcher never announced a `{stem}` probe after {} probe writes within \
                 {deadline:?}; the OS watch stream likely never installed or delivery is \
                 stalled. Paths announced meanwhile: {seen:?}",
                probes.len()
            );
        }
    }

    fn announced(paths: &[&str]) -> RepoChangedPayload {
        RepoChangedPayload {
            path: "repo".into(),
            change: RepoChange {
                kinds: vec![ChangeKind::Worktree],
                paths: paths.iter().map(|p| (*p).to_owned()).collect(),
                paths_truncated: false,
            },
        }
    }

    /// Deterministic, independent of fseventsd: the caller's own setup is
    /// announced first and the probe only later (a late-delivering FSEvents).
    /// Priming must not return on the setup callback — the probe, still in
    /// flight past the 2.1 s silence, would otherwise land in the caller's
    /// asserted-quiet window.
    #[test]
    fn prime_watcher_waits_for_its_own_probe_not_the_first_callback() {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut lagged = Vec::new();
        prime_watcher_with(&rx, PRIME_DEADLINE, |name| {
            if name == "watcher-prime-0.txt" {
                tx.send(announced(&["staged.txt", "tracked.txt"])).unwrap();
            }
            // Every write is announced in order, but only after a lag past
            // the old helper's DEBOUNCE_MAX_WAIT + 100 ms silence and inside
            // the deadline: a slow fseventsd.
            let tx = tx.clone();
            let name = name.to_owned();
            lagged.push(thread::spawn(move || {
                thread::sleep(DEBOUNCE_MAX_WAIT + Duration::from_millis(900));
                tx.send(announced(&[name.as_str()])).unwrap();
            }));
        });
        // Primed means every probe written was consumed: nothing in flight.
        let straggler = rx.recv_timeout(Duration::from_millis(1500));
        for handle in lagged {
            handle.join().unwrap();
        }
        assert!(
            straggler.is_err(),
            "priming returned before its own probe was delivered: {straggler:?}"
        );
    }

    /// The edit loop of `worktree_file_event_triggers_and_git_only_events_still_do`
    /// inspects the payload it gets, so a stray earlier callback (a late
    /// priming probe) must not be handed back as the answer, and no edit may
    /// still be in flight once it returns.
    #[test]
    fn the_edit_wait_returns_the_payload_of_its_last_edit_not_a_stray_callback() {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut written: Vec<String> = Vec::new();
        let mut lagged = Vec::new();
        let payload = write_until_announced(
            &rx,
            PRIME_DEADLINE,
            "unstaged-edit",
            |name| {
                if written.is_empty() {
                    tx.send(announced(&["watcher-prime-3.txt"])).unwrap();
                }
                written.push(name.to_owned());
                let tx = tx.clone();
                let name = name.to_owned();
                // Every edit is announced in order, a lag after the stray.
                lagged.push(thread::spawn(move || {
                    thread::sleep(Duration::from_millis(900));
                    tx.send(announced(&[name.as_str()])).unwrap();
                }));
            },
            names_probe,
        );
        let last = written.last().expect("at least one edit").clone();
        let straggler = rx.recv_timeout(Duration::from_millis(1000));
        for handle in lagged {
            handle.join().unwrap();
        }
        assert_eq!(
            payload.change.paths,
            vec![last],
            "the stray callback was taken for the edit"
        );
        assert!(
            straggler.is_err(),
            "an edit was still in flight: {straggler:?}"
        );
    }

    /// The stream is proven live (probe 0 is announced) but the last probe
    /// written before that, probe 1, is lost. Waiting out the deadline for it
    /// would fail a test whose subject is something else; the wait must go
    /// silent for two windows and then probe again.
    #[test]
    fn a_lost_last_probe_is_replaced_once_the_pipeline_goes_silent() {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut written: Vec<String> = Vec::new();
        let mut lagged = Vec::new();
        let payload = write_until_announced(
            &rx,
            Duration::from_secs(5),
            "watcher-prime",
            |name| {
                let n = written.len();
                written.push(name.to_owned());
                // Probe 0 arrives late enough that probe 1 is already
                // written; probe 1 is lost; later probes arrive promptly.
                let lag = match n {
                    0 => Some(Duration::from_millis(900)),
                    1 => None,
                    _ => Some(Duration::from_millis(100)),
                };
                if let Some(lag) = lag {
                    let tx = tx.clone();
                    let name = name.to_owned();
                    lagged.push(thread::spawn(move || {
                        thread::sleep(lag);
                        tx.send(announced(&[name.as_str()])).unwrap();
                    }));
                }
            },
            names_probe,
        );
        for handle in lagged {
            handle.join().unwrap();
        }
        assert_eq!(written.len(), 3, "{written:?}");
        assert_eq!(payload.change.paths, vec![written[2].clone()]);
    }

    /// A stalled backend still fails loud, naming the attempts and the paths
    /// it did see, rather than passing on someone else's callback.
    #[test]
    #[should_panic(expected = "Paths announced meanwhile: {\"staged.txt\"}")]
    fn prime_watcher_fails_loud_when_only_foreign_paths_arrive() {
        let (tx, rx) = std::sync::mpsc::channel();
        prime_watcher_with(&rx, Duration::from_millis(1200), |name| {
            if name == "watcher-prime-0.txt" {
                tx.send(announced(&["staged.txt"])).unwrap();
            }
        });
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
            assert!(!first.path.is_empty());

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
        stop.stop();
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
        //    freshly-proven-hot stream was once lost for >15s. The wait is
        //    keyed on the last edit written, so a late callback cannot stand
        //    in for it.
        let payload = write_until_announced(
            &rx,
            PRIME_DEADLINE,
            "unstaged-edit",
            |name| std::fs::write(root.join(name), "dirty\n").unwrap(),
            names_probe,
        );
        // The emission names what it touched, not only that something did.
        assert!(
            payload.change.kinds.contains(&ChangeKind::Worktree),
            "{payload:?}"
        );
        assert!(
            payload
                .change
                .paths
                .iter()
                .any(|p| p.starts_with("unstaged-edit-")),
            "{payload:?}"
        );

        // Close the quiet window so the next write is a distinct emission
        // rather than coalesced with a late event from the first write.
        thread::sleep(DEBOUNCE_QUIET + Duration::from_millis(100));

        // 2. A .git-only write still triggers through the recursive watch.
        //    Retry-bounded for the same reason as above; each attempt uses a
        //    fresh name inside `.git` so no state carries between attempts.
        //    `.git` writes are not listed in `paths`, so a probe cannot be
        //    named: the predicate ignores it and ANY GitState callback
        //    satisfies it, including a stray one from earlier setup. The
        //    last-probe wait therefore adds nothing here; that is acceptable
        //    only because nothing a stray callback could corrupt is asserted
        //    afterwards.
        let payload = write_until_announced(
            &rx,
            PRIME_DEADLINE,
            "gitpulse-probe",
            |name| std::fs::write(root.join(".git").join(name), "x").unwrap(),
            |payload, _| payload.change.kinds.contains(&ChangeKind::GitState),
        );
        assert!(
            payload.change.kinds.contains(&ChangeKind::GitState),
            "{payload:?}"
        );

        stop.stop();
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
    /// The event queue does not receive nested `target/` writes on any
    /// platform (see [`measure_target_directory_event_rate`]): the root watch
    /// is non-recursive on Linux and Windows, and on macOS the callback drops
    /// them. Paths still have to be classified here: FSEvents can report the
    /// `target` directory itself, a `target-*` sibling, or a nested path that
    /// leaked through before the rules were loaded. Before this filter every
    /// one of those was signal, and each signal opens a refresh that fans out
    /// into many `git` children.
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
            started.elapsed() < crate::test_support::coverage_relaxed(Duration::from_secs(2)),
            "40k classifications took {:?}",
            started.elapsed()
        );
    }

    /// A list cut at the prefix cap is a finished, partial answer. Re-running
    /// `git ls-files -o -i --directory` every 30 s forever returned the same
    /// cut list at the cost of a full untracked-tree walk.
    #[test]
    fn a_truncated_ignore_list_is_final_not_retried() {
        let mut rules = IgnoreRules::builtin();
        rules.load_error = Some("truncated".into());
        rules.truncated = true;
        let now = Instant::now();
        assert!(!rules_due(&rules, now, now + IGNORE_RETRY * 10, false));
        // A real `.gitignore` edit still reloads it.
        assert!(rules_due(&rules, now, now + SCAN_COALESCE, true));
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
        // FSEvents reports canonical paths (`/private/var`); TempDir hands out
        // `/var`. Either spelling of `target` itself is the directory entry.
        let reported_target = target.canonicalize().unwrap();
        let mut events = 0u32;
        let mut target_events = 0u32;
        let mut sentinel_events = 0u32;
        let mut sample = String::new();
        // Read until the sentinel arrives (a fixed 800 ms window read nothing
        // on a loaded host), then whatever is already queued behind it.
        let deadline = Instant::now() + PRIME_DEADLINE;
        loop {
            let item = if sentinel_events == 0 {
                let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                    break;
                };
                match watcher.receiver.recv_timeout(left) {
                    Ok(item) => item,
                    Err(_) => break,
                }
            } else {
                match watcher.receiver.try_recv() {
                    Ok(item) => item,
                    Err(_) => break,
                }
            };
            events += 1;
            if let Ok(event) = item {
                // Below `target/`, not `target` itself: Windows reports the
                // directory entry as modified (its last-write time) for every
                // file created inside it, which is no nested write reaching the
                // watch. That event is shown below to carry no signal.
                let names_target = event.paths.iter().any(|path| {
                    path != &target
                        && path != &reported_target
                        && path
                            .components()
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
            "nested target/ writes must not reach the event queue on any platform"
        );
        assert!(
            sentinel_events >= 1,
            "the watch must still deliver a file at the worktree root, else the zero above is a dead stream"
        );
        // What the platform may deliver for `target/` itself changes nothing.
        let directory_touched = notify::Event::new(notify::EventKind::Any).add_path(target);
        assert!(
            !event_has_signal(
                &directory_touched,
                &internal_roots_for(dir.path()),
                dir.path(),
                None,
                &IgnoreRules::builtin()
            ),
            "a modified `target` directory entry must not count as signal"
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
        stop.stop();
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
        let (gone_tx, gone_rx) = std::sync::mpsc::channel();
        let key = start_watch_observed(
            &state,
            dir.path().to_string_lossy().into_owned(),
            move |payload| {
                let _ = tx.send(payload);
            },
            move |payload| {
                let _ = gone_tx.send(payload);
            },
        )
        .expect("watch live repo");
        assert!(state.is_watching(&key).unwrap());

        // Keep poking the worktree root until the OS backend delivers: a
        // single write races watch installation, especially when other tests
        // in this process also hold FSEvents/inotify watches. Primed through
        // the shared keyed wait: the silence asserted below must not be
        // broken by a probe still in flight.
        assert!(
            dir.path().join(".git").exists(),
            "temp repo vanished before the watcher primed"
        );
        prime_watcher(&rx, dir.path(), PRIME_DEADLINE);

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
        // Silent toward `repo-changed`, but the UI must still learn the tab
        // is dead: exactly one `repo-gone`, naming the watched key.
        let announced = gone_rx
            .recv_timeout(Duration::from_secs(6))
            .expect("a dead repository must be announced as gone");
        assert_eq!(announced, RepoGonePayload { path: key.clone() });
        assert!(
            gone_rx.recv_timeout(Duration::from_millis(500)).is_err(),
            "repo-gone must fire once"
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
            move |payload| {
                let _ = tx.send(payload);
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
        // files land at the watched worktree root. A single
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
        // Another checkout's index is that checkout's: `git add` there must
        // not refresh this one (see `is_other_checkout_state`). The previous
        // expectation listed it as moving the repository.
        assert!(
            is_git_internal_noise(&linked.join("index"), &roots),
            "a sibling worktree's index is not this checkout's change"
        );
        for path in [
            linked.join("HEAD"),
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
            stop.stop();
        }
    }

    /// The loop itself, end to end: GitPulse's own read under fsmonitor must
    /// not announce a change. Proven live afterwards, so a dead watcher cannot
    /// pass this by staying silent.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_status_read_under_fsmonitor_does_not_announce_itself() {
        /// One git call under fsmonitor. A healthy call takes seconds; under
        /// a loaded host (load average in the dozens) tens of seconds. A hung
        /// one never returns: sampled live, the client sat in `read()` inside
        /// `fsmonitor_ipc__send_query` while the daemon's `handle_client`
        /// waited on a condition variable no event ever signalled, with
        /// nothing on the client side to time it out.
        const FSMONITOR_GIT_BUDGET: Duration = Duration::from_secs(60);
        use std::process::Command;
        // Every git below can wait on the fsmonitor daemon, a separate
        // process whose lifetime the test does not own: each call has a
        // deadline, and a call that misses it fails the test with its name.
        let git = |root: &Path, args: &[&str], no_optional_locks: bool| {
            let mut cmd = Command::new("git");
            cmd.args(args).current_dir(root);
            if no_optional_locks {
                cmd.env("GIT_OPTIONAL_LOCKS", "0");
            }
            crate::test_support::run_within(
                cmd,
                &format!("git {}", args.join(" ")),
                crate::test_support::coverage_relaxed(FSMONITOR_GIT_BUDGET),
            )
        };
        struct StopDaemon(PathBuf);
        impl Drop for StopDaemon {
            fn drop(&mut self) {
                // Never panics: this also runs while the test's own failure
                // unwinds, and a stop that hangs must not outlive its bound.
                let mut cmd = Command::new("git");
                cmd.args(["fsmonitor--daemon", "stop"]).current_dir(&self.0);
                let _ = crate::engine::git_cli::run_bounded(
                    cmd,
                    "git fsmonitor--daemon stop",
                    // Held to the runner's ceiling: relaxed past it, the stop is refused unrun.
                    crate::test_support::coverage_relaxed(FSMONITOR_GIT_BUDGET)
                        .min(crate::engine::git_cli::NETWORK_TIMEOUT),
                    None,
                );
            }
        }
        let status = |root: &Path| {
            let out = git(root, &["status", "--porcelain=v2"], true);
            assert!(out.success, "{}", String::from_utf8_lossy(&out.stderr));
        };
        let temp = TempDir::new().unwrap();
        let (rx, stop, root) = spawn_loop(temp.path());
        // The bounded runner holds git to the same repository-trust gate the
        // product's own reads pass.
        crate::test_support::trust_repo(&root);
        let _daemon = StopDaemon(root.clone());
        let enabled = git(&root, &["config", "core.fsmonitor", "true"], false);
        assert!(enabled.success);
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
        stop.stop();
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
            elapsed < crate::test_support::coverage_relaxed(Duration::from_millis(750)),
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
        struct StopOnDrop(Arc<StopSignal>);
        impl Drop for StopOnDrop {
            fn drop(&mut self) {
                self.0.stop();
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
        stop.stop();
    }

    /// A worktree teardown moves the lane into `<git dir>/gitpulse-teardown/`
    /// and deletes it there: tens of thousands of unlinks under a directory
    /// the watcher watches recursively. None of it is a change to the
    /// repository, so an aside made and deleted there emits nothing.
    #[test]
    fn watch_loop_teardown_aside_churn_emits_nothing() {
        let temp = TempDir::new().unwrap();
        let (rx, stop, root) = spawn_loop(temp.path());
        let git_dir = root.join(".git");
        prime_watcher(&rx, &root, PRIME_DEADLINE);
        await_watcher_quiescence(&rx);

        let aside = git_dir.join("gitpulse-teardown").join("1-2-3");
        let mut leaked = None;
        for round in 0..20 {
            std::fs::create_dir_all(aside.join("build/obj")).unwrap();
            for n in 0..50 {
                std::fs::write(aside.join(format!("build/obj/f{n}")), format!("{round}")).unwrap();
            }
            std::fs::remove_dir_all(&aside).unwrap();
            if let Ok(path) = rx.recv_timeout(Duration::from_millis(50)) {
                leaked = Some(path);
                break;
            }
        }
        if leaked.is_none() {
            // Longer than the debounce's anti-starvation bound: an event that
            // was accepted would be forced out by now.
            leaked = rx.recv_timeout(Duration::from_secs(3)).ok();
        }
        assert!(
            leaked.is_none(),
            "a teardown aside under the git directory must not trigger repo-changed (got {leaked:?})"
        );
        stop.stop();
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
            assert!(!first.path.is_empty());

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
        stop.stop();
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
        stop.stop();
    }

    // ---- audit: access events, rescans, noise starvation, sibling worktrees ----

    fn modify(path: impl Into<PathBuf>) -> notify::Event {
        notify::Event::new(notify::EventKind::Modify(notify::event::ModifyKind::Data(
            notify::event::DataChange::Content,
        )))
        .add_path(path.into())
    }

    #[test]
    fn access_events_are_not_changes() {
        let dir = TempDir::new().unwrap();
        let rules = IgnoreRules::builtin();
        for kind in [
            notify::event::AccessKind::Open(notify::event::AccessMode::Any),
            notify::event::AccessKind::Close(notify::event::AccessMode::Read),
            notify::event::AccessKind::Close(notify::event::AccessMode::Write),
        ] {
            let read = notify::Event::new(notify::EventKind::Access(kind))
                .add_path(dir.path().join("src").join("main.rs"));
            assert!(
                !event_has_signal(&read, &[], dir.path(), None, &rules),
                "{kind:?}: the app's own `git status` reads must not refresh"
            );
        }
        assert!(event_has_signal(
            &modify(dir.path().join("src").join("main.rs")),
            &[],
            dir.path(),
            None,
            &rules
        ));
    }

    #[test]
    fn a_rescan_event_is_a_full_refresh_even_when_it_names_the_git_dir() {
        let dir = TempDir::new().unwrap();
        let git_dir = dir.path().join(".git");
        let roots = vec![git_dir.clone()];
        let rules = IgnoreRules::builtin();
        for named in [git_dir.clone(), dir.path().to_path_buf()] {
            let rescan = notify::Event::new(notify::EventKind::Other)
                .set_flag(notify::event::Flag::Rescan)
                .add_path(named.clone());
            let mut change = ChangeAccumulator::default();
            assert!(
                record_event_signal(&rescan, &roots, dir.path(), None, &rules, &mut change),
                "a rescan naming {} was dropped as noise",
                named.display()
            );
            assert!(change.kinds.contains(&ChangeKind::Unknown));
        }
    }

    #[test]
    fn another_checkouts_index_and_head_are_not_this_checkouts_change() {
        let dir = TempDir::new().unwrap();
        let common = dir.path().join("main/.git");
        let own = common.join("worktrees/a");
        let rules = IgnoreRules::builtin();
        let worktree = dir.path().join("a");
        // Linked worktree A: private dir first, shared common dir second.
        let linked = vec![own.clone(), common.clone()];
        let signal = |roots: &[PathBuf], p: PathBuf| {
            event_has_signal(&modify(p), roots, &worktree, None, &rules)
        };
        assert!(
            !signal(&linked, common.join("worktrees/b/index")),
            "sibling git add"
        );
        assert!(!signal(&linked, common.join("worktrees/b/logs/HEAD")));
        assert!(
            !signal(&linked, common.join("HEAD")),
            "the main checkout's HEAD"
        );
        assert!(
            !signal(&linked, common.join("index")),
            "the main checkout's index"
        );
        assert!(
            signal(&linked, own.join("index")),
            "this checkout's own index"
        );
        assert!(
            signal(&linked, own.join("HEAD")),
            "this checkout's own HEAD"
        );
        assert!(
            signal(&linked, common.join("refs/heads/topic")),
            "refs stay shared"
        );
        assert!(
            signal(&linked, common.join("worktrees/b")),
            "the worktree list moved"
        );
        assert!(
            signal(&linked, common.join("worktrees/b/HEAD")),
            "a sibling's checkout moved a branch"
        );
        // The main checkout watches the same directory as its own.
        let main = vec![common.clone()];
        assert!(
            !signal(&main, common.join("worktrees/b/index")),
            "sibling git add"
        );
        assert!(signal(&main, common.join("index")), "its own index");
        assert!(signal(&main, common.join("HEAD")), "its own HEAD");
    }

    /// Runs the real debounce loop over a hand-fed event channel, reporting
    /// when each refresh was emitted.
    struct FedLoop {
        events: WakeSender,
        emitted: std::sync::mpsc::Receiver<(Instant, RepoChangedPayload)>,
        stop: Arc<StopSignal>,
        timer_wakes: Arc<std::sync::atomic::AtomicU64>,
        thread: Option<thread::JoinHandle<()>>,
        root: PathBuf,
        _dir: TempDir,
    }

    impl FedLoop {
        fn start(overflow_first: bool) -> Self {
            let dir = TempDir::new().unwrap();
            git_init(dir.path(), false);
            let root = dir.path().canonicalize().unwrap();
            let git_dir = root.join(".git");
            let mut watcher = RepoFileWatcher::watch(&git_dir).unwrap();
            let (events, receiver) = std::sync::mpsc::sync_channel(debouncer::EVENT_QUEUE_CAPACITY);
            watcher.receiver = receiver;
            if overflow_first {
                watcher.simulate_overflow();
            }
            // This loop reads the hand-fed channel, so that is the one a stop
            // has to wake.
            let stop = StopSignal::new();
            stop.arm(events.clone());
            let timer_wakes = Arc::<std::sync::atomic::AtomicU64>::default();
            let (tx, emitted) = std::sync::mpsc::channel();
            let ctx = WatchLoopContext {
                git_dir: git_dir.clone(),
                internal_roots: vec![git_dir],
                emit_path: root.to_string_lossy().into_owned(),
                timer_wakes: timer_wakes.clone(),
            };
            let (loop_stop, session_stop) = (stop.clone(), stop.clone());
            let thread = thread::spawn(move || {
                run_watch_loop(&watcher, ctx, loop_stop, None, session_stop, move |p| {
                    let _ = tx.send((Instant::now(), p));
                });
            });
            Self {
                events,
                emitted,
                stop,
                timer_wakes,
                thread: Some(thread),
                root,
                _dir: dir,
            }
        }

        fn send(&self, event: notify::Event) {
            self.events.send(Ok(event)).unwrap();
        }
    }

    impl Drop for FedLoop {
        fn drop(&mut self) {
            self.stop.stop();
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    /// Continuous 10 Hz noise (`index.lock`, fsmonitor cookies) used to hold a
    /// real change back for as long as it lasted: every noise event hit a
    /// `continue` before the emission check, and the receive never timed out.
    #[test]
    fn a_real_change_is_emitted_within_the_cap_under_continuous_noise() {
        let fed = FedLoop::start(false);
        let git_dir = fed.root.join(".git");
        let began = Instant::now();
        fed.send(modify(fed.root.join("src").join("main.rs")));
        let sender = fed.events.clone();
        let noise_stop = Arc::new(AtomicBool::new(false));
        let noisy = noise_stop.clone();
        let noise = thread::spawn(move || {
            while !noisy.load(Ordering::Relaxed) {
                let _ = sender.send(Ok(modify(git_dir.join("index.lock"))));
                thread::sleep(Duration::from_millis(100));
            }
        });
        let first = fed.emitted.recv_timeout(Duration::from_secs(6));
        noise_stop.store(true, Ordering::Relaxed);
        noise.join().unwrap();
        let (at, payload) = first.expect("the change was never emitted under noise");
        assert!(
            at.duration_since(began) < Duration::from_millis(2500),
            "emitted after {:?}",
            at.duration_since(began)
        );
        assert!(payload.change.kinds.contains(&ChangeKind::Worktree));
    }

    #[test]
    fn a_synthetic_rescan_causes_exactly_one_full_refresh() {
        let fed = FedLoop::start(false);
        fed.send(
            notify::Event::new(notify::EventKind::Other)
                .set_flag(notify::event::Flag::Rescan)
                .add_path(fed.root.join(".git")),
        );
        let (_, payload) = fed
            .emitted
            .recv_timeout(Duration::from_secs(3))
            .expect("a rescan must refresh");
        assert!(
            payload.change.kinds.contains(&ChangeKind::Unknown),
            "{payload:?}"
        );
        assert!(
            fed.emitted
                .recv_timeout(Duration::from_millis(1200))
                .is_err(),
            "one rescan, one refresh"
        );
    }

    /// An idle watch has nothing to do until an event arrives, so it must not
    /// wake on a timer to find that out. The loop used to poll its channel
    /// every 200 ms for the stop flag, the repository's liveness and a debounce
    /// that was not pending: five wakes a second, per watched repository, for
    /// as long as the app was open.
    #[test]
    fn an_idle_watch_does_not_wake_on_a_timer() {
        let fed = FedLoop::start(false);
        thread::sleep(Duration::from_millis(1200));
        let wakes = fed.timer_wakes.load(Ordering::Relaxed);
        assert!(wakes <= 1, "an idle watch loop woke {wakes} times in 1.2 s");
    }

    /// The loop now sleeps for as long as 30 s when idle, so a stop that did
    /// not wake it would hang `unwatch` for that long (and fail it at the
    /// 3 s retirement deadline).
    #[test]
    fn a_stop_ends_a_sleeping_loop_without_waiting_for_its_heartbeat() {
        let mut fed = FedLoop::start(false);
        thread::sleep(Duration::from_millis(100));
        let stopped_at = Instant::now();
        fed.stop.stop();
        fed.thread.take().unwrap().join().unwrap();
        assert!(
            stopped_at.elapsed() < crate::test_support::coverage_relaxed(Duration::from_secs(2)),
            "a stopped loop took {:?} to end",
            stopped_at.elapsed()
        );
    }

    #[test]
    fn a_stop_that_precedes_the_wake_channel_is_replayed_when_it_is_armed() {
        let signal = StopSignal::new();
        signal.stop();
        let (tx, rx) = std::sync::mpsc::sync_channel(4);
        signal.arm(tx);
        assert!(rx.try_recv().is_ok(), "the late arm did not wake anyone");
        assert!(signal.is_stopped());
    }

    #[test]
    fn a_watcher_is_alive_until_its_backend_is_released() {
        let dir = TempDir::new().unwrap();
        git_init(dir.path(), false);
        let mut watcher = RepoFileWatcher::watch(&dir.path().join(".git")).unwrap();
        assert!(watcher.backend_alive());
        watcher.shutdown().unwrap();
        assert!(!watcher.backend_alive());
    }

    fn plan(now: Instant) -> WaitPlan {
        WaitPlan {
            now,
            pending: false,
            last_event: now,
            first_pending: None,
            last_scan: None,
            dead_misses: 0,
            rules_retry_at: None,
        }
    }

    #[test]
    fn an_idle_loop_waits_for_its_heartbeat() {
        assert_eq!(next_wait(&plan(Instant::now())), IDLE_HEARTBEAT);
    }

    #[test]
    fn a_missed_repository_is_looked_for_again_soon() {
        let now = Instant::now();
        let mut p = plan(now);
        p.dead_misses = 1;
        assert_eq!(next_wait(&p), DEAD_RECHECK);
    }

    #[test]
    fn a_pending_refresh_wakes_the_loop_when_it_has_been_quiet_long_enough() {
        let now = Instant::now();
        let mut p = plan(now);
        p.pending = true;
        p.first_pending = Some(now);
        assert_eq!(next_wait(&p), DEBOUNCE_QUIET);
        p.last_event = now - Duration::from_millis(150);
        assert_eq!(next_wait(&p), DEBOUNCE_QUIET - Duration::from_millis(150));
        // Quiet already: due now, not later.
        p.last_event = now - DEBOUNCE_QUIET;
        assert_eq!(next_wait(&p), Duration::ZERO);
        p.last_event = now - DEBOUNCE_QUIET - Duration::from_secs(5);
        assert_eq!(next_wait(&p), Duration::ZERO);
    }

    #[test]
    fn continuous_churn_is_cut_at_the_max_wait() {
        let now = Instant::now();
        let mut p = plan(now);
        p.pending = true;
        p.last_event = now - Duration::from_millis(10);
        p.first_pending = Some(now - DEBOUNCE_MAX_WAIT + Duration::from_millis(100));
        assert_eq!(next_wait(&p), Duration::from_millis(100));
        p.first_pending = Some(now - DEBOUNCE_MAX_WAIT);
        assert_eq!(next_wait(&p), Duration::ZERO);
    }

    /// Without this the loop would be told "due now" by the quiet rule and
    /// find `scan_is_due` false, and spin until the coalesce window ended.
    #[test]
    fn a_refresh_inside_the_coalesce_window_waits_for_the_window() {
        let now = Instant::now();
        let mut p = plan(now);
        p.pending = true;
        p.first_pending = Some(now - Duration::from_secs(1));
        p.last_event = now - DEBOUNCE_QUIET;
        p.last_scan = Some(now - Duration::from_millis(500));
        assert_eq!(next_wait(&p), SCAN_COALESCE - Duration::from_millis(500));
        p.last_scan = Some(now - SCAN_COALESCE);
        assert_eq!(next_wait(&p), Duration::ZERO);
    }

    #[test]
    fn the_wait_ends_exactly_when_a_scan_becomes_due() {
        let now = Instant::now();
        for (quiet_for, pending_for, scanned_ago) in [
            (0u64, 0u64, None),
            (150, 600, None),
            (399, 399, None),
            (10, 1990, None),
            (400, 700, Some(100u64)),
            (10, 2000, Some(1999)),
            (900, 900, Some(2000)),
        ] {
            let mut p = plan(now);
            p.pending = true;
            p.last_event = now - Duration::from_millis(quiet_for);
            p.first_pending = Some(now - Duration::from_millis(pending_for));
            p.last_scan = scanned_ago.map(|ms| now - Duration::from_millis(ms));
            let wait = next_wait(&p);
            let due =
                |at: Instant| scan_is_due(true, p.last_event, p.first_pending, p.last_scan, at);
            assert!(
                due(now + wait),
                "not due at the end of a {wait:?} wait: {quiet_for} {pending_for} {scanned_ago:?}"
            );
            if wait > Duration::ZERO {
                assert!(
                    !due(now + wait - Duration::from_nanos(1)),
                    "due before the end of a {wait:?} wait: {quiet_for} {pending_for} {scanned_ago:?}"
                );
            }
        }
    }

    #[test]
    fn a_rules_retry_bounds_the_wait_and_a_past_one_is_due_now() {
        let now = Instant::now();
        let mut p = plan(now);
        p.rules_retry_at = Some(now + Duration::from_secs(5));
        assert_eq!(next_wait(&p), Duration::from_secs(5));
        p.rules_retry_at = Some(now + Duration::from_secs(90));
        assert_eq!(next_wait(&p), IDLE_HEARTBEAT);
        p.rules_retry_at = Some(now - Duration::from_secs(1));
        assert_eq!(next_wait(&p), Duration::ZERO);
    }

    #[test]
    fn the_earliest_of_several_deadlines_wins() {
        let now = Instant::now();
        let mut p = plan(now);
        p.dead_misses = 2;
        p.pending = true;
        p.first_pending = Some(now);
        p.last_event = now - Duration::from_millis(350);
        p.rules_retry_at = Some(now + Duration::from_secs(1));
        assert_eq!(next_wait(&p), Duration::from_millis(50));
    }

    #[test]
    fn a_failed_ignore_query_is_retried_only_while_it_could_succeed() {
        let now = Instant::now();
        let mut rules = IgnoreRules {
            prefixes: HashSet::new(),
            files: HashSet::new(),
            load_error: None,
            truncated: false,
        };
        assert_eq!(rules_retry_at(&rules, now), None);
        rules.load_error = Some("git ls-files timed out".into());
        assert_eq!(rules_retry_at(&rules, now), Some(now + IGNORE_RETRY));
        // A truncated answer is final: asking again returns the same truncation.
        rules.truncated = true;
        assert_eq!(rules_retry_at(&rules, now), None);
    }

    #[test]
    fn a_dropped_event_queue_causes_one_full_refresh() {
        let fed = FedLoop::start(true);
        let (_, payload) = fed
            .emitted
            .recv_timeout(Duration::from_secs(3))
            .expect("an overflow must refresh");
        assert!(
            payload.change.kinds.contains(&ChangeKind::Unknown),
            "{payload:?}"
        );
        assert!(fed
            .emitted
            .recv_timeout(Duration::from_millis(1200))
            .is_err());
    }

    // ---- The recursive worktree-root watch (macOS) ---------------------------

    #[cfg(target_os = "macos")]
    fn wait_for(what: &str, mut condition: impl FnMut() -> bool) {
        let deadline = Instant::now() + PRIME_DEADLINE;
        while !condition() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            thread::sleep(Duration::from_millis(100));
        }
    }

    /// The rule per platform, with its reason: FSEvents is recursive in the
    /// kernel and the backend's `NonRecursive` mode only discards nested paths
    /// in user space; inotify and ReadDirectoryChangesW pay for recursion.
    #[test]
    fn the_root_is_watched_recursively_only_where_the_kernel_already_is() {
        assert_eq!(
            filter::ROOT_WATCH_MODE == notify::RecursiveMode::Recursive,
            cfg!(target_os = "macos")
        );
    }

    /// Before this change the root watch dropped every path below the top
    /// level, so the 6 s poll was the only thing that saw `src/deep/er/x.rs`.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_nested_edit_is_announced_where_the_root_watch_is_recursive() {
        let temp = TempDir::new().unwrap();
        let (rx, stop, root, _) = spawn_loop_observed(temp.path());
        let nested = root.join("src").join("deep").join("er");
        std::fs::create_dir_all(&nested).unwrap();
        prime_watcher(&rx, &root, PRIME_DEADLINE);
        let payload = write_until_announced(
            &rx,
            PRIME_DEADLINE,
            "nested-edit",
            |name| std::fs::write(nested.join(name), "x").unwrap(),
            |payload, probe| {
                let wanted = format!("src/deep/er/{probe}");
                payload.change.paths.contains(&wanted)
            },
        );
        assert!(
            payload.change.kinds.contains(&ChangeKind::Worktree),
            "{payload:?}"
        );
        stop.stop();
    }

    /// The Linux and Windows rule, run where it can be: the same call with the
    /// non-recursive mode delivers the top level and not what is below it.
    #[test]
    fn a_non_recursive_root_watch_delivers_the_top_level_and_not_what_is_below() {
        let dir = TempDir::new().unwrap();
        git_init(dir.path(), false);
        let git_dir = dir.path().join(".git");
        let nested = dir.path().join("src").join("deep");
        std::fs::create_dir_all(&nested).unwrap();
        let watcher = RepoFileWatcher::watch_repo_with(
            &git_dir,
            Some(dir.path()),
            None,
            notify::RecursiveMode::NonRecursive,
        )
        .expect("watch");
        thread::sleep(Duration::from_millis(400));
        while watcher.receiver.try_recv().is_ok() {}

        std::fs::write(nested.join("below.rs"), "x").unwrap();
        std::fs::write(dir.path().join("top-sentinel.rs"), "x").unwrap();
        let (mut below, mut sentinel) = (0u32, 0u32);
        let deadline = Instant::now() + PRIME_DEADLINE;
        while sentinel == 0 {
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                break;
            };
            let Ok(Ok(event)) = watcher.receiver.recv_timeout(left) else {
                break;
            };
            for path in &event.paths {
                match path.file_name().and_then(|name| name.to_str()) {
                    Some("below.rs") => below += 1,
                    Some("top-sentinel.rs") => sentinel += 1,
                    _ => {}
                }
            }
        }
        while let Ok(Ok(event)) = watcher.receiver.try_recv() {
            below += event
                .paths
                .iter()
                .filter(|path| path.file_name().is_some_and(|name| name == "below.rs"))
                .count() as u32;
        }
        assert!(sentinel >= 1, "the top-level file must still arrive");
        assert_eq!(below, 0, "a non-recursive root watch must not see below");
    }

    /// 10,000 files written under a directory git ignores, through the real
    /// watcher and loop: none is announced, the callback classified each path
    /// once (no git is spawned per event: the rule set is a hash lookup), and
    /// nothing reloaded the rules.
    #[cfg(target_os = "macos")]
    #[test]
    fn ten_thousand_writes_under_an_ignored_directory_are_never_announced() {
        let temp = TempDir::new().unwrap();
        let (rx, stop, root, filter) = spawn_loop_observed(temp.path());
        let out = root.join("out");
        for shard in 0..50 {
            std::fs::create_dir_all(out.join(format!("d{shard}"))).unwrap();
        }
        std::fs::write(out.join("seed.o"), "x").unwrap();
        std::fs::write(root.join(".gitignore"), "out/\n").unwrap();
        // The rule file edit and the new directory are what reload the rules.
        wait_for("the rules to be reloaded with `out/`", || {
            filter.loads() >= 2
        });
        prime_watcher(&rx, &root, PRIME_DEADLINE);
        let (loads, examined) = (filter.loads(), filter.examined());

        for i in 0..10_000u32 {
            std::fs::write(
                out.join(format!("d{}", i % 50)).join(format!("f{i}.o")),
                "x",
            )
            .unwrap();
        }
        let payload = write_until_announced(
            &rx,
            PRIME_DEADLINE,
            "sentinel",
            |name| std::fs::write(root.join(name), "x").unwrap(),
            names_probe,
        );
        assert!(
            payload.change.paths.iter().all(|p| !p.starts_with("out/")),
            "{payload:?}"
        );
        while let Ok(late) = rx.try_recv() {
            assert!(late.change.paths.iter().all(|p| !p.starts_with("out/")));
        }
        assert_eq!(filter.loads(), loads, "writes in `out/` reload nothing");
        let seen = filter.examined() - examined;
        assert!(
            (10_000..40_000).contains(&seen),
            "classifier work is one pass per delivered path, got {seen}"
        );
        stop.stop();
    }

    /// What triggers a reload, and how often. A terminal `git status` / `git
    /// add` rewrites the index, which is NOT a trigger; a storm of new
    /// directories is, but at most once per coalesce window.
    #[test]
    fn only_a_rule_file_or_a_new_directory_asks_for_the_rules_again() {
        let roots = vec![PathBuf::from("/repo/.git")];
        let event = |kind: EventKind, path: &str| Event::new(kind).add_path(PathBuf::from(path));
        let modify = EventKind::Modify(notify::event::ModifyKind::Any);
        let folder = EventKind::Create(notify::event::CreateKind::Folder);
        for recursive in [true, false] {
            let asks = |e: &Event| event_may_change_ignore_rules(e, &roots, recursive);
            assert!(asks(&event(modify, "/repo/.gitignore")));
            assert!(asks(&event(modify, "/repo/a/.gitignore")));
            assert!(asks(&event(modify, "/repo/.git/info/exclude")));
            assert!(!asks(&event(folder, "/repo/.git/objects/ab")));
            assert!(!asks(&event(modify, "/repo/.git/index")));
            assert!(!asks(&event(modify, "/repo/.git/HEAD")));
            assert!(!asks(&event(modify, "/repo/.git/config")));
            assert!(!asks(&event(modify, "/repo/src/main.rs")));
        }
        // A new directory asks only where the root watch is recursive: where
        // it is not (Linux, Windows) nothing below it is delivered, so a stale
        // list hides nothing and a `git ls-files` per directory would be new
        // cost.
        assert!(event_may_change_ignore_rules(
            &event(folder, "/repo/dist"),
            &roots,
            true
        ));
        assert!(!event_may_change_ignore_rules(
            &event(folder, "/repo/dist"),
            &roots,
            false
        ));
    }

    // ---- Ignored files, listed one by one ---------------------------------

    #[test]
    fn the_listing_names_ignored_files_and_the_bound_stops_it() {
        let listing =
            "target/\0docs/lcov.info\0.DS_Store\0harness/.devmap-preview.json\0scratch/\0";
        let files = parse_ignored_files(listing);
        let mut names: Vec<&str> = files.iter().map(String::as_str).collect();
        names.sort_unstable();
        assert_eq!(
            names,
            [
                ".DS_Store",
                "docs/lcov.info",
                "harness/.devmap-preview.json"
            ]
        );

        let mut huge = String::new();
        for i in 0..(MAX_IGNORE_FILES + 50) {
            huge.push_str(&format!("d/f{i}.log\0"));
        }
        assert_eq!(parse_ignored_files(&huge).len(), MAX_IGNORE_FILES);
        let long = format!("{}/x\0", "a".repeat(MAX_IGNORE_FILE_BYTES));
        assert!(
            parse_ignored_files(&long).is_empty(),
            "past the byte bound: not recorded"
        );
    }

    #[test]
    fn a_real_listing_carries_the_ignored_files_and_not_the_tracked_ones() {
        let dir = TempDir::new().unwrap();
        git_init(dir.path(), false);
        let root = dir.path();
        std::fs::write(root.join(".gitignore"), "*.local.json\nlcov.info\n").unwrap();
        std::fs::create_dir_all(root.join("a/b")).unwrap();
        std::fs::write(root.join("a/b/settings.local.json"), "x").unwrap();
        std::fs::write(root.join("a/b/lcov.info"), "x").unwrap();
        std::fs::write(root.join("a/b/tracked.json"), "x").unwrap();
        git_in(root, &["add", ".gitignore", "a/b/tracked.json"]);
        git_in(root, &["commit", "-m", "base"]);
        let rules = IgnoreRules::load(root);
        assert!(rules.load_error.is_none(), "{:?}", rules.load_error);
        assert!(
            rules.files.contains("a/b/settings.local.json"),
            "{:?}",
            rules.files
        );
        assert!(rules.files.contains("a/b/lcov.info"));
        assert!(!rules.files.contains("a/b/tracked.json"));
    }

    /// User plus system CPU time of this process so far.
    #[cfg(target_os = "macos")]
    fn process_cpu() -> Duration {
        // SAFETY: `getrusage` fills the zeroed struct it is handed.
        let usage = unsafe {
            let mut usage: libc::rusage = std::mem::zeroed();
            libc::getrusage(libc::RUSAGE_SELF, &mut usage);
            usage
        };
        let micros = |t: libc::timeval| t.tv_sec as u64 * 1_000_000 + t.tv_usec as u64;
        Duration::from_micros(micros(usage.ru_utime) + micros(usage.ru_stime))
    }

    /// What the recursive root watch costs the callback: 50,000 files created
    /// in an ignored directory with (a) no watcher, (b) main's watcher (root
    /// NON-recursive: the backend discards nested paths before our callback),
    /// (c) stage 1's (root recursive, events filtered in the callback). Counts
    /// the native callbacks we receive, the events forwarded to the queue, and
    /// the process CPU time spent from the first write until the callbacks have
    /// gone quiet. The writer's own CPU is arm (a); the difference is the
    /// watcher's. Arms are interleaved over fresh repositories, n = 3. Run on a
    /// quiet host: `cargo test --lib watcher::tests::measure_callback_cost --
    /// --ignored --nocapture`. A run that starts or ends above load 30 is
    /// printed as CONTAMINATED and is not a result.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "measurement; 50,000 file creations per arm on a quiet host"]
    fn measure_callback_cost_of_ignored_churn_main_versus_stage_1() {
        const FILES: u32 = 50_000;
        const REPS: usize = 3;
        let load = || {
            let mut load = [0f64; 3];
            // SAFETY: `getloadavg` writes at most `3` doubles into the array.
            let got = unsafe { libc::getloadavg(load.as_mut_ptr(), 3) };
            if got >= 1 {
                load[0]
            } else {
                f64::NAN
            }
        };
        let load_start = load();
        // (callbacks, forwarded, cpu ms) per repetition, per arm.
        let mut rows: [Vec<(u64, usize, u128)>; 3] = [Vec::new(), Vec::new(), Vec::new()];
        for _ in 0..REPS {
            for (arm, mode) in [
                None,
                Some(notify::RecursiveMode::NonRecursive),
                Some(notify::RecursiveMode::Recursive),
            ]
            .into_iter()
            .enumerate()
            {
                let temp = TempDir::new().unwrap();
                git_init(temp.path(), false);
                let root = temp.path().canonicalize().unwrap();
                let out = root.join("out");
                for shard in 0..50 {
                    std::fs::create_dir_all(out.join(format!("d{shard}"))).unwrap();
                }
                std::fs::write(root.join(".gitignore"), "out/\n").unwrap();
                let watcher = mode.map(|mode| {
                    let watcher = RepoFileWatcher::watch_repo_with(
                        &root.join(".git"),
                        Some(&root),
                        None,
                        mode,
                    )
                    .expect("watch");
                    watcher
                        .filter()
                        .publish_rules(Arc::new(IgnoreRules::load(&root)));
                    thread::sleep(Duration::from_millis(600));
                    while watcher.receiver.try_recv().is_ok() {}
                    watcher
                });
                let (calls_before, cpu_before) = (
                    watcher.as_ref().map_or(0, |w| w.filter().callbacks()),
                    process_cpu(),
                );
                for i in 0..FILES {
                    std::fs::write(
                        out.join(format!("d{}", i % 50)).join(format!("f{i}.o")),
                        "x",
                    )
                    .unwrap();
                }
                // Quiet: the callback count has not moved for 2 s.
                let mut last = u64::MAX;
                let mut still_since = Instant::now();
                let quiet = Duration::from_secs(2);
                while still_since.elapsed() < quiet {
                    thread::sleep(Duration::from_millis(200));
                    let now = watcher.as_ref().map_or(0, |w| w.filter().callbacks());
                    if now != last {
                        last = now;
                        still_since = Instant::now();
                    }
                }
                let cpu = process_cpu() - cpu_before;
                let (callbacks, forwarded) = watcher.as_ref().map_or((0, 0), |w| {
                    (
                        w.filter().callbacks() - calls_before,
                        w.receiver.try_iter().count(),
                    )
                });
                rows[arm].push((callbacks, forwarded, cpu.as_millis()));
            }
        }
        let load_end = load();
        let contaminated = load_start > 30.0 || load_end > 30.0 || load_start.is_nan();
        eprintln!(
            "MEASURE {FILES} creations in an ignored directory, n={REPS}, (callbacks, forwarded, process cpu ms): \\
             no-watcher={:?} main(non-recursive)={:?} stage1(recursive+filter)={:?}; \\
             load 1m start={load_start:.1} end={load_end:.1} {}",
            rows[0],
            rows[1],
            rows[2],
            if contaminated { "CONTAMINATED (above 30): not a result" } else { "clean" }
        );
        assert!(rows[2].iter().all(|(_, forwarded, _)| *forwarded == 0));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn index_writes_never_reload_the_rules_and_a_directory_storm_reloads_at_most_once_per_window() {
        let temp = TempDir::new().unwrap();
        let (rx, stop, root, filter) = spawn_loop_observed(temp.path());
        prime_watcher(&rx, &root, PRIME_DEADLINE);
        wait_for("the first reload to settle", || filter.loads() >= 1);
        thread::sleep(SCAN_COALESCE + Duration::from_millis(300));
        let settled = filter.loads();

        // Twenty index writes, as a terminal full of `git add` would make.
        for i in 0..20 {
            std::fs::write(root.join(format!("f{i}.txt")), "x").unwrap();
            git_in(&root, &["add", &format!("f{i}.txt")]);
        }
        thread::sleep(SCAN_COALESCE + Duration::from_millis(500));
        assert_eq!(filter.loads(), settled, "index writes reload nothing");

        // Thirty new directories over about three seconds.
        let storm = Instant::now();
        for i in 0..30 {
            std::fs::create_dir_all(root.join(format!("newdir{i}"))).unwrap();
            thread::sleep(Duration::from_millis(110));
        }
        let window = storm.elapsed();
        thread::sleep(SCAN_COALESCE + Duration::from_millis(500));
        let reloads = filter.loads() - settled;
        let allowed = window.as_secs_f64() / SCAN_COALESCE.as_secs_f64() + 2.0;
        assert!(reloads >= 1, "new directories do ask for the rules again");
        assert!(
            (reloads as f64) <= allowed,
            "{reloads} reloads in {window:?}: more than one per {SCAN_COALESCE:?} window"
        );
        stop.stop();
    }

    // ---- The poll skips its read only when NOTHING happened -----------------

    /// Waits until the epoch has not moved for `still`: the setup's own events
    /// (git init, directory creations) have been delivered, so a later change
    /// is the test's.
    #[cfg(target_os = "macos")]
    fn wait_for_a_still_epoch(skip: &skip::PollSkip) {
        let still = DEBOUNCE_MAX_WAIT + Duration::from_millis(500);
        let mut seen = skip.epoch_now();
        let mut since = Instant::now();
        wait_for("a still epoch", || {
            let now = skip.epoch_now();
            if now != seen {
                seen = now;
                since = Instant::now();
            }
            since.elapsed() >= still
        });
    }

    /// A trusted repository under a live session, and a counter of the `git
    /// status` spawns made for it while `body` runs.
    fn watched_repo_counting_status_reads<T>(
        body: impl FnOnce(&WatcherState, &str, &Path, &Arc<std::sync::atomic::AtomicUsize>) -> T,
    ) -> T {
        let temp = TempDir::new().unwrap();
        git_init(temp.path(), false);
        crate::test_support::trust_repo(temp.path());
        let root = temp.path().canonicalize().unwrap();
        let path = root.to_string_lossy().into_owned();
        let state = WatcherState::default();
        let reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = Arc::clone(&reads);
        let out = crate::engine::worktree::status_probes::with_probe(
            std::slice::from_ref(&root),
            move |_| {
                counted.fetch_add(1, Ordering::SeqCst);
            },
            || body(&state, &path, &root, &reads),
        );
        let _ = unwatch_all(&state);
        out
    }

    fn poll(state: &WatcherState, path: &str) -> Vec<FileStatus> {
        status_through_watch(state, path, true, || {
            crate::engine::git_reader::GitReader::get_status(path)
        })
        .expect("status")
    }

    #[cfg(target_os = "macos")]
    fn skip_of(state: &WatcherState, path: &str) -> Arc<filter::EventFilter> {
        state.filter_for(path).expect("a live session has a filter")
    }

    fn reads_of(reads: &Arc<std::sync::atomic::AtomicUsize>) -> usize {
        reads.load(Ordering::SeqCst)
    }

    /// The premise of the whole skip: an idle worktree delivers no event, and
    /// neither does GitPulse's own refresh read set (it runs with
    /// `GIT_OPTIONAL_LOCKS=0`), so the poll is answered from the kept read.
    /// Measured with the real stream; under `core.fsmonitor` and
    /// `core.splitIndex` git itself writes under `.git` on a read (cookie files,
    /// a shared-index touch), the epoch moves, and the poll reads every time as
    /// it did before. See `docs/PERFORMANCE.md`.
    #[cfg(target_os = "macos")]
    #[test]
    fn an_idle_worktree_and_its_own_reads_move_no_epoch_so_the_poll_is_skipped() {
        use crate::engine::git_reader::GitReader;
        watched_repo_counting_status_reads(|state, path, root, reads| {
            std::fs::write(root.join("tracked.txt"), "one\n").unwrap();
            git_in(root, &["add", "tracked.txt"]);
            git_in(root, &["commit", "-m", "seed"]);
            git_in(root, &["tag", "v1"]);
            git_in(root, &["branch", "topic"]);
            std::fs::write(root.join("tracked.txt"), "two\n").unwrap();
            std::fs::write(root.join("staged.txt"), "s\n").unwrap();
            git_in(root, &["add", "staged.txt"]);
            std::fs::write(root.join("untracked.txt"), "u\n").unwrap();
            start_watch_inner(state, path.to_string(), |_| {}).unwrap();
            let filter = skip_of(state, path);
            wait_for_a_still_epoch(filter.skip());
            let epoch = filter.skip().epoch_now();
            for _ in 0..10 {
                // The reads one refresh makes, then the poll.
                GitReader::get_status(path).expect("status");
                GitReader::list_branches(path).expect("branches");
                GitReader::list_tags(path).expect("tags");
                GitReader::branch_stats(path).expect("stats");
                crate::engine::stash::list(path).expect("stash");
                GitReader::head_id(path).expect("head");
                poll(state, path);
            }
            thread::sleep(Duration::from_millis(1500));
            assert_eq!(
                filter.skip().epoch_now(),
                epoch,
                "GitPulse's own reads moved the epoch"
            );
            let before = reads_of(reads);
            let skipped_before = filter.skip().counted(skip::Outcome::Skipped);
            for _ in 0..5 {
                poll(state, path);
            }
            assert_eq!(
                reads_of(reads),
                before,
                "nothing happened: the polls are skipped"
            );
            assert_eq!(
                filter.skip().counted(skip::Outcome::Skipped) - skipped_before,
                5,
                "the counters saw the five skips"
            );
        });
    }

    /// Any event at all ends the skip: a plain edit, a write into an ignored
    /// directory, and a touch of git-internal noise.
    #[cfg(target_os = "macos")]
    #[test]
    fn any_event_at_all_makes_the_next_poll_read() {
        watched_repo_counting_status_reads(|state, path, root, reads| {
            std::fs::write(root.join(".gitignore"), "out/\n").unwrap();
            std::fs::create_dir_all(root.join("out")).unwrap();
            start_watch_inner(state, path.to_string(), |_| {}).unwrap();
            let filter = skip_of(state, path);
            wait_for_a_still_epoch(filter.skip());
            poll(state, path);
            poll(state, path);
            assert_eq!(reads_of(reads), 1, "the second poll is skipped");
            type Act = fn(&Path);
            let cases: [(&str, Act); 3] = [
                ("a plain edit", |root| {
                    std::fs::write(root.join("plain.txt"), "x").unwrap()
                }),
                ("a write into an ignored directory", |root| {
                    std::fs::write(root.join("out/build.o"), "x").unwrap()
                }),
                ("git-internal noise", |root| {
                    std::fs::write(root.join(".git/index.lock"), "").unwrap()
                }),
            ];
            for (what, act) in cases {
                wait_for_a_still_epoch(filter.skip());
                let (epoch, before) = (filter.skip().epoch_now(), reads_of(reads));
                act(root);
                wait_for(what, || filter.skip().epoch_now() != epoch);
                poll(state, path);
                assert_eq!(
                    reads_of(reads),
                    before + 1,
                    "{what} must make the poll read"
                );
                poll(state, path);
                assert_eq!(reads_of(reads), before + 1, "and the read is kept again");
            }
        });
    }

    /// The rename that broke the ignore-list design: `a/gen` was ignored by
    /// `a/.gitignore`, then `a` was renamed away and another directory took its
    /// name. The events move the epoch, so the poll reads and shows the file.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_renamed_ancestor_of_a_rule_file_is_seen_because_the_events_move_the_epoch() {
        watched_repo_counting_status_reads(|state, path, root, reads| {
            std::fs::create_dir_all(root.join("a/gen")).unwrap();
            std::fs::write(root.join("a/.gitignore"), "gen/\n").unwrap();
            std::fs::write(root.join("a/gen/seed.rs"), "x").unwrap();
            std::fs::create_dir_all(root.join("c/gen")).unwrap();
            std::fs::write(root.join("c/gen/seed.rs"), "x").unwrap();
            start_watch_inner(state, path.to_string(), |_| {}).unwrap();
            let filter = skip_of(state, path);
            wait_for_a_still_epoch(filter.skip());
            poll(state, path);
            poll(state, path);
            let (epoch, before) = (filter.skip().epoch_now(), reads_of(reads));
            std::fs::rename(root.join("a"), root.join("z")).unwrap();
            std::fs::rename(root.join("c"), root.join("a")).unwrap();
            std::fs::write(root.join("a/gen/new.rs"), "x").unwrap();
            wait_for("the renames to reach the watch", || {
                filter.skip().epoch_now() != epoch
            });
            thread::sleep(Duration::from_millis(1500));
            let rows = poll(state, path);
            assert_eq!(reads_of(reads), before + 1, "the poll reads");
            assert!(rows.iter().any(|r| r.path == "a/gen/new.rs"), "{rows:?}");
        });
    }

    /// The `:build` directory that `git check-ignore` read as a pathspec: it
    /// holds only a swap file, then a source file.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_colon_directory_holding_only_a_swap_file_is_seen_when_a_source_file_arrives() {
        watched_repo_counting_status_reads(|state, path, root, reads| {
            std::fs::write(root.join(".gitignore"), "build\n*.swp\n").unwrap();
            std::fs::create_dir_all(root.join(":build")).unwrap();
            std::fs::write(root.join(":build/.a.swp"), "x").unwrap();
            start_watch_inner(state, path.to_string(), |_| {}).unwrap();
            let filter = skip_of(state, path);
            wait_for_a_still_epoch(filter.skip());
            poll(state, path);
            poll(state, path);
            let (epoch, before) = (filter.skip().epoch_now(), reads_of(reads));
            std::fs::write(root.join(":build/a.rs"), "x").unwrap();
            wait_for("the write to reach the watch", || {
                filter.skip().epoch_now() != epoch
            });
            let rows = poll(state, path);
            assert_eq!(reads_of(reads), before + 1);
            assert!(
                rows.iter().any(|r| r.path.starts_with(":build")),
                "{rows:?}"
            );
        });
    }

    /// Review 2's sequence: a directory created holding only `.a.rs.swp`
    /// (so the ignore listing prints it), then the source file.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_directory_that_holds_only_a_swap_file_shows_the_source_file_written_next() {
        watched_repo_counting_status_reads(|state, path, root, reads| {
            std::fs::write(root.join(".gitignore"), "*.swp\n").unwrap();
            git_in(root, &["add", ".gitignore"]);
            git_in(root, &["commit", "-m", "ignore swap files"]);
            start_watch_inner(state, path.to_string(), |_| {}).unwrap();
            let filter = skip_of(state, path);
            wait_for_a_still_epoch(filter.skip());
            std::fs::create_dir_all(root.join("src/x")).unwrap();
            std::fs::write(root.join("src/x/.a.rs.swp"), "x").unwrap();
            wait_for_a_still_epoch(filter.skip());
            poll(state, path);
            poll(state, path);
            let (epoch, before) = (filter.skip().epoch_now(), reads_of(reads));
            std::fs::write(root.join("src/x/a.rs"), "fn main() {}\n").unwrap();
            wait_for("the write to reach the watch", || {
                filter.skip().epoch_now() != epoch
            });
            let rows = poll(state, path);
            assert_eq!(reads_of(reads), before + 1);
            assert!(rows.iter().any(|r| r.path.starts_with("src/x")), "{rows:?}");
        });
    }

    /// Review 1's sequence: a file under an ignored directory becomes tracked
    /// (`git add -f`), then is edited.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_file_that_becomes_tracked_inside_an_ignored_directory_is_seen_when_edited() {
        watched_repo_counting_status_reads(|state, path, root, reads| {
            std::fs::write(root.join(".gitignore"), "out/\n").unwrap();
            std::fs::create_dir_all(root.join("out/sub")).unwrap();
            std::fs::write(root.join("out/sub/f.txt"), "one\n").unwrap();
            start_watch_inner(state, path.to_string(), |_| {}).unwrap();
            let filter = skip_of(state, path);
            wait_for_a_still_epoch(filter.skip());
            git_in(root, &["add", "-f", "out/sub/f.txt"]);
            git_in(
                root,
                &["commit", "-m", "track a file under an ignored directory"],
            );
            wait_for_a_still_epoch(filter.skip());
            poll(state, path);
            poll(state, path);
            let (epoch, before) = (filter.skip().epoch_now(), reads_of(reads));
            std::fs::write(root.join("out/sub/f.txt"), "two\n").unwrap();
            wait_for("the edit to reach the watch", || {
                filter.skip().epoch_now() != epoch
            });
            let rows = poll(state, path);
            assert_eq!(reads_of(reads), before + 1);
            assert!(
                rows.iter()
                    .any(|r| r.path == "out/sub/f.txt" && r.status_code.contains('M')),
                "{rows:?}"
            );
        });
    }

    /// Finding C: 2,100 untracked files (past the 2,000-file scan budget) in an
    /// unchanged tree. The read at the cap returns different line counts from
    /// the first, which once convicted a healthy watch; paths and status codes
    /// do not differ, so the watch stays trusted and the poll keeps skipping.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_large_unchanged_tree_with_budget_limited_counts_is_not_convicted() {
        watched_repo_counting_status_reads(|state, path, root, reads| {
            let dir = root.join("many");
            std::fs::create_dir_all(&dir).unwrap();
            for i in 0..2_100 {
                std::fs::write(dir.join(format!("u{i}.txt")), format!("line {i}\n")).unwrap();
            }
            start_watch_inner(state, path.to_string(), |_| {}).unwrap();
            let filter = skip_of(state, path);
            wait_for_a_still_epoch(filter.skip());
            // The shortened cap goes through the suite's load-scaling helper (the
            // one `hermetic_contract` requires for wall-clock limits), so a slow
            // read on a busy host stretches the cap with it instead of eating it.
            let cap = crate::test_support::coverage_relaxed(Duration::from_millis(1500));
            filter.skip().set_timings(
                cap,
                crate::test_support::coverage_relaxed(Duration::from_secs(1)),
            );
            let first = poll(state, path);
            assert!(first.len() >= 2_100, "{} rows", first.len());
            thread::sleep(cap + Duration::from_millis(100));
            poll(state, path);
            // Straight after the cap read: served (the cap is measured from
            // when that read began, so a slow read must not eat it).
            poll(state, path);
            assert!(
                !filter.skip().is_convicted(),
                "a healthy watch was convicted"
            );
            assert_eq!(reads_of(reads), 2, "the cap forced one more real read");
        });
    }

    /// A stream that registers and says nothing, simulated by a session whose
    /// filter is never fed. A real change under it is served stale for at most
    /// the cap, then the read at the cap disagrees and the watch is convicted:
    /// every poll reads from then on. Gated to `unix` like the skip's own unit
    /// tests: it arms a recursive filter by hand, which needs a root inode.
    #[cfg(unix)]
    #[test]
    fn a_silent_stream_is_stale_at_most_until_the_cap_then_convicted() {
        watched_repo_counting_status_reads(|state, path, root, reads| {
            let filter = filter::EventFilter::new(
                Some(root),
                vec![root.join(".git")],
                notify::RecursiveMode::Recursive,
            );
            {
                let mut guard = state.lock_sessions().unwrap();
                insert_watch_session(&mut guard, path, &[], None).unwrap();
                guard.get_mut(path).unwrap().filter = Some(Arc::clone(&filter));
            }
            filter
                .skip()
                .set_timings(Duration::from_millis(400), Duration::from_millis(200));
            poll(state, path);
            std::fs::write(root.join("plain.txt"), "x").unwrap();
            let stale = poll(state, path);
            assert_eq!(reads_of(reads), 1, "served within the cap");
            assert!(stale.is_empty(), "this is the bounded staleness: {stale:?}");
            thread::sleep(Duration::from_millis(450));
            let rows = poll(state, path);
            assert_eq!(reads_of(reads), 2, "the cap forces a real read");
            assert!(rows.iter().any(|r| r.path == "plain.txt"), "{rows:?}");
            assert!(!filter.skip().is_convicted());
            thread::sleep(Duration::from_millis(250));
            poll(state, path);
            assert_eq!(reads_of(reads), 3);
            assert!(filter.skip().is_convicted(), "caught missing a change");
            poll(state, path);
            poll(state, path);
            assert_eq!(reads_of(reads), 5, "every poll reads now");
        });
    }

    /// Unknown is never clean, at the poll: a failed registration and a parked
    /// or evicted watch have no filter, so every poll reads.
    #[test]
    fn a_failed_or_parked_watch_makes_every_poll_read() {
        watched_repo_counting_status_reads(|state, path, _, reads| {
            assert!(start_watch_inner(state, "/nonexistent/for/sure".into(), |_| {}).is_err());
            assert!(state.filter_for("/nonexistent/for/sure").is_none());
            poll(state, path);
            poll(state, path);
            assert_eq!(reads_of(reads), 2, "never watched");
        });
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_parked_watch_has_no_kept_read_to_serve() {
        watched_repo_counting_status_reads(|state, path, _, reads| {
            start_watch_inner(state, path.to_string(), |_| {}).unwrap();
            let filter = skip_of(state, path);
            wait_for_a_still_epoch(filter.skip());
            poll(state, path);
            poll(state, path);
            assert_eq!(reads_of(reads), 1);
            unwatch(state, path.to_string()).unwrap();
            assert!(filter.skip().cover_at(Instant::now()).is_none());
            poll(state, path);
            poll(state, path);
            assert_eq!(reads_of(reads), 3, "parked: every poll reads");
        });
    }

    #[cfg(target_os = "macos")]
    fn load_average_1m() -> f64 {
        let mut load = [0f64; 3];
        // SAFETY: `getloadavg` writes at most `3` doubles into the array.
        let got = unsafe { libc::getloadavg(load.as_mut_ptr(), 3) };
        if got >= 1 {
            load[0]
        } else {
            f64::NAN
        }
    }

    /// Counter-based measurement: `git status` spawns for one open worktree
    /// over ten real polls at the production cadence (`STATUS_POLL_INTERVAL_MS`,
    /// 6 s, so 60 s), counted at the spawn site. Four arms run side by side in
    /// each repetition, in separate repositories so the counters cannot mix:
    /// an idle worktree and one with a build writing into an ignored directory
    /// every 200 ms, each polled the old way (`may_reuse` off: what the poll did
    /// before the skip, which stage 1 leaves untouched) and the new way. Run
    /// on a quiet host: `cargo test --lib watcher::tests::measure_poll_skip --
    /// --ignored --nocapture`. A run that starts or ends above load 30 is
    /// printed as CONTAMINATED and is not a result.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "measurement; five minutes of real polls on a quiet host"]
    fn measure_poll_skip_spawns_on_an_idle_and_a_churning_worktree() {
        const POLLS: usize = 10;
        const POLL_EVERY: Duration = Duration::from_secs(6);
        const REPS: usize = 5;
        let load_start = load_average_1m();
        // [idle-old, idle-new, churn-old, churn-new] spawns, per repetition.
        let mut spawns = [Vec::new(), Vec::new(), Vec::new(), Vec::new()];
        for _ in 0..REPS {
            let arms: Vec<_> = [(false, false), (false, true), (true, false), (true, true)]
                .into_iter()
                .map(|(churn, may_reuse)| {
                    thread::spawn(move || {
                        watched_repo_counting_status_reads(|state, path, root, reads| {
                            std::fs::write(root.join(".gitignore"), "out/\n").unwrap();
                            std::fs::create_dir_all(root.join("out")).unwrap();
                            start_watch_inner(state, path.to_string(), |_| {}).unwrap();
                            let filter = skip_of(state, path);
                            wait_for_a_still_epoch(filter.skip());
                            let stop = Arc::new(AtomicBool::new(false));
                            let writer = churn.then(|| {
                                let (stop, out) = (Arc::clone(&stop), root.join("out"));
                                thread::spawn(move || {
                                    let mut n = 0u32;
                                    while !stop.load(Ordering::Acquire) {
                                        let _ =
                                            std::fs::write(out.join(format!("f{}.o", n % 64)), "x");
                                        n += 1;
                                        thread::sleep(Duration::from_millis(200));
                                    }
                                })
                            });
                            for _ in 0..POLLS {
                                status_through_watch(state, path, may_reuse, || {
                                    crate::engine::git_reader::GitReader::get_status(path)
                                })
                                .unwrap();
                                thread::sleep(POLL_EVERY);
                            }
                            stop.store(true, Ordering::Release);
                            if let Some(writer) = writer {
                                let _ = writer.join();
                            }
                            reads_of(reads)
                        })
                    })
                })
                .collect();
            for (arm, handle) in arms.into_iter().enumerate() {
                spawns[arm].push(handle.join().unwrap());
            }
        }
        let load_end = load_average_1m();
        let contaminated = load_start > 30.0 || load_end > 30.0 || load_start.is_nan();
        eprintln!(
            "MEASURE git status spawns per {POLLS} polls at 6 s (60 s), n={REPS}, arms side by side per repetition: \
             idle/always-read={:?} idle/skip={:?} churn-in-ignored-dir/always-read={:?} churn/skip={:?}; \
             load 1m start={load_start:.1} end={load_end:.1} {}",
            spawns[0],
            spawns[1],
            spawns[2],
            spawns[3],
            if contaminated { "CONTAMINATED (above 30): not a result" } else { "clean" }
        );
        assert!(spawns[0].iter().chain(&spawns[2]).all(|n| *n == POLLS));
    }

    /// Linux and Windows: the root watch is not recursive, so nothing can show
    /// a worktree unchanged and every poll reads.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn without_a_recursive_root_watch_every_poll_reads() {
        watched_repo_counting_status_reads(|state, path, _, reads| {
            start_watch_inner(state, path.to_string(), |_| {}).unwrap();
            for _ in 0..4 {
                poll(state, path);
            }
            assert_eq!(reads_of(reads), 4);
        });
    }
}
