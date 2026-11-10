//! What a watch can truthfully say about a worktree: "nothing under it has
//! changed since epoch N".
//!
//! The worktree root is watched recursively on macOS, where FSEvents is
//! recursive in the kernel and the watcher backend only filters. Every event
//! that could move `git status` bumps [`WatchFacts::epoch`] on the backend's
//! own callback thread, before the debounce loop has seen it, so a read that
//! began before an event can never be mistaken for one that covers it: the
//! caller takes the epoch before it reads and keeps the answer under that
//! number, and any later event makes the numbers differ.
//!
//! The same callback drops events that cannot move status (git-internal
//! transients, writes under directories the repository ignores) before they
//! take a slot in the bounded event queue. Without that, one build in an
//! ignored directory fills the queue and the loop answers the overflow with a
//! full refresh, which is the cost this watch exists to remove.
//!
//! Unknown is never clean. [`WatchFacts::cover`] says nothing (`None`) unless
//! every one of these holds, and a caller that gets `None` reads as it always
//! did:
//! - the root watch is recursive (never on Linux or Windows, where the watch
//!   is per directory and bounded by descriptors);
//! - the watch has not ended;
//! - the ignore rules are loaded and no `.gitignore`, `info/exclude` or new
//!   directory has been seen since they were (a stale rule set could hide an
//!   event that now matters);
//! - the root directory is the one that was registered (FSEvents is created
//!   without `WatchRoot`, so a moved or replaced root sends no event; its
//!   inode is the only witness).
//!
//! A dropped or must-rescan event, a backend error and a full queue each bump
//! the epoch: whatever they hid is covered only by a read that starts after.

use super::{
    event_updates_ignore_rules, ignore_verdict, is_generated_state_noise_cached,
    is_git_internal_noise, IgnoreRules, IgnoreVerdict,
};
use crate::engine::git_reader::FileStatus;
use notify::{event::CreateKind, Event, EventKind, RecursiveMode};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::{Duration, Instant};

/// How the worktree root is registered. Recursive only where the backend's
/// recursion is a user-space filter over a kernel-recursive stream (FSEvents);
/// inotify and ReadDirectoryChangesW pay per directory or per tree.
pub(crate) const ROOT_WATCH_MODE: RecursiveMode = if cfg!(target_os = "macos") {
    RecursiveMode::Recursive
} else {
    RecursiveMode::NonRecursive
};

/// A kept status is dropped after this long even when nothing moved. The watch
/// sees the repository and its worktree; it cannot see a global excludes file,
/// `~/.gitconfig` or an attributes file elsewhere, which `git status` reads.
/// This bounds how long such an edit can go unseen, at 12 reads an hour for an
/// idle tab instead of 600.
pub(crate) const KEPT_STATUS_MAX_AGE: Duration = Duration::from_secs(300);
/// More changed files than this is not kept: the clone would cost more than it
/// saves, and a tree that dirty is rarely idle.
const MAX_KEPT_ROWS: usize = 4096;
/// `rules_loaded_for` before any rule set was published.
const NO_RULES: u64 = u64::MAX;

/// What the callback decided about one path.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PathClass {
    /// Moves nothing `git status` reads, and costs no epoch: git-internal
    /// transients and paths under a directory git confirmed as ignored.
    Inert,
    /// Not delivered to the loop (it never treated these as a reason to
    /// refresh), but not proof of no change either: generated-state
    /// directories can hold tracked files, and a build-looking name is only a
    /// name until git confirms it is ignored.
    Weak,
    /// Delivered, and an epoch.
    Signal,
}

struct Kept {
    epoch: u64,
    at: Instant,
    rows: Arc<Vec<FileStatus>>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct RootId {
    dev: u64,
    ino: u64,
}

#[cfg(unix)]
fn root_id(path: &Path) -> Option<RootId> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::symlink_metadata(path).ok()?;
    Some(RootId {
        dev: meta.dev(),
        ino: meta.ino(),
    })
}

