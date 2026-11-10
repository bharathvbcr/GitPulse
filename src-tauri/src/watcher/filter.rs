//! The native callback's gate for the recursive worktree-root watch.
//!
//! On macOS the worktree root is watched recursively: FSEvents is recursive in
//! the kernel and the stream already covers the root because `.git` lives under
//! it, so the backend's `NonRecursive` mode was only a user-space filter that
//! threw nested paths away. With the filter gone a nested edit is seen, and so
//! is everything a build writes under `target/` or `node_modules/`. That volume
//! must not reach the bounded event queue: ten thousand writes would fill it,
//! and a full queue is answered with a full refresh. So the callback, on the
//! backend's own thread, asks the same classifier the debounce loop uses
//! ([`super::event_has_signal`]) and forwards only events that can matter.
//!
//! # What this gate is allowed to be wrong about
//!
//! Only DELIVERY. An event dropped here that should have been forwarded costs
//! one refresh, up to one status poll late (6 s): the poll reads every time,
//! exactly as it did when the root watch was non-recursive, and nothing treats
//! the absence of an event as proof that the worktree is unchanged. That is why
//! the ignore list behind it is the plain `git ls-files -o -i --directory`
//! answer, which describes what is ignored now and not why, and why it is
//! reloaded only when a rule file is edited or a directory appears (see
//! [`super::event_may_change_ignore_rules`]). Nothing here, and nothing built on
//! it, may rely on a path having been classified as unable to matter.
//!
//! The poll's "nothing happened since the read I kept" fact ([`super::skip`])
//! does not use this gate's verdict either: the callback counts every
//! invocation, before any filtering, so an event dropped here still ends it.
//!
//! Ignored FILES that the listing prints one by one (a nested
//! `.claude/settings.local.json`, `lcov.info`, `.DS_Store`) are dropped too
//! when an event names nothing else; main produced no event at all for them
//! (the backend discarded every nested path), so forwarding them would be a
//! new cost. The list is bounded; past the bound a file is forwarded.
//!
//! A rule-file event (`.gitignore`) can itself be dropped here, when it lies
//! under a directory the list names or a build-looking name (`node_modules/pkg/
//! .gitignore`), so it never reaches the loop and cannot ask for a reload. That
//! does not matter for delivery: git ignores a `.gitignore` inside an ignored
//! directory, and in a directory that is only listed because its files are
//! ignored a stale list costs a refresh now and then, which the poll covers.
//! Forwarding them instead would hand a reload to every `npm install`.
//!
//! What is never dropped: a backend error, a must-rescan flag, an event with no
//! path, and an event naming the worktree root itself (the only trace of a repository
//! that moved away, and what wakes the loop to notice).
//!
//! On Linux and Windows the root stays non-recursive (inotify holds one
//! descriptor per directory and ReadDirectoryChangesW reports a whole tree), so
//! the backend never delivers nested paths and this gate forwards everything.

use super::skip::PollSkip;
use super::{event_has_signal, is_worktree_root, IgnoreRules};
use notify::{Event, RecursiveMode};
use std::path::{Path, PathBuf};
use std::sync::{Arc, PoisonError, RwLock};

/// How the worktree root is registered. Recursive only where the backend's
/// recursion is a user-space filter over a kernel-recursive stream (FSEvents).
pub(crate) const ROOT_WATCH_MODE: RecursiveMode = if cfg!(target_os = "macos") {
    RecursiveMode::Recursive
} else {
    RecursiveMode::NonRecursive
};

pub(crate) struct EventFilter {
    recursive: bool,
    root: PathBuf,
    root_canonical: Option<PathBuf>,
    internal_roots: Vec<PathBuf>,
    /// The rule set the debounce loop classifies with, published by it after
    /// every load: one load serves both.
    rules: RwLock<Arc<IgnoreRules>>,
    /// Counts every callback invocation, before any filtering, so the poll can
    /// tell that nothing at all happened (see [`super::skip`]).
    skip: PollSkip,
    /// Paths classified by the callback; the bound a test holds a storm to.
    #[cfg(test)]
    examined: std::sync::atomic::AtomicU64,
    /// Rule sets published, i.e. `git ls-files` reloads the loop made.
    #[cfg(all(test, target_os = "macos"))]
    loads: std::sync::atomic::AtomicU64,
    /// Native callback invocations, whether or not they were forwarded.
    #[cfg(all(test, target_os = "macos"))]
    callbacks: std::sync::atomic::AtomicU64,
}

