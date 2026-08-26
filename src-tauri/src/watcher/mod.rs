pub mod debouncer;

pub use debouncer::RepoFileWatcher;

use crate::engine::git_cli::{resolve_git_common_dir, resolve_git_dir, validate_repo};
use notify::Event;
use serde::Serialize;
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};

pub const MAX_WATCHES: usize = 24;

pub struct WatchSession {
    stop: Arc<AtomicBool>,
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
struct RepoChangedPayload {
    path: String,
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

/// True when `event` carries at least one path that can move repository state.
///
/// Events whose every path is git-internal refresh noise (see
/// [`is_git_internal_noise`]) are dropped before they enter the debounce
/// accumulation. Without this gate, editors/build tools churning `.lock`
/// transients or `COMMIT_EDITMSG` force a full app refresh every
/// [`DEBOUNCE_MAX_WAIT`] indefinitely — the anti-starvation bound turns pure
/// noise into a constant refresh loop. Events without any path cannot be
/// classified, so they count as signal (fail open toward refreshing).
fn event_has_signal(event: &Event, internal_roots: &[std::path::PathBuf]) -> bool {
    if event.paths.is_empty() {
        return true;
    }
    event
        .paths
        .iter()
        .any(|path| !is_git_internal_noise(path, internal_roots))
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
///
/// The filter is deliberately scoped to the git directories: identically
/// named files in the worktree root are tracked content with different
/// semantics (`Cargo.lock`, `yarn.lock`, `poetry.lock` are dependency state,
/// not transients) and must keep firing `repo-changed`.
pub(crate) fn is_git_internal_noise(path: &Path, internal_roots: &[std::path::PathBuf]) -> bool {
    if !internal_roots.iter().any(|root| path.starts_with(root)) {
        return false;
    }
    is_noise_leaf_name(path)
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
        || name.starts_with("gc.log");
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

fn run_watch_loop<F>(
    watcher: RepoFileWatcher,
    ctx: WatchLoopContext,
    stop: Arc<AtomicBool>,
    sessions: Option<std::sync::Arc<Mutex<HashMap<String, WatchSession>>>>,
    session_stop: Arc<AtomicBool>,
    on_change: F,
) where
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
    // When the watched git directory is deleted (repo moved/removed), notify
    // keeps delivering remove/error events forever, and the settle timer would
    // still fire one last `repo-changed` for the corpse. Liveness is therefore
    // checked at the exact point of emission: a dead path can never be
    // announced, and after the miss counter confirms it stays dead, the loop
    // exits and reaps its own session instead of hammering a dead path.
    const DEAD_MISSES: u32 = 3;
    let mut dead_misses: u32 = 0;
    let mut dead_confirmed = false;
    while !stop.load(Ordering::Relaxed) {
        if !git_dir.exists() {
            dead_misses += 1;
            if dead_misses >= DEAD_MISSES {
                dead_confirmed = true;
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
                let mut significant = event_has_signal(&event, &internal_roots);
                for leftover in watcher.receiver.try_iter() {
                    significant |= match leftover {
                        Ok(event) => event_has_signal(&event, &internal_roots),
                        Err(_) => true,
                    };
                }
                if !significant {
                    continue;
                }
                if !pending {
                    first_pending = Some(Instant::now());
                }
                pending = true;
                last_event = Instant::now();
                if should_emit(pending, last_event, first_pending, Instant::now()) {
                    if !git_dir.exists() {
                        dead_confirmed = true;
                        break;
                    }
                    on_change(path.clone());
                    pending = false;
                    first_pending = None;
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
                if should_emit(pending, last_event, first_pending, Instant::now()) {
                    if !git_dir.exists() {
                        dead_confirmed = true;
                        break;
                    }
                    on_change(path.clone());
                    pending = false;
                    first_pending = None;
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    if dead_confirmed {
        // Reap the session we belong to, but only while it is still ours —
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
    let canonical = validate_repo(&repo_path)?;
    let key = canonical.to_string_lossy().into_owned();
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
    let watcher =
        RepoFileWatcher::watch_repo(&git_dir, worktree_root.as_deref(), common_dir.as_deref())?;

    let stop = {
        let mut guard = state.lock_sessions()?;
        match insert_watch_session(&mut guard, &key, std::slice::from_ref(&repo_path))? {
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
            run_watch_loop(
                watcher,
                WatchLoopContext {
                    git_dir,
                    internal_roots,
                    emit_path,
                },
                thread_stop,
                Some(loop_sessions),
                session_stop,
                on_change,
            );
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
    // Pass 1: exact matches against map keys. Covers the healthy case where
    // canonicalization still works.
    for key in keys {
        guard.remove(&key);
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
        guard.remove(&key);
    }
    Ok(())
}

pub fn unwatch_all(state: &WatcherState) -> Result<(), String> {
    state.lock_sessions()?.clear();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
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
        let output = cmd.current_dir(dir).output().expect("spawn git");
        assert!(
            output.status.success(),
            "git init failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn git_in(dir: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args([
                "-c",
                "user.name=GitPulse",
                "-c",
                "user.email=gitpulse@test.local",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .current_dir(dir)
            .output()
            .expect("spawn git");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

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
            .output()
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
        (main, work_parent, work_path)
    }

    struct RestoreCwd(std::path::PathBuf);
    impl Drop for RestoreCwd {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.0);
        }
    }

    impl WatcherState {
        fn begin_watch_slot(&self, key: &str) -> Result<bool, String> {
            let mut guard = self.lock_sessions()?;
            Ok(insert_watch_session(&mut guard, key, &[])?.is_some())
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
        let dir = TempDir::new().unwrap();
        git_init(dir.path(), false);
        let state = WatcherState::default();
        let raw = dir.path().to_string_lossy().into_owned();
        let key = start_watch_inner(&state, raw.clone(), |_| {}).expect("watch");
        assert_eq!(state.watch_count().unwrap(), 1);

        let trailing = format!("{}/", raw.trim_end_matches('/'));
        if trailing != raw {
            unwatch(&state, trailing).unwrap();
            assert_eq!(
                state.watch_count().unwrap(),
                0,
                "trailing-slash alias must unwatch the canonical slot"
            );
            start_watch_inner(&state, raw.clone(), |_| {}).expect("rewatch");
        }

        let dotted = dir.path().join(".").to_string_lossy().into_owned();
        unwatch(&state, dotted).unwrap();
        assert_eq!(
            state.watch_count().unwrap(),
            0,
            "path/./ alias must unwatch the canonical slot"
        );

        let key_again = start_watch_inner(&state, raw.clone(), |_| {}).expect("rewatch");
        assert_eq!(key_again, key);
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
        assert_eq!(
            lexical_normalizations("/tmp/repo/"),
            vec!["/tmp/repo/".to_string(), "/tmp/repo".to_string()]
        );
        assert_eq!(lexical_normalizations("."), vec![".".to_string()]);
        assert_eq!(
            lexical_normalizations("relative/repo"),
            vec!["relative/repo".to_string()]
        );
    }

    #[test]
    fn test_unwatch_relative_path_does_not_canonicalize_against_cwd() {
        let dir = TempDir::new().unwrap();
        git_init(dir.path(), false);
        let state = WatcherState::default();
        let key = start_watch_inner(&state, dir.path().to_string_lossy().into_owned(), |_| {})
            .expect("watch");

        let _restore = RestoreCwd(std::env::current_dir().unwrap());
        std::env::set_current_dir(dir.path()).unwrap();
        unwatch(&state, ".".into()).unwrap();
        assert!(
            state.is_watching(&key).unwrap(),
            "unwatch(\".\") must not treat cwd as the watched repo"
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
            .output()
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
                    watcher,
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
    const PRIME_DEADLINE: Duration = Duration::from_secs(12);

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

    /// Regression (audit D1): once the watched git directory is gone, notify
    /// keeps delivering remove/error events and the old loop re-emitted
    /// `repo-changed` forever while the session stayed resident. The loop
    /// must fall silent and reap its session.
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
        let prime_deadline = Instant::now() + Duration::from_secs(8);
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
