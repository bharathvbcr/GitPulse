//! The budget for git subcommands that run repository hooks, and the cancel
//! switch the UI holds while one is running.
//!
//! A hook is the repository owner's program, not git: a `pre-commit` that
//! lints and tests, a `commit-msg` that calls a service, `git-lfs` behind
//! `pre-push`. Those legitimately outlast [`super::DEFAULT_TIMEOUT`], which
//! is sized for plumbing, and killing them there surfaced as "git commit
//! timed out" with nothing to say a hook was what ran. Hook-running
//! subcommands therefore get [`HOOK_TIMEOUT`]: still finite, so a hook that
//! never returns does not pin the repository's mutation lock forever, and
//! cancellable by the user through [`cancel`] long before it elapses.

use super::ProcessObserver;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Duration;

/// Deadline for a hook-running git subcommand (see [`hooks_run_by`]): 20
/// minutes. Long enough for a pre-commit that runs a full lint and test
/// pass; short enough that a hook waiting on input it will never get is
/// eventually reported instead of holding the repository. Network commands
/// keep their own, longer [`super::NETWORK_TIMEOUT`].
pub const HOOK_TIMEOUT: Duration = Duration::from_secs(20 * 60);

/// The phrase an error carries when the user cancelled a hook-running
/// command. [`is_cancelled`] matches it; nothing else produces it.
const CANCELLED_MARKER: &str = " was cancelled while running ";

/// Hooks each hook-running subcommand can invoke, in the order git runs
/// them. `None` for every subcommand that runs none.
///
/// `cherry-pick`, `revert` and `rebase` commit through git's sequencer,
/// which runs the message hooks but not `pre-commit`; `am` has its own.
pub(crate) fn hooks_run_by(sub: &str) -> Option<&'static [&'static str]> {
    Some(match sub {
        "commit" => &[
            "pre-commit",
            "prepare-commit-msg",
            "commit-msg",
            "post-commit",
            "post-rewrite",
        ],
        "merge" => &[
            "pre-merge-commit",
            "prepare-commit-msg",
            "commit-msg",
            "post-merge",
        ],
        "pull" => &[
            "pre-merge-commit",
            "prepare-commit-msg",
            "commit-msg",
            "post-merge",
            "post-rewrite",
        ],
        "rebase" => &[
            "pre-rebase",
            "prepare-commit-msg",
            "post-commit",
            "post-rewrite",
            "post-checkout",
        ],
        "cherry-pick" | "revert" => &["prepare-commit-msg", "commit-msg", "post-commit"],
        "am" => &[
            "applypatch-msg",
            "pre-applypatch",
            "post-applypatch",
            "post-rewrite",
        ],
        "push" => &["pre-push"],
        _ => return None,
    })
}

/// The deadline a caller that did not choose one gets for `sub`: the hook
/// budget for a hook-running subcommand, the plumbing default otherwise.
pub(crate) fn default_timeout_for(sub: &str) -> Duration {
    if hooks_run_by(sub).is_some() {
        hook_timeout()
    } else {
        super::DEFAULT_TIMEOUT
    }
}

fn hook_timeout() -> Duration {
    #[cfg(test)]
    if let Some(forced) = HOOK_TIMEOUT_OVERRIDE.get() {
        return forced;
    }
    HOOK_TIMEOUT
}

#[cfg(test)]
thread_local! {
    static HOOK_TIMEOUT_OVERRIDE: std::cell::Cell<Option<Duration>> =
        const { std::cell::Cell::new(None) };
}

/// Runs `body` with this thread's hook budget shortened to `budget`, so a
/// test can reach the deadline without waiting out twenty minutes.
#[cfg(test)]
pub(crate) fn with_hook_timeout<T>(budget: Duration, body: impl FnOnce() -> T) -> T {
    HOOK_TIMEOUT_OVERRIDE.set(Some(budget));
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            HOOK_TIMEOUT_OVERRIDE.set(None);
        }
    }
    let _reset = Reset;
    body()
}

/// True when `err` is a hook-running command the user cancelled.
pub fn is_cancelled(err: &str) -> bool {
    err.contains(CANCELLED_MARKER)
}

/// Where `repo`'s hooks live and which of `sub`'s hooks are installed there.
/// `None` when the directory could not be resolved; an empty list when it
/// was, and none of them is an executable file.
fn installed_hooks(repo: &Path, sub: &str) -> Option<(PathBuf, Vec<&'static str>)> {
    let candidates = hooks_run_by(sub)?;
    // `--git-path hooks` honours `core.hooksPath` and linked worktrees, which
    // is where git itself looks; guessing `.git/hooks` would miss husky.
    let raw = super::git_text(repo, &["rev-parse", "--git-path", "hooks"]).ok()?;
    let dir = PathBuf::from(raw.trim());
    let dir = if dir.is_absolute() {
        dir
    } else {
        repo.join(dir)
    };
    let installed = candidates
        .iter()
        .copied()
        .filter(|name| is_runnable(&dir.join(name)))
        .collect();
    Some((dir, installed))
}

#[cfg(unix)]
fn is_runnable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_runnable(path: &Path) -> bool {
    path.is_file()
}

