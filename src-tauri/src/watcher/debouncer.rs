use super::filter::{EventFilter, ROOT_WATCH_MODE};
use notify::{Config, Event, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::Arc;
use std::time::Duration;

/// Events the native callback may have queued for the debounce loop. A burst
/// past this is not buffered further: the callback records an overflow, and
/// the loop answers it with one full refresh (see [`RepoFileWatcher::take_overflow`]).
/// The queue used to be unbounded, so a stalled consumer grew it without limit.
pub(crate) const EVENT_QUEUE_CAPACITY: usize = 4096;

pub struct RepoFileWatcher {
    watcher: Option<RecommendedWatcher>,
    retired: Receiver<()>,
    pub receiver: Receiver<Result<Event, notify::Error>>,
    overflow: Arc<AtomicBool>,
    /// A second sender on `receiver`, kept so that something other than the
    /// native callback can wake whoever is blocked on it (see
    /// [`Self::waker`]). It also means the receiver no longer disconnects when
    /// the backend dies; [`Self::backend_alive`] is how that is noticed.
    wake: SyncSender<Result<Event, notify::Error>>,
    /// Shared with the native callback, which filters with it; the debounce
    /// loop publishes the ignore rules it classifies with (see [`super::filter`]).
    filter: Arc<EventFilter>,
}

impl RepoFileWatcher {
    pub(crate) fn filter(&self) -> Arc<EventFilter> {
        Arc::clone(&self.filter)
    }

    /// A sender onto this watcher's event channel, for waking its reader.
    pub(crate) fn waker(&self) -> SyncSender<Result<Event, notify::Error>> {
        self.wake.clone()
    }

    /// False once the native backend has released its callback, which is what
    /// the channel's disconnecting used to say before it had a second sender.
    /// A watcher whose backend was shut down on purpose is not alive either.
    pub(crate) fn backend_alive(&self) -> bool {
        !matches!(
            self.retired.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Disconnected)
        )
    }

    #[cfg(test)]
    pub(crate) fn simulate_overflow(&self) {
        self.overflow.store(true, Ordering::Release);
    }

    /// True once if events were dropped because the queue was full since the
    /// last call. Dropped events are unknowable, so the caller refreshes
    /// everything — the same answer a `Rescan` event gets.
    pub(crate) fn take_overflow(&self) -> bool {
        self.overflow.swap(false, Ordering::AcqRel)
    }

    /// Wait for the backend to release its callback and outstanding native
    /// reads. notify's Windows Drop only queues Stop; dropping its Rust handle
    /// alone does not mean ReadDirectoryChangesW has relinquished the tree.
    pub(crate) fn shutdown(&mut self) -> Result<(), String> {
        // Whatever this watch said before it ended is not to be relied on after.
        self.filter.skip().end();
        if let Some(watcher) = self.watcher.take() {
            drop(watcher);
            match self.retired.recv_timeout(Duration::from_secs(2)) {
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Ok(()),
                _ => Err(
                    "Filesystem watcher did not release its native resources within 2 seconds"
                        .into(),
                ),
            }
        } else {
            Ok(())
        }
    }

    /// Watches only a resolved git directory recursively (work-tree `.git`,
    /// linked-worktree git dir, or bare repo). See [`Self::watch_repo`] for
    /// the full-repo form.
    pub fn watch(git_dir: &Path) -> Result<Self, String> {
        Self::watch_repo(git_dir, None, None)
    }

    /// Watches a repository for the change detector: the resolved git dir
    /// recursively, the shared common dir when it differs (linked worktrees
    /// keep refs in the main repo's git dir, so branch/ref writes made
    /// anywhere must fire), and — when the repository has a separate
    /// worktree root — that root too.
    ///
    /// How the worktree root is watched depends on the platform
    /// ([`ROOT_WATCH_MODE`]).
    ///
    /// On macOS it is RECURSIVE. FSEvents is recursive in the kernel, and the
    /// stream already covers the root because `.git` lives under it; the
    /// backend's `NonRecursive` mode was only a user-space filter that threw
    /// nested paths away (`framework/notify/src/fsevent.rs`, `callback_impl`).
    /// Choosing `Recursive` removes that filter, adds no stream and no
    /// registration, and lets a nested edit be seen. What would then flood the
    /// bounded event queue (a build writing thousands of files under `target/`)
    /// is dropped by [`EventFilter::admit`] on the callback thread, before it
    /// takes a slot; see [`super::filter`] for what that gate may be wrong about.
    ///
    /// On Linux and Windows it stays NON-recursive (top-level entries only):
    /// inotify watches one descriptor per directory and ReadDirectoryChangesW
    /// reports a whole tree, so recursion there is bounded by descriptors or
    /// pays for the whole tree. Deeper edits still reach us through the
    /// index/HEAD writes they cause inside `.git`, and the status poll covers
    /// the rest.
    ///
    /// # Registration is atomic (was: lossy on macOS)
    ///
    /// Every path is accumulated through one `Watcher::paths_mut()` handle and
    /// installed by a single `commit()`, rather than by successive `watch()`
    /// calls.
    ///
    /// This is not tidiness. notify's fsevent backend implements each
    /// `watch()` as `stop()` + append-path + `run()`, and `run()` recreates
    /// the whole `FSEventStream` with a fresh `kFSEventStreamEventIdSinceNow`
    /// epoch — so a change landing between two successive registrations fell
    /// into no epoch and was missed permanently. With three paths to register
    /// (git dir, common dir, worktree root) that left two windows on every
    /// watch startup, and the six-second status poll existed partly to paper
    /// over them. `FsEventPathsMut` stops once on construction, only appends
    /// in `add()`, and runs once in `commit()`: one stream creation for the
    /// whole set, and no window at all.
    ///
    /// The `PathsMut` API is uniform across backends (inotify and the polling
    /// fallback implement it as plain add/remove), so this is not a
    /// macOS-specific code path — it is the same call everywhere, and it is
    /// simply also correct on FSEvents.
    pub fn watch_repo(
        git_dir: &Path,
        worktree_root: Option<&Path>,
        common_dir: Option<&Path>,
    ) -> Result<Self, String> {
        Self::watch_repo_with(git_dir, worktree_root, common_dir, ROOT_WATCH_MODE)
    }

    /// [`Self::watch_repo`] with the worktree root's mode named, so a test can
    /// exercise the mode the other platforms use on this one.
    pub(crate) fn watch_repo_with(
        git_dir: &Path,
        worktree_root: Option<&Path>,
        common_dir: Option<&Path>,
        root_mode: RecursiveMode,
    ) -> Result<Self, String> {
        if !git_dir.exists() {
            return Err(format!(
                "Git directory does not exist: {}",
                git_dir.display()
            ));
        }

        let (tx, rx) = sync_channel(EVENT_QUEUE_CAPACITY);
        let wake = tx.clone();
        let overflow = Arc::new(AtomicBool::new(false));
        let callback_overflow = overflow.clone();
        let (lifetime, retired) = channel::<()>();

        let mut internal_roots = vec![git_dir.to_path_buf()];
        if let Some(common) = common_dir {
            if !internal_roots.iter().any(|root| root == common) {
                internal_roots.push(common.to_path_buf());
            }
        }
        let filter = EventFilter::new(
            worktree_root.filter(|root| *root != git_dir),
            internal_roots,
            root_mode,
        );
        let callback_filter = Arc::clone(&filter);

        let mut watcher = RecommendedWatcher::new(
            move |res| {
                // The sender's lifetime follows every native callback owner.
                // It is never sent to: disconnect is the shutdown receipt.
                std::hint::black_box(&lifetime);
                if callback_filter.admit(&res) {
                    forward_event(&tx, &callback_overflow, res);
                }
            },
            Config::default().with_poll_interval(Duration::from_millis(150)),
        )
        .map_err(|e| format!("Failed to create filesystem watcher: {}", e))?;

        // Validate before touching the watcher: a missing worktree root used to
        // be reported only AFTER two paths were already live, which left a
        // half-registered watcher behind on the error path.
        if let Some(root) = worktree_root {
            if !root.exists() {
                return Err(format!(
                    "Repository work tree does not exist: {}",
                    root.display()
                ));
            }
        }

        {
            let mut paths = watcher.paths_mut();
            paths
                .add(git_dir, RecursiveMode::Recursive)
                .map_err(|e| format!("Failed to watch git directory: {}", e))?;

            if let Some(common) = common_dir {
                if common != git_dir && common.exists() {
                    paths
                        .add(common, RecursiveMode::Recursive)
                        .map_err(|e| format!("Failed to watch common git directory: {}", e))?;
                }
            }

            if let Some(root) = worktree_root {
                if root != git_dir {
                    paths
                        .add(root, root_mode)
                        .map_err(|e| format!("Failed to watch work tree root: {}", e))?;
                }
            }

            // One stream creation for the whole set. An early `?` above drops
            // `paths` without committing, which leaves the watcher stopped
            // rather than partially armed — the honest state for a failed
            // registration.
            paths
                .commit()
                .map_err(|e| format!("Failed to install repository watches: {}", e))?;
        }

        Ok(Self {
            watcher: Some(watcher),
            retired,
            receiver: rx,
            overflow,
            wake,
            filter,
        })
    }
}

/// Hands one native event to the debounce loop without ever blocking the
/// backend's callback thread: a full queue records an overflow instead.
fn forward_event(
    tx: &SyncSender<Result<Event, notify::Error>>,
    overflow: &AtomicBool,
    res: Result<Event, notify::Error>,
) {
    match tx.try_send(res) {
        Err(TrySendError::Full(_)) => overflow.store(true, Ordering::Release),
        // The consumer is gone; the loop's own disconnect handling reports it.
        Err(TrySendError::Disconnected(_)) | Ok(()) => {}
    }
}

impl Drop for RepoFileWatcher {
    fn drop(&mut self) {
        if let Err(error) = self.shutdown() {
            log::warn!(target: "watcher", "{error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_queue_records_one_overflow_instead_of_growing() {
        let (tx, rx) = sync_channel(2);
        let overflow = AtomicBool::new(false);
        for _ in 0..5 {
            forward_event(&tx, &overflow, Ok(Event::new(notify::EventKind::Any)));
        }
        assert_eq!(rx.try_iter().count(), 2, "the queue is bounded");
        assert!(
            overflow.swap(false, Ordering::AcqRel),
            "the drop is recorded"
        );
        assert!(!overflow.load(Ordering::Acquire));
    }
}