#[cfg(not(unix))]
fn root_id(_path: &Path) -> Option<RootId> {
    None
}

pub(crate) struct WatchFacts {
    recursive: bool,
    epoch: AtomicU64,
    ended: AtomicBool,
    root: PathBuf,
    root_canonical: Option<PathBuf>,
    root_id: Option<RootId>,
    internal_roots: Vec<PathBuf>,
    rules: RwLock<Option<Arc<IgnoreRules>>>,
    /// Bumped by the callback on anything that could change what the rules
    /// say; the loop reads it before a load and publishes the value it read.
    rules_wanted: AtomicU64,
    rules_loaded_for: AtomicU64,
    kept: Mutex<Option<Kept>>,
    /// Paths the callback classified; the bound a test holds a storm to.
    examined: AtomicU64,
}

impl WatchFacts {
    pub(crate) fn new(
        root: Option<&Path>,
        internal_roots: Vec<PathBuf>,
        root_mode: RecursiveMode,
    ) -> Arc<Self> {
        let recursive = root.is_some() && root_mode == RecursiveMode::Recursive;
        let root_path = root.map(Path::to_path_buf).unwrap_or_default();
        Arc::new(Self {
            recursive,
            epoch: AtomicU64::new(0),
            ended: AtomicBool::new(false),
            root_canonical: root.and_then(|root| root.canonicalize().ok()),
            root_id: root.and_then(root_id),
            root: root_path,
            internal_roots,
            rules: RwLock::new(None),
            rules_wanted: AtomicU64::new(0),
            rules_loaded_for: AtomicU64::new(NO_RULES),
            kept: Mutex::new(None),
            examined: AtomicU64::new(0),
        })
    }

    /// The watch is over; nothing it said before may be relied on after.
    pub(crate) fn end(&self) {
        self.ended.store(true, Ordering::Release);
    }

    pub(crate) fn bump(&self) {
        self.epoch.fetch_add(1, Ordering::AcqRel);
    }

    #[cfg(test)]
    pub(crate) fn examined(&self) -> u64 {
        self.examined.load(Ordering::Relaxed)
    }

    /// The count the loop reads before it loads rules.
    pub(crate) fn rules_wanted(&self) -> u64 {
        self.rules_wanted.load(Ordering::Acquire)
    }

    /// Rules loaded after `wanted` was read cover everything up to it. A change
    /// that arrived while the load ran leaves them stale and the loop loads
    /// again.
    pub(crate) fn publish_rules(&self, rules: Arc<IgnoreRules>, wanted: u64) {
        *self.rules.write().unwrap_or_else(PoisonError::into_inner) = Some(rules);
        self.rules_loaded_for.store(wanted, Ordering::Release);
    }

    pub(crate) fn rules_stale(&self) -> bool {
        self.rules_loaded_for.load(Ordering::Acquire) != self.rules_wanted.load(Ordering::Acquire)
    }

    /// The epoch a read that starts now would be covered by, or `None` when
    /// this watch cannot vouch for the worktree. See the module docs.
    pub(crate) fn cover(&self) -> Option<u64> {
        if !self.recursive || self.ended.load(Ordering::Acquire) {
            return None;
        }
        if self.rules_loaded_for.load(Ordering::Acquire) == NO_RULES || self.rules_stale() {
            return None;
        }
        if root_id(&self.root)? != self.root_id? {
            return None;
        }
        Some(self.epoch.load(Ordering::Acquire))
    }

    /// Keeps `rows`, the answer of a read that began when `cover()` said
    /// `epoch`.
    pub(crate) fn keep(&self, epoch: u64, rows: &[FileStatus], now: Instant) {
        let mut slot = self.kept.lock().unwrap_or_else(PoisonError::into_inner);
        *slot = (rows.len() <= MAX_KEPT_ROWS).then(|| Kept {
            epoch,
            at: now,
            rows: Arc::new(rows.to_vec()),
        });
    }

