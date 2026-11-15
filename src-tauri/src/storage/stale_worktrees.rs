//! Linked worktrees nobody is using any more, reported with what removing each
//! would free. Read-only: nothing here removes a worktree. Removal is a person's
//! click through the existing guarded `cmd_remove_worktree`, or the cleaner's
//! opt-in prune scope, which goes through the same engine teardown.
//!
//! A worktree is stale when every one of these was checked and holds:
//! - no process has its working directory inside it (the process scan, which
//!   covers agent sessions, shells and builds alike);
//! - it is not locked and git does not already call it prunable;
//! - its tree is clean (`git status` printed nothing);
//! - no active task lease in the main checkout's store names its branch (or
//!   names none), and the worktree's own store holds none: the store is per
//!   checkout, and a lane's agent leases in its own;
//! - it has been idle at least a day (its last commit and its index, which
//!   checkout, staging and status refreshes touch), so a lane created moments
//!   ago with no commits of its own is not "integrated and finished";
//! - its HEAD is integrated into the main worktree's branch, or it has been
//!   idle for the threshold. A detached HEAD keeps its commits by HEAD
//!   alone, so it must be integrated.
//!
//! A check that could not run is never read as one that passed: the worktree
//! is then not stale, and the reason says which check could not answer.
use crate::engine::git_cli;
use crate::ipc_error::CommandError;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::{Duration, Instant};

