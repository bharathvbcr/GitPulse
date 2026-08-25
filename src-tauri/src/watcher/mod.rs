pub mod debouncer;

pub use debouncer::RepoFileWatcher;

use crate::engine::git_cli::{resolve_git_dir, validate_repo};
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
}

impl Drop for WatchSession {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

#[derive(Default)]
pub struct WatcherState {
    sessions: Mutex<HashMap<String, WatchSession>>,
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
) -> Result<Option<Arc<AtomicBool>>, String> {
    if sessions.contains_key(key) {
        return Ok(None);
    }
    if sessions.len() >= MAX_WATCHES {
        return Err(format!("Too many watched repositories (max {MAX_WATCHES})"));
    }
    let stop = Arc::new(AtomicBool::new(false));
    sessions.insert(key.to_string(), WatchSession { stop: stop.clone() });
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

fn run_watch_loop<F>(watcher: RepoFileWatcher, stop: Arc<AtomicBool>, path: String, on_change: F)
where
    F: Fn(String),
{
    let mut pending = false;
    let mut last_event = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        match watcher.receiver.recv_timeout(Duration::from_millis(200)) {
            Ok(_) => {
                pending = true;
                last_event = Instant::now();
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if pending && last_event.elapsed() >= Duration::from_millis(400) {
                    on_change(path.clone());
                    pending = false;
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
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

    {
        let guard = state.lock_sessions()?;
        if guard.contains_key(&key) {
            return Ok(key);
        }
        if guard.len() >= MAX_WATCHES {
            return Err(format!("Too many watched repositories (max {MAX_WATCHES})"));
        }
    }

    let watcher = RepoFileWatcher::watch(&git_dir)?;

    let stop = {
        let mut guard = state.lock_sessions()?;
        match insert_watch_session(&mut guard, &key)? {
            None => return Ok(key),
            Some(stop) => stop,
        }
    };

    // Release the sessions mutex before spawn. Holding it across spawn can
    // deadlock if the watch thread (or `on_change`) needs the same lock.
    let emit_path = key.clone();
    let thread_stop = stop.clone();
    if let Err(e) = thread::Builder::new()
        .name("gitpulse-fs-watch".into())
        .spawn(move || {
            run_watch_loop(watcher, thread_stop, emit_path, on_change);
        })
    {
        let _ = state.abandon_watch_slot(&key, &stop);
        return Err(format!("Failed to start watcher: {}", e));
    }
    Ok(key)
}

pub fn unwatch(state: &WatcherState, repo_path: String) -> Result<(), String> {
    let keys = watch_lookup_keys(&repo_path);
    let mut guard = state.lock_sessions()?;
    for key in keys {
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
            Ok(insert_watch_session(&mut guard, key)?.is_some())
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
}