impl EventFilter {
    pub(crate) fn new(
        root: Option<&Path>,
        internal_roots: Vec<PathBuf>,
        root_mode: RecursiveMode,
    ) -> Arc<Self> {
        let recursive = root.is_some() && root_mode == RecursiveMode::Recursive;
        Arc::new(Self {
            recursive,
            skip: PollSkip::new(root, recursive),
            root: root.map(Path::to_path_buf).unwrap_or_default(),
            root_canonical: root.and_then(|root| root.canonicalize().ok()),
            internal_roots,
            // Until the loop publishes its first load, only the built-in
            // build-output names are known.
            rules: RwLock::new(Arc::new(IgnoreRules::builtin())),
            #[cfg(test)]
            examined: std::sync::atomic::AtomicU64::new(0),
            #[cfg(all(test, target_os = "macos"))]
            loads: std::sync::atomic::AtomicU64::new(0),
            #[cfg(all(test, target_os = "macos"))]
            callbacks: std::sync::atomic::AtomicU64::new(0),
        })
    }

    pub(crate) fn skip(&self) -> &PollSkip {
        &self.skip
    }

    pub(crate) fn publish_rules(&self, rules: Arc<IgnoreRules>) {
        *self.rules.write().unwrap_or_else(PoisonError::into_inner) = rules;
        #[cfg(all(test, target_os = "macos"))]
        self.loads
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    #[cfg(test)]
    pub(crate) fn examined(&self) -> u64 {
        self.examined.load(std::sync::atomic::Ordering::Relaxed)
    }

    #[cfg(all(test, target_os = "macos"))]
    pub(crate) fn loads(&self) -> u64 {
        self.loads.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Called by the native callback before [`Self::admit`], so a measurement
    /// can count what the backend delivered to us.
    #[cfg(all(test, target_os = "macos"))]
    pub(crate) fn count_callback(&self) {
        self.callbacks
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    #[cfg(all(test, target_os = "macos"))]
    pub(crate) fn callbacks(&self) -> u64 {
        self.callbacks.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// True when the root watch is recursive, i.e. on macOS.
    pub(crate) fn recursive(&self) -> bool {
        self.recursive
    }

    /// Whether every path of `event` is an ignored file the listing printed
    /// one by one. Such a write cannot matter to an immediate refresh and is
    /// not worth a queue slot; an event that also names anything else is
    /// forwarded (an editor's atomic save names a temporary file and the
    /// tracked file it replaces).
    fn names_only_listed_ignored_files(&self, event: &Event, rules: &IgnoreRules) -> bool {
        if rules.files.is_empty() || event.paths.is_empty() {
            return false;
        }
        let root_canonical = self.root_canonical.as_deref();
        event.paths.iter().all(|path| {
            let relative = path
                .strip_prefix(&self.root)
                .ok()
                .or_else(|| root_canonical.and_then(|root| path.strip_prefix(root).ok()));
            relative.is_some_and(|relative| {
                let printed = relative.to_string_lossy();
                if cfg!(windows) {
                    rules.files.contains(printed.replace('\\', "/").as_str())
                } else {
                    rules.files.contains(printed.as_ref())
                }
            })
        })
    }

    /// Whether `res` is handed to the debounce loop. Runs on the backend's
    /// thread: it spawns nothing and takes only the rule set's read lock.
    pub(crate) fn admit(&self, res: &Result<Event, notify::Error>) -> bool {
        if !self.recursive {
            return true;
        }
        // Whatever this is, the poll may not skip on the strength of a read
        // that began before it. Nothing below may influence that.
        self.skip.event();
        let Ok(event) = res else {
            return true;
        };
        let rules = Arc::clone(&self.rules.read().unwrap_or_else(PoisonError::into_inner));
        #[cfg(test)]
        self.examined.fetch_add(
            event.paths.len() as u64,
            std::sync::atomic::Ordering::Relaxed,
        );
        let root_canonical = self.root_canonical.as_deref();
        if event
            .paths
            .iter()
            .any(|path| is_worktree_root(path, &self.root, root_canonical))
        {
            return true;
        }
        if self.names_only_listed_ignored_files(event, &rules) {
            return false;
        }
        event_has_signal(
            event,
            &self.internal_roots,
            &self.root,
            root_canonical,
            &rules,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{CreateKind, DataChange, ModifyKind};
    use notify::EventKind;

    fn filter(root: &Path, mode: RecursiveMode) -> Arc<EventFilter> {
        EventFilter::new(Some(root), vec![root.join(".git")], mode)
    }

    fn with_rules(filter: &EventFilter, prefixes: &[&str]) {
        let mut rules = IgnoreRules::builtin();
        rules.prefixes = prefixes.iter().map(|p| p.to_string()).collect();
        filter.publish_rules(Arc::new(rules));
    }

    fn write(path: PathBuf) -> Result<Event, notify::Error> {
        Ok(Event::new(EventKind::Modify(ModifyKind::Data(DataChange::Content))).add_path(path))
    }

    #[test]
    fn a_nested_edit_is_forwarded_and_an_ignored_directory_write_is_not() {
        let temp = tempfile::TempDir::new().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let filter = filter(&root, RecursiveMode::Recursive);
        with_rules(&filter, &["gen"]);
        assert!(filter.admit(&write(root.join("src/deep/er/x.rs"))));
        assert!(!filter.admit(&write(root.join("gen/out/a.o"))));
        assert!(!filter.admit(&write(root.join("target/debug/x"))));
        assert!(!filter.admit(&write(root.join(".git/index.lock"))));
        assert!(filter.admit(&write(root.join(".git/index"))));
    }

    #[test]
    fn what_cannot_be_classified_is_never_dropped() {
        let temp = tempfile::TempDir::new().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let filter = filter(&root, RecursiveMode::Recursive);
        // Before the first rule set exists only the built-in names are known.
        assert!(filter.admit(&write(root.join("gen/out/a.o"))));
        assert!(!filter.admit(&write(root.join("target/debug/x"))));
        with_rules(&filter, &["gen"]);
        let rescan = Event::new(EventKind::Other).set_flag(notify::event::Flag::Rescan);
        assert!(filter.admit(&Ok(rescan)), "must-rescan");
        assert!(filter.admit(&Err(notify::Error::generic("lost"))), "error");
        assert!(
            filter.admit(&Ok(Event::new(EventKind::Other))),
            "an event with no path"
        );
        assert!(
            filter.admit(&Ok(Event::new(EventKind::Any).add_path(root.clone()))),
            "the root itself wakes the loop"
        );
    }

    #[test]
    fn a_non_recursive_root_watch_forwards_everything_it_is_given() {
        let temp = tempfile::TempDir::new().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let filter = filter(&root, RecursiveMode::NonRecursive);
        with_rules(&filter, &["gen"]);
        assert!(filter.admit(&write(root.join("gen/out/a.o"))));
        assert!(filter.admit(&write(root.join(".git/index.lock"))));
        assert_eq!(filter.examined(), 0, "no classification where none is due");
    }

    /// The poll's premise: EVERY callback invocation moves the epoch, whether
    /// or not the event is forwarded, and whatever it is.
    #[test]
    fn every_callback_invocation_moves_the_epoch_whatever_the_filter_says() {
        let temp = tempfile::TempDir::new().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let filter = filter(&root, RecursiveMode::Recursive);
        with_rules(&filter, &["gen"]);
        let rescan = Event::new(EventKind::Other).set_flag(notify::event::Flag::Rescan);
        let access = Event::new(EventKind::Access(notify::event::AccessKind::Any))
            .add_path(root.join("src/a.rs"));
        let cases: Vec<(&str, Result<Event, notify::Error>, bool)> = vec![
            ("a source edit", write(root.join("src/a.rs")), true),
            (
                "an ignored-directory write",
                write(root.join("gen/x.o")),
                false,
            ),
            (
                "a build-looking directory write",
                write(root.join("target/x")),
                false,
            ),
            (
                "git-internal noise",
                write(root.join(".git/index.lock")),
                false,
            ),
            ("an access event", Ok(access), false),
            ("a must-rescan flag", Ok(rescan), true),
            ("a backend error", Err(notify::Error::generic("lost")), true),
            (
                "an event with no path",
                Ok(Event::new(EventKind::Other)),
                true,
            ),
        ];
        for (what, event, forwarded) in cases {
            let before = filter.skip().epoch_now();
            assert_eq!(filter.admit(&event), forwarded, "{what}: forwarded");
            assert_eq!(filter.skip().epoch_now(), before + 1, "{what}: epoch");
        }
    }

    /// Where the root watch is not recursive nothing is counted: the poll
    /// there always reads (see `PollSkip::cover_at`).
    #[test]
    fn a_non_recursive_watch_cannot_vouch_for_anything() {
        let temp = tempfile::TempDir::new().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let filter = filter(&root, RecursiveMode::NonRecursive);
        assert_eq!(filter.skip().cover_at(std::time::Instant::now()), None);
    }

    /// Ten thousand writes under an ignored directory: each is classified once,
    /// none is forwarded, and nothing spawns (this function has no process API).
    #[test]
    fn ten_thousand_ignored_writes_are_dropped_with_one_classification_each() {
        let temp = tempfile::TempDir::new().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let filter = filter(&root, RecursiveMode::Recursive);
        with_rules(&filter, &["out"]);
        for i in 0..10_000u32 {
            assert!(!filter.admit(&write(
                root.join("out")
                    .join(format!("d{}", i % 50))
                    .join(format!("f{i}.o"))
            )));
        }
        assert_eq!(filter.examined(), 10_000);
        assert!(filter.admit(&Ok(
            Event::new(EventKind::Create(CreateKind::File)).add_path(root.join("sentinel.txt"))
        )));
    }

    fn with_files(filter: &EventFilter, files: &[&str]) {
        let mut rules = IgnoreRules::builtin();
        rules.files = files.iter().map(|f| f.to_string()).collect();
        filter.publish_rules(Arc::new(rules));
    }

    /// Main produced no event at all for a nested ignored file (the backend
    /// discarded every nested path), so forwarding one is a new cost.
    #[test]
    fn a_write_to_a_listed_ignored_nested_file_forwards_nothing() {
        let temp = tempfile::TempDir::new().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let filter = filter(&root, RecursiveMode::Recursive);
        with_files(
            &filter,
            &["crates/p/lcov.info", ".claude/settings.local.json"],
        );
        assert!(!filter.admit(&write(root.join("crates/p/lcov.info"))));
        assert!(!filter.admit(&write(root.join(".claude/settings.local.json"))));
        // Not listed: forwarded, as is a tracked nested file.
        assert!(filter.admit(&write(root.join("crates/p/Cargo.toml"))));
        assert!(filter.admit(&write(root.join("crates/p/lcov.info.bak"))));
    }

    /// An editor's atomic save writes a temporary file and renames it over the
    /// tracked file; the tracked path must still be forwarded, together with
    /// the temporary one and on its own.
    #[test]
    fn an_atomic_save_over_a_tracked_file_is_still_forwarded() {
        let temp = tempfile::TempDir::new().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let filter = filter(&root, RecursiveMode::Recursive);
        // The temporary name is itself ignored (`*.swp`), the tracked one is not.
        with_files(&filter, &["src/.main.rs.swp"]);
        let rename = Event::new(EventKind::Modify(ModifyKind::Name(
            notify::event::RenameMode::Both,
        )))
        .add_path(root.join("src/.main.rs.swp"))
        .add_path(root.join("src/main.rs"));
        assert!(filter.admit(&Ok(rename)), "both paths in one event");
        assert!(
            filter.admit(&write(root.join("src/main.rs"))),
            "the tracked path alone"
        );
        assert!(
            !filter.admit(&write(root.join("src/.main.rs.swp"))),
            "the swap file alone"
        );
    }

    /// A `.gitignore` under a directory the list names is dropped here, so it
    /// cannot ask for a reload; see the module comment for why that is fine.
    #[test]
    fn a_rule_file_under_an_ignored_or_build_directory_is_dropped() {
        let temp = tempfile::TempDir::new().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let filter = filter(&root, RecursiveMode::Recursive);
        with_rules(&filter, &["gen"]);
        assert!(!filter.admit(&write(root.join("gen/.gitignore"))));
        assert!(!filter.admit(&write(root.join("node_modules/pkg/.gitignore"))));
        assert!(filter.admit(&write(root.join("src/.gitignore"))));
    }
}