/// What the user can act on, appended to a timeout or cancel of `sub`.
fn hooks_clause(repo: &Path, sub: &str) -> String {
    match installed_hooks(repo, sub) {
        Some((dir, installed)) if !installed.is_empty() => format!(
            "its {} hook{} ({}) in {}",
            installed.join(", "),
            if installed.len() == 1 { "" } else { "s" },
            if installed.len() == 1 {
                "installed"
            } else {
                "all installed"
            },
            dir.display()
        ),
        Some((dir, _)) => format!(
            "git itself: none of the hooks git {sub} runs is installed in {}",
            dir.display()
        ),
        None => "its hooks (the hooks directory could not be read)".to_string(),
    }
}

/// The error for a hook-running command killed at its deadline. Keeps the
/// runner's `"{label} timed out after {n}s"` sentence first, which is what
/// every timeout classifier matches, then names the hook.
pub(crate) fn timeout_message(repo: &Path, sub: &str, runner_message: &str) -> String {
    format!(
        "{runner_message} while running {}. Hook-running commands are stopped after {}s; \
         a hook waiting for input it will never get is the usual cause.",
        hooks_clause(repo, sub),
        hook_timeout().as_secs()
    )
}

/// The error for a hook-running command the user cancelled. The command was
/// stopped mid-run, so the repository may hold part of what it did.
pub(crate) fn cancelled_message(repo: &Path, sub: &str) -> String {
    format!(
        "git {sub}{CANCELLED_MARKER}{}. Review the working tree before retrying.",
        hooks_clause(repo, sub)
    )
}

/// Each in-flight run's id and its cancel switch.
type RunSwitches = Vec<(u64, Arc<AtomicBool>)>;

#[derive(Default)]
struct Registry {
    next: AtomicU64,
    runs: Mutex<HashMap<PathBuf, RunSwitches>>,
}

fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(Registry::default)
}

/// One hook-running child in `repo`, registered for [`cancel`] while it
/// lives. Each run owns its own switch, so a cancel reaches only the runs in
/// flight when it was asked for, never one that starts afterwards.
pub(crate) struct HookedRun {
    repo: PathBuf,
    id: u64,
    flag: Arc<AtomicBool>,
}

impl HookedRun {
    pub(crate) fn enter(repo: &Path) -> Self {
        let registry = registry();
        let id = registry.next.fetch_add(1, Ordering::Relaxed);
        let flag = Arc::new(AtomicBool::new(false));
        registry
            .runs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(repo.to_path_buf())
            .or_default()
            .push((id, flag.clone()));
        Self {
            repo: repo.to_path_buf(),
            id,
            flag,
        }
    }
}

impl Drop for HookedRun {
    fn drop(&mut self) {
        let mut runs = registry()
            .runs
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(list) = runs.get_mut(&self.repo) {
            list.retain(|(id, _)| *id != self.id);
            if list.is_empty() {
                runs.remove(&self.repo);
            }
        }
    }
}

impl ProcessObserver for HookedRun {
    fn cancelled(&self) -> bool {
        self.flag.load(Ordering::Relaxed)
    }
}

/// Stops every hook-running git command in flight in `repo` (a canonical
/// path, as [`super::validate_repo`] returns it), process group and all, so
/// the hook goes with it. Returns how many were signalled; zero means there
/// was nothing to cancel, which the caller reports rather than pretending.
pub fn cancel(repo: &Path) -> usize {
    let runs = registry()
        .runs
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    runs.get(repo).map_or(0, |list| {
        for (_, flag) in list {
            flag.store(true, Ordering::Relaxed);
        }
        list.len()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hook_running_subcommands_get_the_hook_budget_and_plumbing_does_not() {
        for sub in [
            "commit",
            "merge",
            "rebase",
            "am",
            "cherry-pick",
            "revert",
            "pull",
            "push",
        ] {
            assert_eq!(default_timeout_for(sub), HOOK_TIMEOUT, "{sub}");
        }
        for sub in [
            "merge-base",
            "commit-tree",
            "rev-parse",
            "status",
            "",
            "log",
        ] {
            assert_eq!(
                default_timeout_for(sub),
                super::super::DEFAULT_TIMEOUT,
                "{sub}"
            );
        }
        assert!(HOOK_TIMEOUT > super::super::DEFAULT_TIMEOUT);
        assert!(HOOK_TIMEOUT < super::super::NETWORK_TIMEOUT);
    }

    #[test]
    fn a_cancel_reaches_only_runs_in_flight_in_that_repository() {
        let here = Path::new("/hooks-test/here");
        let there = Path::new("/hooks-test/there");
        assert_eq!(cancel(here), 0, "nothing registered yet");
        let a = HookedRun::enter(here);
        let b = HookedRun::enter(there);
        assert_eq!(cancel(here), 1);
        assert!(a.cancelled());
        assert!(!b.cancelled());
        let later = HookedRun::enter(here);
        assert!(
            !later.cancelled(),
            "a run started after the cancel is not cancelled"
        );
        drop((a, b, later));
        assert_eq!(cancel(here), 0, "finished runs unregister");
    }
}