/// Linked worktrees examined per report. Past it the rest are counted, not
/// examined, and the report says so.
const MAX_EXAMINED: usize = 64;
/// Total time spent sizing the stale ones; each gets the walk's own file cap.
const SIZING_BUDGET: Duration = Duration::from_secs(60);
/// Idle days every stale worktree needs, integrated or not. A worktree made
/// from the main branch a minute ago is "integrated" with nothing of its own
/// yet: an unstarted lane, not a finished one.
const MIN_IDLE_DAYS: u64 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StaleWorktree {
    pub path: String,
    pub name: String,
    pub branch: Option<String>,
    /// Processes working inside it; `None` when the process table could not
    /// be read.
    pub processes: Option<u32>,
    /// `git status` printed nothing; `None` when status could not run.
    pub clean: Option<bool>,
    /// Every commit at its HEAD is reachable from the main worktree's branch;
    /// `None` when that could not be asked (detached main, failed read).
    pub integrated: Option<bool>,
    /// Whole days since its last activity: the later of its HEAD commit and
    /// its index file's modification; `None` when either is unread.
    pub idle_days: Option<u64>,
    pub locked: bool,
    /// Every check above was answered and all of them hold.
    pub stale: bool,
    /// Why it is not stale, one clause per failed or unanswered check; empty
    /// when it is.
    pub reasons: Vec<String>,
    /// Logical and clone-private size, measured for stale worktrees only.
    pub size: Option<super::WorktreeUsage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StaleWorktreeReport {
    pub repo_path: String,
    /// The idle threshold applied, in days.
    pub idle_days: u32,
    /// Linked worktrees the repository lists.
    pub listed: u32,
    /// Of those, how many were examined (at most 64).
    pub examined: u32,
    pub worktrees: Vec<StaleWorktree>,
    pub issues: Vec<String>,
}

/// The process-table read, injectable for tests.
pub(crate) type ProcessesIn<'a> = &'a dyn Fn(&Path) -> Result<Vec<i32>, String>;

pub fn stale_worktrees(
    repo_path: &str,
    idle_days: u32,
) -> Result<StaleWorktreeReport, CommandError> {
    stale_worktrees_with(
        repo_path,
        idle_days,
        &crate::sysload::procscan::working_in,
        true,
        crate::storage::hygiene::global::current_time(),
    )
}

pub(crate) fn stale_worktrees_with(
    repo_path: &str,
    idle_days: u32,
    processes_in: ProcessesIn<'_>,
    measure: bool,
    now: u64,
) -> Result<StaleWorktreeReport, CommandError> {
    let repo = git_cli::validate_repo(repo_path)?;
    let infos = crate::engine::worktree::list_worktrees_lite(&repo.to_string_lossy())?;
    let main_branch = infos
        .iter()
        .find(|i| i.is_main)
        .and_then(|i| i.branch.clone());
    let linked: Vec<_> = infos.iter().filter(|i| !i.is_main && !i.is_bare).collect();
    let mut report = StaleWorktreeReport {
        repo_path: repo.to_string_lossy().into_owned(),
        idle_days,
        listed: u32::try_from(linked.len()).unwrap_or(u32::MAX),
        examined: 0,
        worktrees: vec![],
        issues: vec![],
    };
    if main_branch.is_none() {
        report.issues.push(
            "The main worktree is not on a branch, so no worktree can be judged integrated".into(),
        );
    }
    let tasks = crate::tasks::view(&repo.to_string_lossy());
    if !tasks.error.is_empty() {
        report.issues.push(format!(
            "Task leases could not be read, so no worktree is called stale: {}",
            tasks.error
        ));
    }
    let sizing_deadline = Instant::now() + SIZING_BUDGET;
    for info in linked.iter().take(MAX_EXAMINED) {
        report.examined += 1;
        let path = Path::new(&info.path);
        let mut row = StaleWorktree {
            path: info.path.clone(),
            name: info.name.clone(),
            branch: info.branch.clone(),
            processes: None,
            clean: None,
            integrated: None,
            idle_days: None,
            locked: info.is_locked,
            stale: false,
            reasons: vec![],
            size: None,
        };
        if info.is_locked {
            row.reasons.push("locked".into());
        }
        if info.is_prunable || !path.is_dir() {
            row.reasons
                .push("its directory is missing; git worktree prune clears the record".into());
            report.worktrees.push(row);
            continue;
        }
        match processes_in(path) {
            Ok(pids) => {
                row.processes = Some(u32::try_from(pids.len()).unwrap_or(u32::MAX));
                if !pids.is_empty() {
                    row.reasons
                        .push(format!("{} process(es) are working in it", pids.len()));
                }
            }
            Err(e) => row
                .reasons
                .push(format!("the process table could not be read: {e}")),
        }
        match git_cli::git_with_stdin(
            path,
            &[
                "-c",
                "core.fsmonitor=false",
                "status",
                "--porcelain=v1",
                "-z",
                "--untracked-files=normal",
            ],
            &[],
        ) {
            Ok(out) => {
                row.clean = Some(out.is_empty());
                if !out.is_empty() {
                    row.reasons.push("it has uncommitted work".into());
                }
            }
            Err(e) => row
                .reasons
                .push(format!("its status could not be read: {}", e.message())),
        }
        if let Some(main) = &main_branch {
            let range = format!("refs/heads/{main}..HEAD");
            match git_cli::git_text(path, &["rev-list", "--count", &range, "--"]) {
                Ok(count) => row.integrated = Some(count.trim() == "0"),
                Err(e) => row.reasons.push(format!(
                    "whether it is integrated could not be read: {}",
                    e.message()
                )),
            }
        }
        if !tasks.error.is_empty() {
            row.reasons.push("task leases could not be read".into());
        } else if tasks.leases.iter().any(|lease| {
            lease.branch.is_none() || lease.branch.as_deref() == info.branch.as_deref()
        }) {
            row.reasons
                .push("an active task lease may name its branch".into());
        }
        // The task store is per checkout: a lane can hold leases in its own.
        let own = crate::tasks::view(&info.path);
        if !own.error.is_empty() {
            row.reasons
                .push("its own task store could not be read".into());
        } else if !own.leases.is_empty() {
            row.reasons
                .push("its own task store holds an active lease".into());
        }
        match last_activity(path) {
            Ok(at) => row.idle_days = Some(now.saturating_sub(at) / 86_400),
            Err(e) => row
                .reasons
                .push(format!("its last activity could not be read: {e}")),
        }
        if let Some(idle) = row.idle_days {
            let unused = (row.integrated == Some(true) && idle >= MIN_IDLE_DAYS)
                || idle >= u64::from(idle_days).max(MIN_IDLE_DAYS);
            if !unused {
                row.reasons.push(if row.integrated == Some(true) {
                    format!("it was active within the last {MIN_IDLE_DAYS} day(s)")
                } else {
                    format!(
                        "its branch is not integrated and it was active within the last {idle_days} day(s)"
                    )
                });
            }
        }
        // A detached worktree's commits are held by its HEAD alone: removing
        // it leaves anything not on the main branch on no ref at all, so only
        // an integrated one can be stale, however long it has been idle.
        if info.branch.is_none() && row.integrated != Some(true) {
            row.reasons.push(
                "it is detached and not integrated, so removing it would leave its commits on no branch"
                    .into(),
            );
        }
        row.stale = row.reasons.is_empty()
            && row.processes == Some(0)
            && row.clean == Some(true)
            && row.idle_days.is_some()
            && !row.locked;
        if row.stale && measure {
            row.size = Some(super::measure_linked_worktree(
                &info.path,
                &info.name,
                info.branch.as_deref(),
                sizing_deadline,
            ));
        }
        report.worktrees.push(row);
    }
    if linked.len() > MAX_EXAMINED {
        report.issues.push(format!(
            "{} linked worktree(s) past the first {MAX_EXAMINED} were not examined",
            linked.len() - MAX_EXAMINED
        ));
    }
    Ok(report)
}

/// The later of the worktree's HEAD commit time and its index file's
/// modification time, in Unix seconds.
fn last_activity(worktree: &Path) -> Result<u64, String> {
    let committed = git_cli::git_text(worktree, &["log", "-1", "--format=%ct", "HEAD", "--"])
        .map_err(|e| e.message().to_string())?
        .trim()
        .parse::<u64>()
        .map_err(|_| "its HEAD commit time is not a number".to_string())?;
    let index = git_cli::git_text(
        worktree,
        &["rev-parse", "--path-format=absolute", "--git-path", "index"],
    )
    .map_err(|e| e.message().to_string())?;
    let touched = std::fs::metadata(index.trim())
        .and_then(|m| m.modified())
        .map_err(|e| format!("its index: {e}"))?
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_secs();
    Ok(committed.max(touched))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) {
        let mut full = vec![
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
        ];
        full.extend_from_slice(args);
        git_cli::git_text(dir, &full).unwrap();
    }

    /// A repository on `main` with three linked worktrees: one integrated and
    /// clean, one with a fresh unmerged commit, one dirty.
    fn fixture() -> (tempfile::TempDir, std::path::PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let root = git_cli::canonicalize_plain(temp.path()).unwrap();
        let repo = root.join("repo");
        std::fs::create_dir(&repo).unwrap();
        git_cli::git_global(&["init", "-q", "-b", "main", repo.to_str().unwrap()]).unwrap();
        crate::test_support::trust_repo(&repo);
        std::fs::write(repo.join("a.txt"), "a").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-qm", "base"]);
        for name in ["done", "busy", "dirty"] {
            let wt = root.join(name);
            git(
                &repo,
                &["worktree", "add", "-q", "-b", name, wt.to_str().unwrap()],
            );
            crate::test_support::trust_repo(&wt);
        }
        std::fs::write(root.join("busy/b.txt"), "b").unwrap();
        git(&root.join("busy"), &["add", "."]);
        git(&root.join("busy"), &["commit", "-qm", "fresh"]);
        std::fs::write(root.join("dirty/a.txt"), "edited").unwrap();
        (temp, root)
    }

    /// `days` days from now, as the report's clock.
    fn later(days: u64) -> u64 {
        crate::storage::hygiene::global::current_time() + days * 86_400
    }

    fn row<'a>(report: &'a StaleWorktreeReport, name: &str) -> &'a StaleWorktree {
        report
            .worktrees
            .iter()
            .find(|w| w.path.ends_with(&format!("/{name}")))
            .unwrap_or_else(|| panic!("{name} missing: {report:?}"))
    }

    #[test]
    fn only_an_unused_clean_integrated_worktree_is_stale() {
        let (_temp, root) = fixture();
        let none: ProcessesIn<'_> = &|_| Ok(vec![]);
        let report = stale_worktrees_with(
            root.join("repo").to_str().unwrap(),
            7,
            none,
            false,
            later(2),
        )
        .unwrap();
        assert_eq!(report.listed, 3);
        assert_eq!(report.examined, 3);
        let done = row(&report, "done");
        assert!(done.stale, "{done:?}");
        assert_eq!(done.integrated, Some(true));
        assert!(
            !row(&report, "busy").stale,
            "a fresh unmerged commit is in use"
        );
        let dirty = row(&report, "dirty");
        assert!(!dirty.stale);
        assert_eq!(dirty.clean, Some(false));
    }

    #[test]
    fn a_process_inside_or_an_unreadable_process_table_is_never_stale() {
        let (_temp, root) = fixture();
        let busy: ProcessesIn<'_> = &|_| Ok(vec![4242]);
        let report = stale_worktrees_with(
            root.join("repo").to_str().unwrap(),
            7,
            busy,
            false,
            later(2),
        )
        .unwrap();
        assert!(!row(&report, "done").stale);
        let unreadable: ProcessesIn<'_> = &|_| Err("table unavailable".into());
        let report = stale_worktrees_with(
            root.join("repo").to_str().unwrap(),
            7,
            unreadable,
            false,
            later(2),
        )
        .unwrap();
        let done = row(&report, "done");
        assert!(
            !done.stale,
            "a check that could not run is not a pass: {done:?}"
        );
        assert_eq!(done.processes, None);
        assert!(done.reasons.iter().any(|r| r.contains("could not be read")));
    }

    #[test]
    fn an_unmerged_branch_idle_past_the_threshold_is_stale() {
        let (_temp, root) = fixture();
        let none: ProcessesIn<'_> = &|_| Ok(vec![]);
        // Ten days on, past the 7-day threshold, an unmerged branch is idle.
        let report = stale_worktrees_with(
            root.join("repo").to_str().unwrap(),
            7,
            none,
            false,
            later(10),
        )
        .unwrap();
        assert!(row(&report, "busy").stale);
        assert!(!row(&report, "dirty").stale, "still dirty");
    }

    /// A lane made from the main branch moments ago has no commits of its own
    /// and is "integrated": it is unstarted, not finished, and never stale.
    #[test]
    fn a_just_created_lane_is_not_stale_however_integrated() {
        let (_temp, root) = fixture();
        let none: ProcessesIn<'_> = &|_| Ok(vec![]);
        let report = stale_worktrees_with(
            root.join("repo").to_str().unwrap(),
            7,
            none,
            false,
            crate::storage::hygiene::global::current_time(),
        )
        .unwrap();
        let done = row(&report, "done");
        assert_eq!(done.integrated, Some(true));
        assert!(!done.stale, "{done:?}");
        assert!(
            done.reasons.iter().any(|r| r.contains("active within")),
            "{done:?}"
        );
    }

    /// A detached worktree holds its commits by HEAD alone; idle or not, it
    /// is stale only when those commits are on the main branch.
    #[test]
    fn a_detached_unintegrated_worktree_is_never_stale() {
        let (_temp, root) = fixture();
        let busy = root.join("busy");
        git(&busy, &["checkout", "-q", "--detach"]);
        git(&root.join("repo"), &["branch", "-D", "busy"]);
        let none: ProcessesIn<'_> = &|_| Ok(vec![]);
        let report = stale_worktrees_with(
            root.join("repo").to_str().unwrap(),
            7,
            none,
            false,
            later(30),
        )
        .unwrap();
        let row = row(&report, "busy");
        assert_eq!(row.branch, None);
        assert_eq!(row.integrated, Some(false));
        assert!(!row.stale, "{row:?}");
        assert!(
            row.reasons.iter().any(|r| r.contains("detached")),
            "{row:?}"
        );
    }
    /// The task store is per checkout: a lane's agent leases its task in the
    /// lane's own `.devcouncil/state.sqlite`, which the repository's store
    /// never sees. A lease there keeps the worktree out of the stale list.
    #[test]
    fn a_lease_in_the_worktrees_own_task_store_is_never_stale() {
        let (_temp, root) = fixture();
        // Ignored, as GitPulse's own repository ignores it, so the store
        // does not make the tree dirty and the lease is what is judged.
        std::fs::write(root.join("repo/.git/info/exclude"), ".devcouncil/\n").unwrap();
        let db = crate::tasks::store_path(root.join("done").to_str().unwrap());
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        drop(dc_store::Store::open(&db).unwrap());
        rusqlite::Connection::open(&db)
            .unwrap()
            .execute(
                "INSERT INTO task_leases (id, task_id, owner, lease_token, status, created_at, expires_at)
                 VALUES ('l', 'TASK-1', 'agent', 'token', 'active', '2026-01-01T00:00:00Z', '2999-01-01T00:00:00Z')",
                [],
            )
            .unwrap();
        let none: ProcessesIn<'_> = &|_| Ok(vec![]);
        let report = stale_worktrees_with(
            root.join("repo").to_str().unwrap(),
            7,
            none,
            false,
            later(2),
        )
        .unwrap();
        let done = row(&report, "done");
        assert_eq!(done.clean, Some(true), "{done:?}");
        assert!(!done.stale, "{done:?}");
        assert!(
            done.reasons.iter().any(|r| r.contains("own task store")),
            "{done:?}"
        );
    }
}
