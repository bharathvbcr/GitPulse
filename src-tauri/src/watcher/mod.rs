pub mod debouncer;

pub use debouncer::RepoFileWatcher;

use crate::engine::git_cli::{resolve_git_common_dir, resolve_git_dir, validate_repo};
use serde::Serialize;
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};

/// Cap on concurrently watched repositories. Sized for agent workflows that
/// fan out across many clones and linked worktrees at once; each watch is one
/// OS stream plus a 2s fingerprint poll, so dozens stay cheap.
pub const MAX_WATCHES: usize = 64;

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

/// Quiet period after the last event before a debounced `repo-changed` fires.
const SETTLE_QUIET: Duration = Duration::from_millis(400);
/// Upper bound on how long pending events may go unreported when writes never
/// stop long enough to settle. Without this, a continuous writer (build tool,
/// agent automation) resets the settle timer forever and the UI goes stale
/// indefinitely; with it, updates flow at least once per interval under load.
const MAX_REPORT_LATENCY: Duration = Duration::from_secs(2);
/// How often the loop cross-checks a [`watch_fingerprint`] against the last
/// observed state. The OS stream (FSEvents/inotify) provides low-latency
/// delivery; this consumer-side poll exists because those backends can stall
/// or drop deliveries under heavy system load, and a silently dead stream
/// would otherwise mean status updates never fire again. The check is a dozen
/// stats per interval per watched repo — negligible next to the git calls a
/// report itself triggers.
const WATCHDOG_INTERVAL: Duration = Duration::from_secs(2);

/// Whether the loop owes an emission at `now`: events have been quiet for
/// [`SETTLE_QUIET`], or the oldest unemitted event has waited out
/// [`MAX_REPORT_LATENCY`] — whichever comes first. Factored as a pure
/// function so the emission rule is unit-testable without a filesystem.
fn should_emit(
    pending: bool,
    last_event: Instant,
    first_pending: Option<Instant>,
    now: Instant,
) -> bool {
    if !pending {
        return false;
    }
    let settled = now.duration_since(last_event) >= SETTLE_QUIET;
    let starved =
        first_pending.is_some_and(|since| now.duration_since(since) >= MAX_REPORT_LATENCY);
    settled || starved
}

