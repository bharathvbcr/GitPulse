use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver};
use std::time::Duration;

pub struct RepoFileWatcher {
    _watcher: RecommendedWatcher,
    pub receiver: Receiver<Result<notify::Event, notify::Error>>,
}

/// Cheap change fingerprint over the small set of paths whose modification
/// implies repository activity: the git dir itself plus HEAD/index/packed-refs,
/// and — when watching a checkout — the worktree root directory and its
/// immediate entries (creating or deleting a top-level file bumps the
/// directory mtime). Used by the consumer-side watchdog in `run_watch_loop`.
///
/// Returns `(path, len, mtime_nanos)` triples so additions, removals, size
/// changes, and in-place rewrites all register.
pub fn watch_fingerprint(git_dir: &Path, worktree_root: Option<&Path>) -> Vec<(PathBuf, u64, i64)> {
    fn stat_entry(path: &Path) -> Option<(PathBuf, u64, i64)> {
        let meta = std::fs::symlink_metadata(path).ok()?;
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos() as i64)
            .unwrap_or(i64::MIN);
        Some((path.to_path_buf(), meta.len(), mtime))
    }

    let mut paths = vec![
        git_dir.to_path_buf(),
        git_dir.join("HEAD"),
        git_dir.join("index"),
        git_dir.join("packed-refs"),
    ];
    if let Some(root) = worktree_root {
        paths.push(root.to_path_buf());
        if let Ok(entries) = std::fs::read_dir(root) {
            for entry in entries.flatten().take(256) {
                paths.push(entry.path());
            }
        }
    }
    paths.sort();
    paths.dedup();
    paths.iter().filter_map(|p| stat_entry(p)).collect()
}

impl RepoFileWatcher {
    /// Watches only a resolved git directory recursively (work-tree `.git`,
    /// linked-worktree git dir, or bare repo). See [`Self::watch_repo`] for
    /// the full-repo form.
    pub fn watch(git_dir: &Path) -> Result<Self, String> {
        Self::watch_repo(git_dir, None)
    }

    /// Watches a repository for the change detector: the resolved git dir
    /// recursively, and — when the repository has a separate worktree root —
    /// that root too.
    ///
    /// The worktree watch is deliberately NON-recursive (top-level entries
    /// only). Recursive watching of a whole checkout is far too hot for the
    /// debounce loop, but without *some* worktree coverage an unstaged edit
    /// never fires `repo-changed` and status goes stale. Top-level entries are
    /// where edits surface first (new/removed files, mtime churn on roots);
    /// deeper edits still reach us through the index/HEAD writes they cause
    /// inside `.git`. Both `notify` backends in use here support the mode:
    /// inotify natively, FSEvents via its own filtering.
    ///
    /// Delivery latency and reliability of those OS backends vary with system
    /// load; [`crate::watcher::run_watch_loop`] therefore cross-checks a
    /// [`watch_fingerprint`] on a short interval and reports changes even when
    /// the OS stream goes quiet.
    pub fn watch_repo(git_dir: &Path, worktree_root: Option<&Path>) -> Result<Self, String> {
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