    /// The kept rows, when they were read at `epoch` and are not too old.
    pub(crate) fn kept(&self, epoch: u64, now: Instant) -> Option<Arc<Vec<FileStatus>>> {
        let slot = self.kept.lock().unwrap_or_else(PoisonError::into_inner);
        slot.as_ref()
            .filter(|kept| {
                kept.epoch == epoch && now.saturating_duration_since(kept.at) < KEPT_STATUS_MAX_AGE
            })
            .map(|kept| Arc::clone(&kept.rows))
    }

    /// The backend callback's gate: whether `res` is delivered to the loop.
    /// Runs on the backend's thread, so it spawns nothing and takes only the
    /// rule set's read lock.
    pub(crate) fn admit(&self, res: &Result<Event, notify::Error>) -> bool {
        if !self.recursive {
            return true;
        }
        let event = match res {
            Ok(event) => event,
            // A backend error says events may have been lost.
            Err(_) => {
                self.bump();
                return true;
            }
        };
        if event.need_rescan() || event.paths.is_empty() {
            self.bump();
            return true;
        }
        // Reads move nothing; the loop discards them too.
        if matches!(event.kind, EventKind::Access(_)) {
            return true;
        }
        if event_updates_ignore_rules(event) {
            self.rules_wanted.fetch_add(1, Ordering::AcqRel);
        }
        let rules = self
            .rules
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let mut deliver = false;
        let mut moved = false;
        for path in &event.paths {
            self.examined.fetch_add(1, Ordering::Relaxed);
            match self.classify(path, rules.as_deref()) {
                PathClass::Inert => {}
                PathClass::Weak => moved = true,
                PathClass::Signal => {
                    deliver = true;
                    moved = true;
                    // A directory that appears may be one the rules should
                    // already cover; its contents are not filtered until they
                    // are reloaded.
                    if matches!(event.kind, EventKind::Create(CreateKind::Folder))
                        && !self
                            .internal_roots
                            .iter()
                            .any(|root| path.starts_with(root))
                    {
                        self.rules_wanted.fetch_add(1, Ordering::AcqRel);
                    }
                }
            }
        }
        if moved {
            self.bump();
        }
        deliver
    }

