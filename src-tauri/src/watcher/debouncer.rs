use notify::{Config, Event, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::Path;
use std::sync::mpsc::{channel, Receiver};
use std::time::Duration;

pub struct RepoFileWatcher {
    _watcher: RecommendedWatcher,
    pub receiver: Receiver<Result<Event, notify::Error>>,
}

impl RepoFileWatcher {
    /// Watches a resolved git directory recursively (work-tree `.git`, worktree git dir, or bare repo).
    pub fn watch(git_dir: &Path) -> Result<Self, String> {
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

        Ok(Self {
            _watcher: watcher,
            receiver: rx,
        })
    }
}