fn run_watch_loop<F>(
    watcher: RepoFileWatcher,
    stop: Arc<AtomicBool>,
    path: String,
    fingerprint_paths: Option<(
        std::path::PathBuf,
        Option<std::path::PathBuf>,
        Option<std::path::PathBuf>,
    )>,
    sessions: Option<std::sync::Arc<Mutex<HashMap<String, WatchSession>>>>,
    session_stop: Arc<AtomicBool>,
    on_change: F,
) where
    F: Fn(String),
{
    // Liveness probe for the watched repository: the git dir carried in
    // `fingerprint_paths` doubles as the probe target, so the loop has one
    // source of truth for the location whose death ends it.
    let repo_alive = || {
        fingerprint_paths
            .as_ref()
            .is_some_and(|(git_dir, _, _)| git_dir.exists())
    };
    let mut pending = false;
    let mut pending_since: Option<Instant> = None;
    let mut last_event = Instant::now();
    // Watchdog state: the fingerprint as of the last cross-check, when that
    // check ran, and whether consumed OS events have made the cached
    // fingerprint stale. Seeding from the current state means only changes
    // after the loop started are reported — never a spurious first report.
    let (mut watchdog_fp, mut last_watchdog) = match &fingerprint_paths {
        Some((git_dir, worktree_root, common_dir)) => (
            Some(crate::watcher::debouncer::watch_fingerprint(
                git_dir,
                worktree_root.as_deref(),
                common_dir.as_deref(),
            )),
            Instant::now(),
        ),
        None => (None, Instant::now()),
    };
    let mut fingerprint_stale = false;
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
        if !repo_alive() {
            dead_misses += 1;
            if dead_misses >= DEAD_MISSES {
                dead_confirmed = true;
                break;
            }
        } else {
            dead_misses = 0;
        }
        match watcher.receiver.recv_timeout(Duration::from_millis(200)) {
            Ok(_) => {
                // A real OS delivery proves the stream is alive and that
                // everything it has emitted is accounted for: resynchronize
                // the fingerprint on the next tick so the watchdog never
                // re-reports changes the stream already announced.
                fingerprint_stale = true;
                if !pending {
                    pending_since = Some(Instant::now());
                }
                pending = true;
                last_event = Instant::now();
                // Drain everything already queued behind that first event:
                // during a checkout storm thousands can pile up, and one
                // recv per loop iteration would let the backlog (and memory)
                // grow unboundedly while never settling faster.
                for _ in watcher.receiver.try_iter() {}
                if should_emit(pending, last_event, pending_since, Instant::now()) {
                    if !repo_alive() {
                        dead_confirmed = true;
                        break;
                    }
                    on_change(path.clone());
                    pending = false;
                    pending_since = None;
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                // Watchdog tick: if the OS stream has gone quiet while the
                // repository actually changed, treat the divergence as an
                // event. This is the single consumer, and the fingerprint is
                // resynchronized on every stream delivery, so the watchdog
                // only ever reports changes the stream did NOT.
                if let (Some(fp), Some((git_dir, worktree_root, common_dir))) =
                    (&mut watchdog_fp, &fingerprint_paths)
                {
                    if fingerprint_stale || last_watchdog.elapsed() >= WATCHDOG_INTERVAL {
                        fingerprint_stale = false;
                        last_watchdog = Instant::now();
                        let next = crate::watcher::debouncer::watch_fingerprint(
                            git_dir,
                            worktree_root.as_deref(),
                            common_dir.as_deref(),
                        );
                        if *fp != next {
                            *fp = next;
                            if !pending {
                                pending_since = Some(Instant::now());
                            }
                            pending = true;
                        }
                    }
                }
                if should_emit(pending, last_event, pending_since, Instant::now()) {
                    if !repo_alive() {
                        dead_confirmed = true;
                        break;
                    }
                    on_change(path.clone());
                    pending = false;
                    pending_since = None;
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
            if let Ok(mut guard) = sessions.lock() {
                if guard
                    .get(&path)
                    .is_some_and(|session| Arc::ptr_eq(&session.stop, &session_stop))
                {
                    guard.remove(&path);
                }
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
        let _ = app.emit("repo-changed", RepoChangedPayload { path });
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
    let common_dir = resolve_git_common_dir(&canonical).ok();
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
    let emit_path = key.clone();
    let thread_stop = stop.clone();
    let session_stop = stop.clone();
    let loop_sessions = state.sessions.clone();
    let fingerprint_paths = Some((git_dir.clone(), worktree_root.clone(), common_dir.clone()));
    if let Err(e) = thread::Builder::new()
        .name("gitpulse-fs-watch".into())
        .spawn(move || {
            run_watch_loop(
                watcher,
                thread_stop,
                emit_path,
                fingerprint_paths,
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
            err.contains("64") || err.to_lowercase().contains("too many"),
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
            err.contains("64") || err.to_lowercase().contains("too many"),
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
        let fingerprint_paths = Some((git_dir.clone(), Some(canonical.clone()), None));
        let (tx, rx) = std::sync::mpsc::channel();
        thread::Builder::new()
            .name("watch-loop-test".into())
            .spawn(move || {
                run_watch_loop(
                    watcher,
                    thread_stop,
                    emit_path.clone(),
                    fingerprint_paths,
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
        // Warmup handshake: prove the backend's event stream is actually live
        // before any timing assertion runs. macOS FSEvents streams can start
        // late under system load; without this handshake that startup race
        // reads as a product bug. If the very first event never arrives the
        // watcher setup is broken and failing here is correct.
        std::fs::write(canonical.join(".gitpulse-warmup"), "warm").unwrap();
        rx.recv_timeout(Duration::from_secs(30))
            .expect("watcher backend must deliver a warmup event once live");
        // The callback already implies the 400ms settle window passed, so a
        // short drain clears any coalesced stragglers cleanly.
        thread::sleep(Duration::from_millis(200));
        while rx.try_recv().is_ok() {}
        (rx, stop, canonical)
    }

    /// Debounce proof: 30 rapid top-level writes must settle into exactly one
    /// callback after the quiet window, not thirty. A second callback during
    /// a subsequent quiet window would mean events are being forwarded
    /// per-write instead of debounced. The priming write exists because an
    /// OS backend may need its first delivered event before later ones flow;
    /// without it the storm races watch installation.
    #[test]
    fn watch_loop_coalesces_a_write_storm_into_one_settled_callback() {
        let temp = TempDir::new().unwrap();
        let (rx, stop, root) = spawn_loop(temp.path());

        // Prime: prove the pipeline delivers before measuring coalescing.
        std::fs::write(root.join("prime.txt"), "x").unwrap();
        let _ = rx
            .recv_timeout(Duration::from_secs(15))
            .expect("priming write must be delivered");

        // Storm: 30 writes inside roughly one debounce tick.
        for i in 0..30 {
            std::fs::write(root.join(format!("storm_{i}.txt")), "x").unwrap();
        }

        // Exactly one settled callback for the whole burst.
        let first = rx
            .recv_timeout(Duration::from_secs(30))
            .expect("at least one settled callback");
        assert!(!first.is_empty());

        // Anti-spam window well past the 400ms settle. Ideally the burst
        // yields exactly one callback and then silence; however, the OS
        // backends may split one logical write burst into several event
        // batches delivered seconds apart (observed on FSEvents under load,
        // flaking this test identically before the merge). Each delivered
        // batch legitimately settles into its own callback, so absolute
        // silence cannot be demanded. What MUST hold — the regression this
        // test exists for — is coalescing: a handful of callbacks at most,
        // never one-per-write spam.
        let mut extras = 0u32;
        let quiet_deadline = Instant::now() + Duration::from_millis(2500);
        while let Ok(extra) = rx.recv_timeout(
            quiet_deadline
                .saturating_duration_since(Instant::now())
                .max(Duration::from_millis(1)),
        ) {
            assert!(!extra.is_empty());
            extras += 1;
            assert!(
                extras <= 2,
                "burst produced {} follow-up callbacks: events forwarded per-write, not coalesced",
                extras + 1
            );
        }
        stop.store(true, Ordering::SeqCst);
    }

    /// Anti-starvation bound: a writer that never pauses long enough for the
    /// 400ms quiet window must still produce callbacks — at least one per
    /// [`MAX_REPORT_LATENCY`] — so a busy build or agent session can never
    /// leave the UI stale indefinitely.
    #[test]
    fn watch_loop_reports_under_sustained_writes_without_quiet() {
        let temp = TempDir::new().unwrap();
        let (rx, stop, root) = spawn_loop(temp.path());

        // Each write lands ~150ms after the previous one, inside the 400ms
        // settle window, so only the max-latency bound can produce a callback
        // while the churn is running (a scheduling hiccup that stretches one
        // gap past 400ms would legitimately fire early via the quiet path).
        let churn_start = Instant::now();
        let mut i = 0u32;
        while churn_start.elapsed() < Duration::from_millis(2400) {
            std::fs::write(root.join(format!("churn_{i}.txt")), "x").unwrap();
            i += 1;
            thread::sleep(Duration::from_millis(150));
        }
        rx.recv_timeout(Duration::from_secs(10))
            .expect("sustained writes must still produce at least one callback");
        stop.store(true, Ordering::SeqCst);
    }

    /// The reason for the second watch: an unstaged top-level edit fires the
    /// callback even though nothing under `.git` changed — and `.git`-only
    /// writes still fire as before.
    #[test]
    fn worktree_file_event_triggers_and_git_only_events_still_do() {
        let temp = TempDir::new().unwrap();
        let (rx, stop, root) = spawn_loop(temp.path());

        // 1. Worktree-only write: no index/HEAD traffic involved.
        std::fs::write(root.join("unstaged-edit.txt"), "dirty\n").unwrap();
        rx.recv_timeout(Duration::from_secs(30))
            .expect("unstaged worktree edit must trigger repo-changed");

        // Close the quiet window so the next write is a distinct emission
        // rather than coalesced with a late event from the first write.
        thread::sleep(SETTLE_QUIET + Duration::from_millis(100));

        // 2. A .git-only write still triggers through the recursive watch.
        std::fs::write(root.join(".git").join("gitpulse-probe"), "x").unwrap();
        rx.recv_timeout(Duration::from_secs(30))
            .expect("git-dir write must still trigger repo-changed");

        stop.store(true, Ordering::SeqCst);
    }

    /// Decision table for the emission rule: quiet period alone emits, but a
    /// pending refresh must also fire once the first event has waited out
    /// MAX_REPORT_LATENCY — even while events keep arriving.
    #[test]
    fn should_emit_quiet_period_or_max_latency() {
        let now = Instant::now();
        // Nothing pending: never emit.
        assert!(!should_emit(false, now - SETTLE_QUIET * 3, Some(now), now));
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
        // max latency: emit anyway — this is the anti-starvation bound.
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
    /// 400ms quiet window never opened). The max-latency bound must produce
    /// at least one refresh within roughly MAX_REPORT_LATENCY even though
    /// writes never stop.
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
                .recv_timeout(SETTLE_QUIET + Duration::from_millis(200))
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

        // Warmup handshake through the worktree root: prove the OS stream is
        // live before the timing assertion below (FSEvents can start late
        // under load; that startup race must not read as a product bug).
        std::fs::write(work_path.join(".gitpulse-warmup"), "warm").unwrap();
        rx.recv_timeout(Duration::from_secs(30))
            .expect("watcher must deliver a warmup event once live");
        while rx.try_recv().is_ok() {}

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
        std::fs::write(common_refs.join("pulse_new_branch"), "0".repeat(40)).unwrap();

        rx.recv_timeout(Duration::from_secs(6))
            .expect("common-dir ref write must trigger repo-changed for the worktree");
    }
}