    fn classify(&self, path: &Path, rules: Option<&IgnoreRules>) -> PathClass {
        if is_git_internal_noise(path, &self.internal_roots) {
            return PathClass::Inert;
        }
        if self
            .internal_roots
            .iter()
            .any(|root| path.starts_with(root))
        {
            return PathClass::Signal;
        }
        let relative = path.strip_prefix(&self.root).ok().or_else(|| {
            self.root_canonical
                .as_deref()
                .and_then(|root| path.strip_prefix(root).ok())
        });
        let Some(relative) = relative else {
            // An alias this cheap test cannot resolve: the loop decides.
            return PathClass::Signal;
        };
        if relative.as_os_str().is_empty() {
            // The root itself: FSEvents reports it for a coalesced storm and
            // when the directory is renamed away. The loop never refreshes on
            // it, but it must wake the loop, which is how a vanished
            // repository is noticed within a second instead of at the next
            // heartbeat, and it names no file, so it proves nothing.
            return PathClass::Signal;
        }
        if is_generated_state_noise_cached(path, &self.root, self.root_canonical.as_deref()) {
            return PathClass::Weak;
        }
        match ignore_verdict(relative, rules) {
            IgnoreVerdict::Confirmed => PathClass::Inert,
            IgnoreVerdict::Named => PathClass::Weak,
            IgnoreVerdict::Not => PathClass::Signal,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{DataChange, ModifyKind};
    use std::collections::HashSet;

    fn rules(prefixes: &[&str]) -> Arc<IgnoreRules> {
        Arc::new(IgnoreRules {
            prefixes: prefixes
                .iter()
                .map(|p| (*p).to_string())
                .collect::<HashSet<_>>(),
            load_error: None,
            truncated: false,
        })
    }

    fn facts(root: &Path) -> Arc<WatchFacts> {
        WatchFacts::new(
            Some(root),
            vec![root.join(".git")],
            RecursiveMode::Recursive,
        )
    }

    fn ready(facts: &WatchFacts, prefixes: &[&str]) {
        facts.publish_rules(rules(prefixes), facts.rules_wanted());
    }

    fn write(path: &Path) -> Result<Event, notify::Error> {
        Ok(
            Event::new(EventKind::Modify(ModifyKind::Data(DataChange::Content)))
                .add_path(path.to_path_buf()),
        )
    }

    #[test]
    fn a_nested_edit_is_delivered_and_moves_the_epoch() {
        let temp = tempfile::TempDir::new().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let facts = facts(&root);
        ready(&facts, &[]);
        let before = facts.cover().expect("recursive and loaded");
        assert!(facts.admit(&write(&root.join("src/deep/er/file.rs"))));
        assert_eq!(facts.cover(), Some(before + 1));
    }

    #[test]
    fn a_write_under_a_confirmed_ignored_directory_is_dropped_without_an_epoch() {
        let temp = tempfile::TempDir::new().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let facts = facts(&root);
        ready(&facts, &["scratch"]);
        let before = facts.cover().unwrap();
        assert!(!facts.admit(&write(&root.join("scratch/a/b.txt"))));
        assert_eq!(facts.cover(), Some(before));
    }

    /// A name that merely looks like build output is not a reason to trust
    /// that nothing changed: it is dropped from the queue, but it costs an
    /// epoch because git has not said it is ignored.
    #[test]
    fn a_build_looking_name_git_did_not_confirm_is_dropped_but_costs_an_epoch() {
        let temp = tempfile::TempDir::new().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let facts = facts(&root);
        ready(&facts, &[]);
        let before = facts.cover().unwrap();
        assert!(!facts.admit(&write(&root.join("target/debug/x.o"))));
        assert_eq!(facts.cover(), Some(before + 1));
        ready(&facts, &["target"]);
        let after = facts.cover().unwrap();
        assert!(!facts.admit(&write(&root.join("target/debug/y.o"))));
        assert_eq!(facts.cover(), Some(after));
    }

    /// A renamed-away repository is noticed because its root's event wakes the
    /// loop; dropping it would leave the dead watch asleep until the heartbeat.
    #[test]
    fn an_event_for_the_root_itself_is_delivered_and_costs_an_epoch() {
        let temp = tempfile::TempDir::new().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let facts = facts(&root);
        ready(&facts, &[]);
        let before = facts.cover().unwrap();
        assert!(facts.admit(&write(&root)));
        assert_eq!(facts.cover(), Some(before + 1));
    }

    #[test]
    fn git_internal_transients_cost_no_epoch_but_a_ref_write_does() {
        let temp = tempfile::TempDir::new().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let facts = facts(&root);
        ready(&facts, &[]);
        let before = facts.cover().unwrap();
        assert!(!facts.admit(&write(&root.join(".git/index.lock"))));
        assert_eq!(facts.cover(), Some(before));
        assert!(facts.admit(&write(&root.join(".git/refs/heads/main"))));
        assert_eq!(facts.cover(), Some(before + 1));
    }

    #[test]
    fn the_watch_says_nothing_before_rules_load_and_while_they_are_stale() {
        let temp = tempfile::TempDir::new().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let facts = facts(&root);
        assert_eq!(facts.cover(), None, "rules never loaded");
        ready(&facts, &[]);
        assert!(facts.cover().is_some());
        assert!(facts.admit(&write(&root.join(".gitignore"))));
        assert_eq!(facts.cover(), None, "a rule file changed after the load");
        ready(&facts, &[]);
        assert!(facts.cover().is_some());
    }

    #[test]
    fn a_non_recursive_watch_never_vouches_and_delivers_everything() {
        let temp = tempfile::TempDir::new().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let facts = WatchFacts::new(
            Some(&root),
            vec![root.join(".git")],
            RecursiveMode::NonRecursive,
        );
        ready(&facts, &[]);
        assert_eq!(facts.cover(), None);
        assert!(facts.admit(&write(&root.join("target/debug/x.o"))));
    }

    #[test]
    fn an_ended_watch_and_a_replaced_root_say_nothing() {
        let temp = tempfile::TempDir::new().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let facts = facts(&root);
        ready(&facts, &[]);
        assert!(facts.cover().is_some());
        facts.end();
        assert_eq!(facts.cover(), None);

        let moved = temp.path().join("moved");
        let replaced_root = temp.path().join("wt");
        std::fs::create_dir(&replaced_root).unwrap();
        let replaced = WatchFacts::new(
            Some(&replaced_root),
            vec![replaced_root.join(".git")],
            RecursiveMode::Recursive,
        );
        ready(&replaced, &[]);
        assert!(replaced.cover().is_some());
        std::fs::rename(&replaced_root, &moved).unwrap();
        assert_eq!(replaced.cover(), None, "the root moved away");
        std::fs::create_dir(&replaced_root).unwrap();
        assert_eq!(
            replaced.cover(),
            None,
            "a different directory now stands at the same path"
        );
    }

    #[test]
    fn a_rescan_an_error_and_an_eventless_wake_each_cost_an_epoch() {
        let temp = tempfile::TempDir::new().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let facts = facts(&root);
        ready(&facts, &[]);
        let start = facts.cover().unwrap();
        let rescan = Event::new(EventKind::Other).set_flag(notify::event::Flag::Rescan);
        assert!(facts.admit(&Ok(rescan)));
        assert!(facts.admit(&Err(notify::Error::generic("lost"))));
        assert!(facts.admit(&Ok(Event::new(EventKind::Any))));
        assert_eq!(facts.cover(), Some(start + 3));
    }

    /// The race the epoch exists for: an event that lands while a read is
    /// running must not be covered by that read.
    #[test]
    fn a_read_in_flight_when_an_event_arrives_does_not_cover_it() {
        let temp = tempfile::TempDir::new().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let facts = facts(&root);
        ready(&facts, &[]);
        let now = Instant::now();
        let epoch = facts.cover().unwrap();
        // The read starts here; the file changes while it runs.
        assert!(facts.admit(&write(&root.join("late.txt"))));
        facts.keep(epoch, &[], now);
        assert!(facts.kept(facts.cover().unwrap(), now).is_none());
    }

    #[test]
    fn a_kept_read_is_served_only_at_its_own_epoch_and_only_while_young() {
        let temp = tempfile::TempDir::new().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let facts = facts(&root);
        ready(&facts, &[]);
        let now = Instant::now();
        let epoch = facts.cover().unwrap();
        facts.keep(epoch, &[], now);
        assert!(facts.kept(epoch, now).is_some());
        assert!(facts.kept(epoch + 1, now).is_none());
        assert!(facts.kept(epoch, now + KEPT_STATUS_MAX_AGE).is_none());
    }

    /// 10,000 writes under an ignored directory: every one is classified once,
    /// none reaches the loop's queue, none costs an epoch, and nothing spawns
    /// (classification is a hash lookup; this function has no process API).
    #[test]
    fn ten_thousand_ignored_writes_are_dropped_with_one_classification_each() {
        let temp = tempfile::TempDir::new().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let facts = facts(&root);
        ready(&facts, &["out"]);
        let before = facts.cover().unwrap();
        let mut delivered = 0u32;
        for i in 0..10_000u32 {
            let path = root
                .join("out")
                .join(format!("d{}", i % 50))
                .join(format!("f{i}.o"));
            if facts.admit(&write(&path)) {
                delivered += 1;
            }
        }
        assert_eq!(delivered, 0);
        assert_eq!(facts.examined(), 10_000, "one classification per path");
        assert_eq!(facts.cover(), Some(before));
    }
}
