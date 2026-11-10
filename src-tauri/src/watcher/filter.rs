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
//! What is never dropped: a backend error, a must-rescan flag, an event with no
//! path, and an event naming the worktree root itself (the only trace of a repository
//! that moved away, and what wakes the loop to notice).
//!
//! On Linux and Windows the root stays non-recursive (inotify holds one
//! descriptor per directory and ReadDirectoryChangesW reports a whole tree), so
//! the backend never delivers nested paths and this gate forwards everything.

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
    /// Paths classified by the callback; the bound a test holds a storm to.
    #[cfg(test)]
    examined: std::sync::atomic::AtomicU64,
    /// Rule sets published, i.e. `git ls-files` reloads the loop made.
    #[cfg(test)]
    loads: std::sync::atomic::AtomicU64,
}

impl EventFilter {
    pub(crate) fn new(
        root: Option<&Path>,
        internal_roots: Vec<PathBuf>,
        root_mode: RecursiveMode,
    ) -> Arc<Self> {
        Arc::new(Self {
            recursive: root.is_some() && root_mode == RecursiveMode::Recursive,
            root: root.map(Path::to_path_buf).unwrap_or_default(),
            root_canonical: root.and_then(|root| root.canonicalize().ok()),
            internal_roots,
            // Until the loop publishes its first load, only the built-in
            // build-output names are known.
            rules: RwLock::new(Arc::new(IgnoreRules::builtin())),
            #[cfg(test)]
            examined: std::sync::atomic::AtomicU64::new(0),
            #[cfg(test)]
            loads: std::sync::atomic::AtomicU64::new(0),
        })
    }

    pub(crate) fn publish_rules(&self, rules: Arc<IgnoreRules>) {
        *self.rules.write().unwrap_or_else(PoisonError::into_inner) = rules;
        #[cfg(test)]
        self.loads
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    #[cfg(test)]
    pub(crate) fn examined(&self) -> u64 {
        self.examined.load(std::sync::atomic::Ordering::Relaxed)
    }

    #[cfg(test)]
    pub(crate) fn loads(&self) -> u64 {
        self.loads.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Whether `res` is handed to the debounce loop. Runs on the backend's
    /// thread: it spawns nothing and takes only the rule set's read lock.
    pub(crate) fn admit(&self, res: &Result<Event, notify::Error>) -> bool {
        if !self.recursive {
            return true;
        }
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
        Ok(
            Event::new(EventKind::Modify(ModifyKind::Data(DataChange::Content)))
                .add_path(path),
        )
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
            filter.admit(&Ok(
                Event::new(EventKind::Any).add_path(root.clone())
            )),
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
                root.join("out").join(format!("d{}", i % 50)).join(format!("f{i}.o"))
            )));
        }
        assert_eq!(filter.examined(), 10_000);
        assert!(filter.admit(&Ok(
            Event::new(EventKind::Create(CreateKind::File)).add_path(root.join("sentinel.txt"))
        )));
    }
}
