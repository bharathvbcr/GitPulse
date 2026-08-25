use notify::{Config, Event, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::Path;
use std::sync::mpsc::{channel, Receiver};
use std::time::Duration;

pub struct RepoFileWatcher {
    _watcher: RecommendedWatcher,
    pub receiver: Receiver<Result<Event, notify::Error>>,
}

impl RepoFileWatcher {
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
    /// The worktree watch is deliberately NON-recursive (top-level entries
    /// only). Recursive watching of a whole checkout is far too hot for the
    /// debounce loop, but without *some* worktree coverage an unstaged edit
    /// never fires `repo-changed` and status goes stale. Top-level entries are
    /// where edits surface first (new/removed files, mtime churn on roots);
    /// deeper edits still reach us through the index/HEAD writes they cause
    /// inside `.git`. Both `notify` backends in use here support the mode:
    /// inotify natively, FSEvents via its own filtering.
    pub fn watch_repo(
        git_dir: &Path,
        worktree_root: Option<&Path>,
        common_dir: Option<&Path>,
    ) -> Result<Self, String> {
        if !git_dir.exists() {
            return Err(format!(
                "Git directory does not exist: {}",
                git_dir.display()
            ));
        }

        let (tx, rx) = channel();

        let mut watcher = RecommendedWatcher::new(
            move |res| {
                let _ = tx.send(res);
            },
            Config::default().with_poll_interval(Duration::from_millis(150)),
        )
        .map_err(|e| format!("Failed to create filesystem watcher: {}", e))?;

        watcher
            .watch(git_dir, RecursiveMode::Recursive)
            .map_err(|e| format!("Failed to watch git directory: {}", e))?;

        if let Some(common) = common_dir {
            if common != git_dir && common.exists() {
                watcher
                    .watch(common, RecursiveMode::Recursive)
                    .map_err(|e| format!("Failed to watch common git directory: {}", e))?;
            }
        }

        if let Some(root) = worktree_root {
            if !root.exists() {
                return Err(format!(
                    "Repository work tree does not exist: {}",
                    root.display()
                ));
            }
            if root != git_dir {
                watcher
                    .watch(root, RecursiveMode::NonRecursive)
                    .map_err(|e| format!("Failed to watch work tree root: {}", e))?;
            }
        }

        Ok(Self {
            _watcher: watcher,
            receiver: rx,
        })
    }
}
