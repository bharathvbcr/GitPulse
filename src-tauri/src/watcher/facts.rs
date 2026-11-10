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
//! transients, writes under directories git confirmed as ignored) before they
//! take a slot in the bounded event queue. Without that, one build in an
//! ignored directory fills the queue and the loop answers the overflow with a
//! full refresh, which is the cost this watch exists to remove.
//!
//! # Unknown is never clean
//!
//! [`WatchFacts::cover_at`] says nothing (`None`) unless every one of these
//! holds, and a caller that gets `None` reads as it always did:
//! - the root watch is recursive (never on Linux or Windows, where the watch
//!   is per directory and bounded by descriptors);
//! - the watch has not ended;
//! - the ignore rules are loaded and nothing that can change them has been
//!   seen since: a `.gitignore` or `info/exclude` edit, a new directory, or a
//!   write to this checkout's own index or `HEAD`. The rule set is the answer
//!   of `git ls-files -o -i --directory`, which depends on what is tracked, so
//!   a file that becomes tracked inside an ignored directory (`git add -f`,
//!   `checkout`, `stash pop`, `read-tree`) makes it wrong;
//! - the root directory is the one that was registered (FSEvents is created
//!   without `WatchRoot`, so a moved or replaced root sends no event; its
//!   inode is the only witness);
//! - the watch is not convicted.
//!
//! A dropped or must-rescan event, a backend error and a full queue each bump
//! the epoch: whatever they hid is covered only by a read that starts after.
//!
//! # A watch has to earn its trust
//!
//! A stream can stay registered and deliver nothing (this host's `fseventsd`
//! has wedged). Nothing in the epoch can show that, so two independent
//! witnesses are checked and either one makes the watch *suspect*:
//! - **identity**: with each kept read the stat identity (mtime, size, inode)
//!   of this checkout's own `index` and `HEAD` is recorded, the files git
//!   itself rewrites. Before a kept read is served they are stat-ed again; a
//!   difference with an unchanged epoch means an event was missed. A few
//!   stats, no spawn.
//! - **disagreement**: a real read made at an unchanged epoch (the cap
//!   expired, or the identity check refused the kept read) that returns a
//!   different answer from the kept one means a change went unseen.
//!
//! A suspect watch is given [`SUSPECT_GRACE`] for the event to arrive (the
//! stream's own latency is milliseconds, but it is not zero). If the epoch has
//! moved by then the suspicion is dropped; if it has not, the watch is
//! *convicted*: it is logged once, the kept read is discarded, and `cover_at`
//! says nothing, so the poll reads every time as it did before this watch
//! existed. A conviction lifts when delivery is proven again: a later read
//! differs from the one before it AND the epoch moved between them (an event
//! arrived for a change a read then confirmed), or the watch is re-registered
//! (a new `WatchFacts`).

use super::{
    event_updates_ignore_rules, ignore_verdict, is_generated_state_noise_cached,
    is_git_internal_noise, IgnoreRules, IgnoreVerdict,
};
use crate::engine::git_reader::FileStatus;
use notify::{event::CreateKind, Event, EventKind, RecursiveMode};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::{Duration, Instant, UNIX_EPOCH};

/// How the worktree root is registered. Recursive only where the backend's
/// recursion is a user-space filter over a kernel-recursive stream (FSEvents);
/// inotify and ReadDirectoryChangesW pay per directory or per tree.
pub(crate) const ROOT_WATCH_MODE: RecursiveMode = if cfg!(target_os = "macos") {
    RecursiveMode::Recursive
} else {
    RecursiveMode::NonRecursive
};

