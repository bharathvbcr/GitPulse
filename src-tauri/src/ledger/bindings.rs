//! Which task a worktree is working on.
//!
//! A binding is not a setting stored in a config file — it is an *event*, and
//! it lives in the ledger with everything else. That choice buys two things a
//! settings file would not: the binding is timestamped, so a later audit can
//! say which task a commit was made under rather than which task the worktree
//! is bound to *now*; and it survives the same crash the rest of the history
//! survives.
//!
//! Resolution is "the most recent bind or unbind for this path wins". An
//! unbind is recorded rather than deleting the bind, for the same reason: the
//! work that happened while the binding held really did happen under it.

use super::{append, tail, ActorKind, Draft, LedgerError, Outcome};

/// The action name a binding event carries.
pub const BIND: &str = "worktree.bind";
/// The action name an unbinding event carries.
pub const UNBIND: &str = "worktree.unbind";

/// Binds `worktree_path` to `task_id`, recording it as an event.
pub fn bind(repo_path: &str, worktree_path: &str, task_id: &str) -> Result<i64, LedgerError> {
    if task_id.is_empty() {
        return Err(LedgerError::new(
            "no_task",
            "a binding must name a task; use unbind() to clear one",
        ));
    }
    append(Draft {
        repo_path: repo_path.to_string(),
        worktree_path: Some(worktree_path.to_string()),
        task_id: Some(task_id.to_string()),
        action: BIND.to_string(),
        object: Some(worktree_path.to_string()),
        actor_kind: Some(ActorKind::Human),
        outcome: Some(Outcome::Ok),
        ..Default::default()
    })
}

/// Clears whatever `worktree_path` was bound to.
pub fn unbind(repo_path: &str, worktree_path: &str) -> Result<i64, LedgerError> {
    append(Draft {
        repo_path: repo_path.to_string(),
        worktree_path: Some(worktree_path.to_string()),
        action: UNBIND.to_string(),
        object: Some(worktree_path.to_string()),
        actor_kind: Some(ActorKind::Human),
        outcome: Some(Outcome::Ok),
        ..Default::default()
    })
}

/// The task `worktree_path` is bound to, if any.
///
/// Scans the ledger for the newest bind/unbind naming this path. The scan is
/// bounded by paging rather than by a `LIMIT` on a reversed query, because the
/// ledger is append-only and the newest binding is at the end.
pub fn resolve(repo_path: &str, worktree_path: &str) -> Result<Option<String>, LedgerError> {
    let mut cursor = 0i64;
    let mut current: Option<String> = None;
    loop {
        let page = tail(repo_path, cursor, 1000)?;
        if page.is_empty() {
            break;
        }
        for event in &page {
            if event.worktree_path.as_deref() != Some(worktree_path) {
                continue;
            }
            match event.action.as_str() {
                BIND => current = event.task_id.clone(),
                UNBIND => current = None,
                _ => {}
            }
        }
        cursor = page[page.len() - 1].id;
        if page.len() < 1000 {
            break;
        }
    }
    Ok(current)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_str().unwrap().to_string();
        (dir, path)
    }

    #[test]
    fn an_unbound_worktree_resolves_to_nothing() {
        let (_d, repo) = repo();
        assert_eq!(resolve(&repo, "/wt/a").unwrap(), None);
    }

    #[test]
    fn a_binding_resolves_to_its_task() {
        let (_d, repo) = repo();
        bind(&repo, "/wt/a", "TASK-1").unwrap();
        assert_eq!(resolve(&repo, "/wt/a").unwrap(), Some("TASK-1".into()));
    }

    #[test]
    fn the_newest_binding_wins() {
        let (_d, repo) = repo();
        bind(&repo, "/wt/a", "TASK-1").unwrap();
        bind(&repo, "/wt/a", "TASK-2").unwrap();
        assert_eq!(resolve(&repo, "/wt/a").unwrap(), Some("TASK-2".into()));
    }

    #[test]
    fn unbinding_clears_it_without_erasing_the_history() {
        let (_d, repo) = repo();
        bind(&repo, "/wt/a", "TASK-1").unwrap();
        unbind(&repo, "/wt/a").unwrap();
        assert_eq!(resolve(&repo, "/wt/a").unwrap(), None);

        // The bind is still on the record: work done while it held was done
        // under that task, and an unbind does not retract that.
        let events = tail(&repo, 0, 100).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].action, BIND);
        assert_eq!(events[0].task_id.as_deref(), Some("TASK-1"));
    }

    #[test]
    fn bindings_are_per_worktree() {
        let (_d, repo) = repo();
        bind(&repo, "/wt/a", "TASK-1").unwrap();
        bind(&repo, "/wt/b", "TASK-2").unwrap();
        unbind(&repo, "/wt/a").unwrap();
        assert_eq!(resolve(&repo, "/wt/a").unwrap(), None);
        assert_eq!(resolve(&repo, "/wt/b").unwrap(), Some("TASK-2".into()));
    }

    #[test]
    fn a_binding_must_name_a_task() {
        let (_d, repo) = repo();
        assert_eq!(bind(&repo, "/wt/a", "").unwrap_err().code, "no_task");
    }

    #[test]
    fn resolution_survives_a_restart() {
        let (_d, repo) = repo();
        bind(&repo, "/wt/a", "TASK-1").unwrap();
        super::super::tests_support::reset_registry();
        assert_eq!(resolve(&repo, "/wt/a").unwrap(), Some("TASK-1".into()));
    }

    #[test]
    fn resolution_pages_past_the_first_window() {
        // The newest binding is at the END of an append-only log, so a scan
        // that stopped after one page would return a stale task forever.
        let (_d, repo) = repo();
        bind(&repo, "/wt/a", "TASK-1").unwrap();
        for i in 0..1200 {
            append(Draft {
                repo_path: repo.clone(),
                action: format!("git.commit{i}"),
                actor_kind: Some(ActorKind::Human),
                outcome: Some(Outcome::Ok),
                ..Default::default()
            })
            .unwrap();
        }
        bind(&repo, "/wt/a", "TASK-2").unwrap();
        assert_eq!(resolve(&repo, "/wt/a").unwrap(), Some("TASK-2".into()));
    }
}
