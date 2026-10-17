//! The one door `benches/watcher.rs` uses to reach the event classifier.
//!
//! `record_event_signal` is the per-event decision inside the debounce loop
//! (does this filesystem event name a change worth a refresh, and of what
//! class), and `IgnoreRules` is the repository-specific half of it. Both are
//! private on purpose: nothing outside this module should classify events
//! except the watch loop. A bench is a separate crate, so without a door it
//! could measure neither, and a classifier that regresses from linear to
//! quadratic in the ignore list would be found by a user.
//!
//! This builds the same `IgnoreRules` the loop builds (from `git ls-files -o -i
//! --directory -z` output, through the same parser) and calls the same
//! function; it adds no logic of its own.
//!
//! [`start_idle_watch`] is the second door, for `benches/idle_wakeups.rs`: the
//! public `start_watch` needs a real window system's `AppHandle`, which a bench
//! does not have, and what that bench measures is the loop itself. It starts
//! the same session `start_watch` does, with callbacks that drop what they are
//! told.

use std::path::{Path, PathBuf};

use notify::Event;

use super::{
    parse_ignored_directories, record_event_signal, start_watch_observed, ChangeAccumulator,
    IgnoreRules, WatcherState,
};

/// Starts a real watch session on `repo_path` whose announcements go nowhere.
pub fn start_idle_watch(state: &WatcherState, repo_path: &str) -> Result<String, String> {
    start_watch_observed(state, repo_path.to_owned(), |_| {}, |_| {})
}

/// A worktree's classifier: its ignore rules and the path facts the loop holds.
pub struct StormClassifier {
    rules: IgnoreRules,
    internal_roots: Vec<PathBuf>,
    worktree: PathBuf,
    worktree_canonical: Option<PathBuf>,
}

/// What classifying one burst did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StormOutcome {
    /// Events the classifier admitted as signal.
    pub admitted: u64,
    /// Events it dropped as noise.
    pub dropped: u64,
    /// Distinct changed paths one debounce window recorded (bounded).
    pub paths_recorded: u64,
    /// Distinct change classes the window recorded.
    pub kinds: u64,
    /// The window's path list hit its bound.
    pub truncated: bool,
}

impl StormClassifier {
    /// `ignored_directories_z` is `git ls-files -o -i --directory --exclude-standard -z`
    /// output. `ignored_prefixes` reports how many the parser kept.
    pub fn new(worktree: &Path, git_dir: &Path, ignored_directories_z: &str) -> (Self, usize) {
        let (prefixes, truncated) = parse_ignored_directories(ignored_directories_z);
        let kept = prefixes.len();
        let rules = IgnoreRules {
            prefixes,
            load_error: None,
            truncated,
        };
        (
            Self {
                rules,
                internal_roots: vec![git_dir.to_path_buf()],
                worktree: worktree.to_path_buf(),
                worktree_canonical: worktree.canonicalize().ok(),
            },
            kept,
        )
    }

    /// Classifies `events` as one debounce window.
    pub fn classify(&self, events: &[Event]) -> StormOutcome {
        let mut window = ChangeAccumulator::default();
        let mut admitted = 0u64;
        for event in events {
            if record_event_signal(
                event,
                &self.internal_roots,
                &self.worktree,
                self.worktree_canonical.as_deref(),
                &self.rules,
                &mut window,
            ) {
                admitted += 1;
            }
        }
        StormOutcome {
            admitted,
            dropped: events.len() as u64 - admitted,
            paths_recorded: window.paths.len() as u64,
            kinds: window.kinds.len() as u64,
            truncated: window.truncated,
        }
    }
}