/// A kept status is dropped after this long even when nothing moved. The watch
/// sees the repository and its worktree; it cannot see a global excludes file
/// or `~/.gitconfig`, which `git status` reads, and it cannot tell a stream
/// that went quiet from a repository that did. This bounds how long such a
/// change can go unseen, and is the longest a plain edit under a dead stream
/// stays hidden before the disagreement it causes convicts the watch.
///
/// The trade: an idle tab costs one `git status` per minute instead of ten (the
/// poll is 6 s) or the one in 300 s a longer cap would give, and the worst
/// case for a stream that silently died is a minute of stale rows instead of
/// five. Shorter would spend most of what the watch saves; longer is a
/// stale-clean a person can notice.
pub(crate) const KEPT_STATUS_MAX_AGE: Duration = Duration::from_secs(60);
/// How long a suspected missed event is given to arrive before the watch is
/// convicted. FSEvents is created with no latency (`NoDefer`, 0.0); this is
/// three orders of magnitude over its normal delivery.
pub(crate) const SUSPECT_GRACE: Duration = Duration::from_secs(3);
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
    /// refresh), but not proof of no change either: a generated-state
    /// directory git has not confirmed as ignored can hold tracked files, and
    /// a build-looking name is only a name until git confirms it.
    Weak,
    /// Delivered, and an epoch.
    Signal,
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

/// Stat identity of one file git rewrites.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct FileId {
    mtime_ns: u128,
    len: u64,
    ino: u64,
}

fn file_id(path: &Path) -> Option<FileId> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    #[cfg(unix)]
    let ino = {
        use std::os::unix::fs::MetadataExt;
        meta.ino()
    };
    #[cfg(not(unix))]
    let ino = 0;
    Some(FileId {
        mtime_ns: meta
            .modified()
            .ok()?
            .duration_since(UNIX_EPOCH)
            .ok()?
            .as_nanos(),
        len: meta.len(),
        ino,
    })
}

/// This checkout's own `index` and `HEAD`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) struct GitIdentity {
    index: Option<FileId>,
    head: Option<FileId>,
}

struct Kept {
    epoch: u64,
    at: Instant,
    identity: GitIdentity,
    rows: Arc<Vec<FileStatus>>,
}

struct Suspicion {
    epoch: u64,
    since: Instant,
}

#[derive(Default)]
struct Trust {
    kept: Option<Kept>,
    suspect: Option<Suspicion>,
    convicted: bool,
    /// While convicted: the previous read, to tell when delivery is proven.
    last_read: Option<(u64, Arc<Vec<FileStatus>>)>,
}

/// Whether two answers say the same thing. `FileStatus` has no `PartialEq`
/// (it is a wire type), and its serialized form is what a person would see.
fn same_rows(a: &[FileStatus], b: &[FileStatus]) -> bool {
    a.len() == b.len()
        && serde_json::to_string(a).ok().is_some_and(|left| {
            serde_json::to_string(b)
                .ok()
                .is_some_and(|right| left == right)
        })
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
    trust: Mutex<Trust>,
    max_age_ms: AtomicU64,
    grace_ms: AtomicU64,
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
            trust: Mutex::new(Trust::default()),
            max_age_ms: AtomicU64::new(KEPT_STATUS_MAX_AGE.as_millis() as u64),
            grace_ms: AtomicU64::new(SUSPECT_GRACE.as_millis() as u64),
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

    /// The epoch whether or not the watch can vouch.
    pub(crate) fn epoch_now(&self) -> u64 {
        self.epoch.load(Ordering::Acquire)
    }

    #[cfg(test)]
    pub(crate) fn examined(&self) -> u64 {
        self.examined.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    pub(crate) fn set_timings(&self, max_age: Duration, grace: Duration) {
        self.max_age_ms
            .store(max_age.as_millis() as u64, Ordering::Relaxed);
        self.grace_ms
            .store(grace.as_millis() as u64, Ordering::Relaxed);
    }

    #[cfg(test)]
    pub(crate) fn is_convicted(&self) -> bool {
        self.lock_trust().convicted
    }

    /// Rules loaded and nothing since asked for a reload.
    #[cfg(test)]
    pub(crate) fn rules_fresh(&self) -> bool {
        self.rules_loaded_for.load(Ordering::Acquire) != NO_RULES && !self.rules_stale()
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

    fn lock_trust(&self) -> std::sync::MutexGuard<'_, Trust> {
        self.trust.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// [`Self::cover_at`] now.
    #[cfg(test)]
    pub(crate) fn cover(&self) -> Option<u64> {
        self.cover_at(Instant::now())
    }

    /// The epoch a read that starts at `now` would be covered by, or `None`
    /// when this watch cannot vouch for the worktree. See the module docs.
    pub(crate) fn cover_at(&self, now: Instant) -> Option<u64> {
        if !self.recursive || self.ended.load(Ordering::Acquire) {
            return None;
        }
        if self.rules_loaded_for.load(Ordering::Acquire) == NO_RULES || self.rules_stale() {
            return None;
        }
        if root_id(&self.root)? != self.root_id? {
            return None;
        }
        let epoch = self.epoch_now();
        let mut trust = self.lock_trust();
        if trust.convicted {
            return None;
        }
        if let Some(suspect) = &trust.suspect {
            if suspect.epoch != epoch {
                // An event arrived: the stream is delivering.
                trust.suspect = None;
            } else if now.saturating_duration_since(suspect.since)
                >= Duration::from_millis(self.grace_ms.load(Ordering::Relaxed))
            {
                trust.convicted = true;
                trust.suspect = None;
                trust.kept = None;
                log::warn!(
                    target: "watcher",
                    "{}: a change reached git but no event reached the watch for {:?}; \
                     the watch is not trusted and every status poll will read until a \
                     change is seen being delivered",
                    self.root.display(),
                    SUSPECT_GRACE
                );
                return None;
            }
        }
        Some(epoch)
    }

    /// This checkout's own `index` and `HEAD` as they are now.
    pub(crate) fn identity(&self) -> GitIdentity {
        let Some(own) = self.internal_roots.first() else {
            return GitIdentity::default();
        };
        GitIdentity {
            index: file_id(&own.join("index")),
            head: file_id(&own.join("HEAD")),
        }
    }

    /// The kept rows, when they were read at `epoch`, are young, and the
    /// checkout's index and `HEAD` are the files they were read with. A
    /// difference there with the epoch unmoved is a missed event: the kept
    /// read is refused and the watch becomes suspect.
    pub(crate) fn serve(
        &self,
        epoch: u64,
        now: Instant,
        identity: &GitIdentity,
    ) -> Option<Arc<Vec<FileStatus>>> {
        let mut trust = self.lock_trust();
        let kept = trust.kept.as_ref()?;
        let max_age = Duration::from_millis(self.max_age_ms.load(Ordering::Relaxed));
        if kept.epoch != epoch || now.saturating_duration_since(kept.at) >= max_age {
            return None;
        }
        if kept.identity != *identity {
            if trust.suspect.is_none() {
                trust.suspect = Some(Suspicion { epoch, since: now });
            }
            return None;
        }
        Some(Arc::clone(&kept.rows))
    }

    /// Records the answer of a real read that began when `cover_at` said
    /// `epoch` and the checkout looked like `identity`. A read that returns
    /// something other than the kept answer while the epoch has not moved
    /// makes the watch suspect.
    pub(crate) fn settle(
        &self,
        epoch: u64,
        rows: &[FileStatus],
        identity: GitIdentity,
        now: Instant,
    ) {
        let mut trust = self.lock_trust();
        if let Some(prev) = &trust.kept {
            if prev.epoch == epoch
                && self.epoch_now() == epoch
                && !same_rows(&prev.rows, rows)
                && trust.suspect.is_none()
            {
                trust.suspect = Some(Suspicion { epoch, since: now });
            }
        }
        trust.kept = (rows.len() <= MAX_KEPT_ROWS).then(|| Kept {
            epoch,
            at: now,
            identity,
            rows: Arc::new(rows.to_vec()),
        });
    }

    /// Every real read, vouched for or not. While convicted, a read that
    /// differs from the one before it with the epoch moved in between proves
    /// the stream delivered for a change a read then confirmed, and lifts the
    /// conviction.
    pub(crate) fn observe(&self, epoch_before: u64, rows: &[FileStatus]) {
        let mut trust = self.lock_trust();
        if !trust.convicted {
            return;
        }
        let proven = trust.last_read.as_ref().is_some_and(|(prev_epoch, prev)| {
            self.epoch_now() > *prev_epoch && !same_rows(prev, rows)
        });
        if proven {
            trust.convicted = false;
            trust.suspect = None;
            trust.last_read = None;
            trust.kept = None;
            log::info!(
                target: "watcher",
                "{}: a change was delivered and confirmed by a read; the watch is trusted again",
                self.root.display()
            );
            return;
        }
        trust.last_read =
            (rows.len() <= MAX_KEPT_ROWS).then(|| (epoch_before, Arc::new(rows.to_vec())));
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
            // What is tracked decides what is ignored: the rule set is stale
            // the moment this checkout's index or HEAD is written.
            if self.is_own_tracking_state(path) {
                self.rules_wanted.fetch_add(1, Ordering::AcqRel);
            }
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

    /// This checkout's own `index` or `HEAD`: the files whose change can
    /// change which paths are tracked. (A linked checkout's own git directory
    /// is `internal_roots[0]`, so its private copies are these.)
    fn is_own_tracking_state(&self, path: &Path) -> bool {
        self.internal_roots.first().is_some_and(|own| {
            path.parent() == Some(own.as_path())
                && path
                    .file_name()
                    .is_some_and(|name| name == "index" || name == "HEAD")
        })
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
        let verdict = ignore_verdict(relative, rules);
        // A directory git lists as ignored is inert whatever it is called,
        // `.devcouncil` included; the name only matters while git has not said.
        if verdict == IgnoreVerdict::Confirmed {
            return PathClass::Inert;
        }
        if verdict == IgnoreVerdict::Named
            || is_generated_state_noise_cached(path, &self.root, self.root_canonical.as_deref())
        {
            return PathClass::Weak;
        }
        PathClass::Signal
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
                .map(|p| p.to_ascii_lowercase())
                .collect::<HashSet<_>>(),
            exact: prefixes
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

    fn row(path: &str) -> FileStatus {
        FileStatus {
            path: path.to_string(),
            old_path: None,
            status_code: " M".to_string(),
            is_staged: false,
            is_conflicted: false,
            additions: 1,
            deletions: 0,
            staged_additions: None,
            staged_deletions: None,
            unstaged_additions: None,
            unstaged_deletions: None,
            warnings: Vec::new(),
        }
    }

    /// A root with a `.git` whose `index` and `HEAD` exist, so identity moves.
    fn repo_like() -> (tempfile::TempDir, PathBuf, Arc<WatchFacts>) {
        let temp = tempfile::TempDir::new().unwrap();
        let root = temp.path().canonicalize().unwrap();
        std::fs::create_dir(root.join(".git")).unwrap();
        std::fs::write(root.join(".git/index"), "i0").unwrap();
        std::fs::write(root.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        let facts = facts(&root);
        ready(&facts, &[]);
        (temp, root, facts)
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

    /// Ignored means ignored as git spelled it. A directory that differs only
    /// by case is a different directory on a case-sensitive volume, and on a
    /// case-insensitive one events carry the on-disk spelling, which is the
    /// one git printed. So the question "can this change be assumed away" is
    /// asked of the exact spelling on both kinds of volume; the folded match
    /// only keeps the old refresh behaviour. (Run here: this is volume
    /// independent. Not run: a real case-sensitive volume.)
    #[test]
    fn a_directory_that_differs_from_an_ignored_one_only_by_case_is_not_ignored() {
        let temp = tempfile::TempDir::new().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let facts = facts(&root);
        ready(&facts, &["out"]);
        let before = facts.cover().unwrap();
        assert!(!facts.admit(&write(&root.join("out/a.txt"))));
        assert_eq!(facts.cover(), Some(before), "the exact spelling is ignored");
        // Not delivered (the refresh classifier still folds) but it costs an
        // epoch, so no kept read can claim to cover it.
        assert!(!facts.admit(&write(&root.join("Out/a.txt"))));
        assert_eq!(facts.cover(), Some(before + 1));
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

    /// A ledger-like writer under a generated-state directory git lists as
    /// ignored moves nothing; the same directory before git has said so does.
    #[test]
    fn a_generated_state_directory_git_confirms_as_ignored_costs_no_epoch() {
        let temp = tempfile::TempDir::new().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let facts = facts(&root);
        ready(&facts, &[]);
        let before = facts.cover().unwrap();
        assert!(!facts.admit(&write(&root.join(".devcouncil/ledger.sqlite-wal"))));
        assert_eq!(facts.cover(), Some(before + 1), "git has not said");
        ready(&facts, &[".devcouncil"]);
        let after = facts.cover().unwrap();
        for _ in 0..100 {
            assert!(!facts.admit(&write(&root.join(".devcouncil/ledger.sqlite-wal"))));
        }
        assert_eq!(facts.cover(), Some(after), "git confirmed it ignored");
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

    /// What is tracked decides what `ls-files -o -i` prints, so a write to this
    /// checkout's index or HEAD (and only this checkout's) invalidates it.
    #[test]
    fn a_write_to_this_checkouts_index_or_head_makes_the_rules_stale() {
        let temp = tempfile::TempDir::new().unwrap();
        let root = temp.path().canonicalize().unwrap();
        for own in ["index", "HEAD"] {
            let facts = facts(&root);
            ready(&facts, &["out"]);
            assert!(facts.cover().is_some());
            facts.admit(&write(&root.join(".git").join(own)));
            assert_eq!(facts.cover(), None, "{own} was written");
            ready(&facts, &["out"]);
            assert!(facts.cover().is_some());
        }
        // A lock file, another checkout's index and a ref are not it.
        let facts = facts(&root);
        ready(&facts, &["out"]);
        facts.admit(&write(&root.join(".git/index.lock")));
        facts.admit(&write(&root.join(".git/worktrees/other/index")));
        facts.admit(&write(&root.join(".git/refs/heads/main")));
        assert!(
            facts.cover().is_some(),
            "none of those changes what is tracked here"
        );
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
        let (_temp, root, facts) = repo_like();
        let now = Instant::now();
        let epoch = facts.cover().unwrap();
        let identity = facts.identity();
        // The read starts here; the file changes while it runs.
        assert!(facts.admit(&write(&root.join("late.txt"))));
        facts.settle(epoch, &[], identity, now);
        let later = facts.cover().unwrap();
        assert!(facts.serve(later, now, &identity).is_none());
    }

    #[test]
    fn a_kept_read_is_served_only_at_its_own_epoch_and_only_while_young() {
        let (_temp, _root, facts) = repo_like();
        let now = Instant::now();
        let epoch = facts.cover().unwrap();
        let identity = facts.identity();
        facts.settle(epoch, &[], identity, now);
        assert!(facts.serve(epoch, now, &identity).is_some());
        assert!(facts.serve(epoch + 1, now, &identity).is_none());
        assert!(facts
            .serve(epoch, now + KEPT_STATUS_MAX_AGE, &identity)
            .is_none());
    }

    /// The index moved and no event came: the kept read is refused at once,
    /// and the watch is convicted once the event has had its grace.
    #[test]
    fn an_index_that_moved_without_an_event_refuses_the_kept_read_and_convicts() {
        let (_temp, root, facts) = repo_like();
        let t0 = Instant::now();
        let epoch = facts.cover_at(t0).unwrap();
        let identity = facts.identity();
        facts.settle(epoch, &[row("a.txt")], identity, t0);
        assert!(facts.serve(epoch, t0, &identity).is_some());

        // git rewrites the index; the stream says nothing (nothing is injected).
        std::fs::write(root.join(".git/index"), "i1-longer").unwrap();
        let moved = facts.identity();
        assert_ne!(moved, identity);
        assert!(facts.serve(epoch, t0, &moved).is_none(), "refused at once");
        assert!(!facts.is_convicted(), "given its grace first");
        assert_eq!(facts.cover_at(t0 + Duration::from_secs(1)), Some(epoch));
        assert_eq!(facts.cover_at(t0 + SUSPECT_GRACE), None, "now convicted");
        assert!(facts.is_convicted());
        assert_eq!(facts.cover_at(t0 + Duration::from_secs(600)), None);
    }

    /// The same index write with its event delivered is the healthy case.
    #[test]
    fn an_index_that_moved_with_its_event_is_not_a_conviction() {
        let (_temp, root, facts) = repo_like();
        let t0 = Instant::now();
        let epoch = facts.cover_at(t0).unwrap();
        let identity = facts.identity();
        facts.settle(epoch, &[], identity, t0);
        std::fs::write(root.join(".git/index"), "i1-longer").unwrap();
        facts.admit(&write(&root.join(".git/index")));
        // The write also made the rules stale; reload as the loop would.
        ready(&facts, &[]);
        let now = facts
            .cover_at(t0 + SUSPECT_GRACE * 2)
            .expect("not convicted");
        assert_eq!(now, epoch + 1);
        assert!(facts.serve(now, t0, &facts.identity()).is_none());
        assert!(!facts.is_convicted());
    }

    /// A plain file changed under a stream that says nothing. The kept read is
    /// served (the watch cannot know) until the cap; the read at the cap
    /// disagrees with it at an unmoved epoch, and the watch is convicted.
    #[test]
    fn a_plain_edit_under_a_silent_stream_is_stale_at_most_until_the_cap_then_convicts() {
        let (_temp, _root, facts) = repo_like();
        let t0 = Instant::now();
        let epoch = facts.cover_at(t0).unwrap();
        let identity = facts.identity();
        facts.settle(epoch, &[], identity, t0);
        // Inside the cap the stale answer is served: that is the bound.
        let almost = t0 + KEPT_STATUS_MAX_AGE - Duration::from_secs(1);
        assert!(facts.serve(epoch, almost, &identity).is_some());
        // At the cap it is not; the real read now sees the edit.
        let at_cap = t0 + KEPT_STATUS_MAX_AGE;
        assert!(facts.serve(epoch, at_cap, &identity).is_none());
        facts.settle(epoch, &[row("edited.txt")], identity, at_cap);
        assert!(!facts.is_convicted());
        assert_eq!(facts.cover_at(at_cap + SUSPECT_GRACE), None);
        assert!(facts.is_convicted());
    }

    /// The same read with nothing different is the healthy, idle case.
    #[test]
    fn a_read_at_the_cap_that_agrees_does_not_convict() {
        let (_temp, _root, facts) = repo_like();
        let t0 = Instant::now();
        let epoch = facts.cover_at(t0).unwrap();
        let identity = facts.identity();
        facts.settle(epoch, &[row("a.txt")], identity, t0);
        let at_cap = t0 + KEPT_STATUS_MAX_AGE;
        facts.settle(epoch, &[row("a.txt")], identity, at_cap);
        assert_eq!(facts.cover_at(at_cap + SUSPECT_GRACE * 10), Some(epoch));
        assert!(!facts.is_convicted());
    }

    #[test]
    fn a_conviction_lifts_when_a_delivered_change_is_confirmed_by_a_read() {
        let (_temp, root, facts) = repo_like();
        let t0 = Instant::now();
        let epoch = facts.cover_at(t0).unwrap();
        let identity = facts.identity();
        facts.settle(epoch, &[row("a.txt")], identity, t0);
        facts.settle(epoch, &[row("b.txt")], identity, t0);
        assert_eq!(facts.cover_at(t0 + SUSPECT_GRACE), None);
        assert!(facts.is_convicted());

        // Convicted: polls read. A read with nothing new proves nothing.
        facts.observe(facts.epoch_now(), &[row("b.txt")]);
        facts.observe(facts.epoch_now(), &[row("b.txt")]);
        assert!(facts.is_convicted());
        // An event arrives and the next read differs from the one before it.
        assert!(facts.admit(&write(&root.join("c.txt"))));
        facts.observe(facts.epoch_now(), &[row("b.txt"), row("c.txt")]);
        assert!(!facts.is_convicted());
        let now = facts
            .cover_at(t0 + SUSPECT_GRACE * 3)
            .expect("trusted again");
        assert_eq!(now, facts.epoch_now());
    }

    /// An event alone, or a changed answer alone, is not delivery proven.
    #[test]
    fn a_conviction_does_not_lift_on_an_event_without_a_confirmed_change() {
        let (_temp, root, facts) = repo_like();
        let t0 = Instant::now();
        let epoch = facts.cover_at(t0).unwrap();
        let identity = facts.identity();
        facts.settle(epoch, &[row("a.txt")], identity, t0);
        facts.settle(epoch, &[row("b.txt")], identity, t0);
        assert_eq!(facts.cover_at(t0 + SUSPECT_GRACE), None);
        facts.observe(facts.epoch_now(), &[row("b.txt")]);
        assert!(facts.admit(&write(&root.join("noise.txt"))));
        facts.observe(facts.epoch_now(), &[row("b.txt")]);
        assert!(facts.is_convicted(), "the answer did not change");
        // The answer changes but no event moved the epoch in between.
        facts.observe(facts.epoch_now(), &[row("z.txt")]);
        assert!(facts.is_convicted());
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
