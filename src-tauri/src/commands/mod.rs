use crate::analyzer::{
    CommitFilter, ConventionalCommit, ConventionalCommitParser, CoverageReport, CoverageScanner,
    DepsHealthReport, DepsScanner, FileCoverage, LanguageDetector, LanguageInfo, LineCounts,
    LocCounter,
};
use crate::diff::{
    compute_word_diff, ConflictDocument, ConflictResolver, FilePatch, IntraLineDiff, PatchBuilder,
};
use crate::engine::git_cli::{resolve_repo, sandbox_write, ResolvedRepo};
use crate::engine::git_reader::{
    BlameLine, CommitDetails, CommitFileChange, FileBlob, ReflogEntry, RepoLanguageStat,
};
use crate::engine::git_writer::RebaseStep;
use crate::engine::{
    BranchInfo, BranchStatsReport, FileStatus, GitReader, GitWriter, TagInfo, WorktreeInfo,
};
use crate::github::{
    checkout_pull_request, create_issue, load_dependabot_alerts, load_github_context,
    validate_issue_payload, DependabotReport, GitHubContext,
};
use crate::graph::{
    BezierGeometryCalculator, BranchFoldingEngine, CubicBezierCurve, FoldedBranchRun, LaneSolver,
    VisualCommitRow,
};
use crate::stack::{StackTreeEngine, StackedBranchNode};
use crate::watcher::{start_watch, unwatch, WatcherState};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use tauri::{AppHandle, State};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleasePublishResult {
    pub tag: String,
    pub remote: String,
    pub created_tag: bool,
    pub tag_policy: Option<crate::harness::PolicyVerdict>,
    pub output: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitGraphPayload {
    pub rows: Vec<VisualCommitRow>,
    pub folds: Vec<FoldedBranchRun>,
    pub head_id: Option<String>,
    /// Branches, remotes and tags pointing at rows in this graph.
    pub refs: Vec<crate::graph::RefDecoration>,
    /// True when the walk hit its row cap and older commits exist. The client
    /// offers "load more" instead of silently hiding the rest of history.
    pub has_more: bool,
}

#[tauri::command(async)]
pub fn cmd_pick_folder() -> Option<String> {
    rfd::FileDialog::new()
        .set_title("Select Git Repository Directory")
        .pick_folder()
        .map(|p| p.to_string_lossy().to_string())
}

/// Long-running commands are `async fn`, but that alone only moves them off
/// the main thread: their bodies would still run as blocking code on the
/// shared async runtime's worker threads, starving every other command. Each
/// heavy body therefore runs through [`off_thread`], Tauri's blocking pool,
/// so an agent hammering status/fetch/graph in parallel keeps the runtime
/// responsive.
#[tauri::command(async)]
pub async fn cmd_list_branches(repo_path: String) -> Result<Vec<BranchInfo>, String> {
    off_thread(move || GitReader::list_branches(&repo_path)).await
}

/// Backfills per-branch churn after list_branches has rendered: the heavy
/// shortstat walks stay off the initial load and memoize through the oid-keyed
/// churn cache, so repeat calls while browsing are cheap.
#[tauri::command(async)]
pub async fn cmd_branch_stats(repo_path: String) -> Result<BranchStatsReport, String> {
    off_thread(move || GitReader::branch_stats(&repo_path)).await
}

/// Runs a blocking body on Tauri's blocking thread pool instead of an async
/// worker thread. Every git/gh/npm subprocess call, repository scan and
/// sidecar round trip below goes through this seam.
async fn off_thread<F, T>(body: F) -> Result<T, String>
where
    F: FnOnce() -> Result<T, String> + Send + 'static,
    T: Send + 'static,
{
    tauri::async_runtime::spawn_blocking(body)
        .await
        .map_err(|e| format!("background task failed: {e}"))?
}

#[tauri::command(async)]
pub async fn cmd_get_status(repo_path: String) -> Result<Vec<FileStatus>, String> {
    off_thread(move || GitReader::get_status(&repo_path)).await
}

#[tauri::command(async)]
pub async fn cmd_get_commit_graph(
    repo_path: String,
    max_commits: usize,
    query: Option<String>,
    revision: Option<String>,
    skip: Option<usize>,
) -> Result<CommitGraphPayload, String> {
    off_thread(move || {
        // One row over the cap: the extra row is the has_more probe and is
        // dropped before lanes are solved.
        let fetch_limit = max_commits.clamp(1, GitReader::MAX_HISTORY_COMMITS) + 1;
        let repo = repo_path.clone();
        let rev = revision.clone();
        // History is the long pole; HEAD and ref decorations are independent of
        // it, so they run concurrently instead of serializing three walks behind
        // one another on a large repository.
        let (history, head_id, refs) = std::thread::scope(|scope| {
            let history = scope.spawn(move || {
                GitReader::read_commit_history_paged(
                    &repo,
                    skip.unwrap_or(0),
                    fetch_limit,
                    rev.as_deref(),
                )
            });
            let repo2 = repo_path.clone();
            let head = scope.spawn(move || GitReader::head_id(&repo2).ok());
            let repo3 = repo_path.clone();
            let refs =
                scope.spawn(move || crate::graph::list_ref_decorations(&repo3).unwrap_or_default());
            (
                history
                    .join()
                    .unwrap_or_else(|_| Err("history walker panicked".into())),
                head.join().unwrap_or(None),
                refs.join().unwrap_or_default(),
            )
        });

        let mut raw_commits = history?;
        let has_more = raw_commits.len() > max_commits;
        if has_more {
            raw_commits.truncate(max_commits);
        }

        let filter = query
            .as_deref()
            .map(CommitFilter::parse)
            .unwrap_or_default();
        if let Some(ref path) = filter.path {
            let touching: HashSet<String> =
                GitReader::commits_touching_path(&repo_path, path, fetch_limit)?
                    .into_iter()
                    .collect();
            raw_commits.retain(|c| touching.contains(&c.id));
        }
        if !filter.is_empty() {
            raw_commits.retain(|c| filter.matches_commit(c));
        }

        let mut folding = BranchFoldingEngine::new();
        folding.identify_foldable_runs(&raw_commits);
        let folds = folding.get_foldable_runs().values().cloned().collect();

        let mut solver = LaneSolver::new(12);
        let rows = solver.solve(&raw_commits);
        Ok(CommitGraphPayload {
            rows,
            folds,
            head_id,
            refs,
            has_more,
        })
    })
    .await
}

#[tauri::command(async)]
pub async fn cmd_get_file_diff(
    repo_path: String,
    file_path: String,
    is_staged: bool,
    ignore_whitespace: Option<bool>,
) -> Result<String, String> {
    off_thread(move || {
        GitReader::get_file_diff(
            &repo_path,
            &file_path,
            is_staged,
            ignore_whitespace.unwrap_or(false),
        )
    })
    .await
}

#[tauri::command(async)]
pub async fn cmd_get_commit_diff(repo_path: String, commit_id: String) -> Result<String, String> {
    off_thread(move || GitReader::get_commit_diff(&repo_path, &commit_id)).await
}

#[tauri::command(async)]
pub async fn cmd_get_range_diff(
    repo_path: String,
    from: String,
    to: String,
) -> Result<String, String> {
    off_thread(move || GitReader::get_range_diff(&repo_path, &from, &to)).await
}

#[tauri::command(async)]
pub async fn cmd_get_commit_files(
    repo_path: String,
    commit_id: String,
) -> Result<Vec<CommitFileChange>, String> {
    off_thread(move || GitReader::get_commit_files(&repo_path, &commit_id)).await
}

#[tauri::command(async)]
pub async fn cmd_get_commit_details(
    repo_path: String,
    commit_id: String,
) -> Result<CommitDetails, String> {
    off_thread(move || GitReader::get_commit_details(&repo_path, &commit_id)).await
}

#[tauri::command(async)]
pub async fn cmd_get_file_content(
    repo_path: String,
    file_path: String,
    commit_id: Option<String>,
) -> Result<String, String> {
    off_thread(move || GitReader::get_file_content(&repo_path, &file_path, commit_id.as_deref()))
        .await
}

#[tauri::command(async)]
pub async fn cmd_get_file_blob(
    repo_path: String,
    file_path: String,
    commit_id: Option<String>,
) -> Result<FileBlob, String> {
    off_thread(move || GitReader::get_file_blob(&repo_path, &file_path, commit_id.as_deref())).await
}

/// Writes a file inside the repository, after the write gate has judged the
/// path. This is the conflict editor's save path, so it is a real edit and is
/// gated as one.
#[tauri::command(async)]
pub async fn cmd_write_file_content(
    repo_path: String,
    file_path: String,
    content: String,
) -> Result<Guarded<()>, String> {
    off_thread(move || {
        let policy = crate::harness::check_file(&repo_path, &file_path, "modify");
        if policy.blocks() {
            return Err(policy.refusal());
        }
        sandbox_write(&repo_path, &file_path, &content)?;
        Ok(Guarded { policy, output: () })
    })
    .await
}

#[tauri::command(async)]
pub fn cmd_compute_word_diff(old_line: String, new_line: String) -> IntraLineDiff {
    compute_word_diff(&old_line, &new_line)
}

#[tauri::command(async)]
pub async fn cmd_stage_file(repo_path: String, file_path: String) -> Result<(), String> {
    off_thread(move || GitWriter::stage_file(&repo_path, &file_path)).await
}

#[tauri::command(async)]
pub async fn cmd_unstage_file(repo_path: String, file_path: String) -> Result<(), String> {
    off_thread(move || GitWriter::unstage_file(&repo_path, &file_path)).await
}

#[tauri::command(async)]
pub async fn cmd_stage_selective_patch(
    repo_path: String,
    file_patch: FilePatch,
) -> Result<(), String> {
    off_thread(move || {
        // Paths and line contents are interpolated verbatim into the patch
        // text; validate before anything reaches `git apply`.
        PatchBuilder::validate_file_patch(&file_patch)?;
        let patch = PatchBuilder::build_selective_patch(&file_patch, true);
        GitWriter::apply_patch_to_index(&repo_path, &patch)
    })
    .await
}

/// Commits the index, after the harness's command gate has judged the exact
/// command line this would run.
#[tauri::command(async)]
pub async fn cmd_commit(
    repo_path: String,
    message: String,
    amend: bool,
) -> Result<Guarded<String>, String> {
    off_thread(move || {
        let mut argv = vec!["git", "commit", "-m", message.as_str()];
        if amend {
            argv.push("--amend");
        }
        let policy = guard(&repo_path, &argv)?;
        let output = GitWriter::commit(&repo_path, &message, amend)?;
        Ok(Guarded { policy, output })
    })
    .await
}

#[tauri::command(async)]
pub async fn cmd_checkout_branch(repo_path: String, branch_name: String) -> Result<(), String> {
    off_thread(move || GitWriter::checkout_branch(&repo_path, &branch_name)).await
}

#[tauri::command(async)]
pub async fn cmd_create_branch(
    repo_path: String,
    branch_name: String,
    start_point: Option<String>,
) -> Result<(), String> {
    off_thread(move || GitWriter::create_branch(&repo_path, &branch_name, start_point.as_deref()))
        .await
}

#[tauri::command(async)]
pub async fn cmd_delete_branch(
    repo_path: String,
    branch_name: String,
    force: bool,
) -> Result<Guarded<()>, String> {
    off_thread(move || {
        let flag = if force { "-D" } else { "-d" };
        let policy = guard(&repo_path, &["git", "branch", flag, branch_name.as_str()])?;
        GitWriter::delete_branch(&repo_path, &branch_name, force)?;
        Ok(Guarded { policy, output: () })
    })
    .await
}

#[tauri::command(async)]
pub async fn cmd_rename_branch(
    repo_path: String,
    old_name: String,
    new_name: String,
) -> Result<Guarded<()>, String> {
    off_thread(move || {
        let policy = guard(
            &repo_path,
            &["git", "branch", "-m", old_name.as_str(), new_name.as_str()],
        )?;
        GitWriter::rename_branch(&repo_path, &old_name, &new_name)?;
        Ok(Guarded { policy, output: () })
    })
    .await
}

#[tauri::command(async)]
pub async fn cmd_clone_repo(url: String, target_dir: String) -> Result<String, String> {
    off_thread(move || GitWriter::clone_repo(&url, &target_dir)).await
}

#[tauri::command(async)]
pub fn cmd_parse_conflict(file_path: String, content: String) -> ConflictDocument {
    ConflictResolver::parse(&file_path, &content)
}

#[tauri::command(async)]
pub fn cmd_resolve_conflict(document: ConflictDocument) -> Result<String, String> {
    ConflictResolver::render_resolved(&document).map_err(|e| e.to_string())
}

#[tauri::command(async)]
pub fn cmd_detect_language(file_path: String) -> LanguageInfo {
    LanguageDetector::detect_from_path(&file_path)
}

#[tauri::command(async)]
pub fn cmd_count_loc(content: String, comment_prefix: Option<String>) -> LineCounts {
    LocCounter::count(&content, comment_prefix.as_deref())
}

#[tauri::command(async)]
pub fn cmd_parse_conventional_commit(message: String) -> Option<ConventionalCommit> {
    ConventionalCommitParser::new().parse(&message)
}

#[tauri::command(async)]
pub async fn cmd_get_file_blame(
    repo_path: String,
    file_path: String,
) -> Result<Vec<BlameLine>, String> {
    off_thread(move || GitReader::get_file_blame(&repo_path, &file_path)).await
}

#[tauri::command(async)]
pub async fn cmd_rebase_interactive(
    repo_path: String,
    onto_commit: String,
    steps: Vec<RebaseStep>,
) -> Result<Guarded<()>, String> {
    off_thread(move || {
        let policy = guard(
            &repo_path,
            &["git", "rebase", "--onto", onto_commit.as_str()],
        )?;
        GitWriter::execute_rebase_sequence(&repo_path, &onto_commit, &steps)?;
        Ok(Guarded { policy, output: () })
    })
    .await
}

#[tauri::command(async)]
pub async fn cmd_get_stack_hierarchy(
    repo_path: String,
    default_branch: String,
) -> Result<Vec<StackedBranchNode>, String> {
    off_thread(move || {
        let branches = GitReader::list_branches(&repo_path)?;
        let mut branch_tips = HashMap::new();
        for b in branches {
            if !b.is_remote {
                branch_tips.insert(b.name, b.tip_commit_id);
            }
        }

        let raw_commits = GitReader::read_commit_history(&repo_path, 2000, None)?;
        let mut commit_parents = HashMap::new();
        for c in raw_commits {
            commit_parents.insert(c.id, c.parent_ids);
        }

        Ok(StackTreeEngine::build_stack_hierarchy(
            &branch_tips,
            &commit_parents,
            &default_branch,
        ))
    })
    .await
}

#[tauri::command(async)]
pub fn cmd_get_bezier_connector(
    from_lane: u32,
    from_row: usize,
    to_lane: u32,
    to_row: usize,
    color_index: u32,
    is_merge: bool,
) -> CubicBezierCurve {
    let calc = BezierGeometryCalculator::new(26.0, 36.0, 20.0, 18.0);
    calc.calculate_connector(from_lane, from_row, to_lane, to_row, color_index, is_merge)
}

#[tauri::command(async)]
pub async fn cmd_list_tags(repo_path: String) -> Result<Vec<TagInfo>, String> {
    off_thread(move || GitReader::list_tags(&repo_path)).await
}

#[tauri::command(async)]
pub async fn cmd_get_reflog(
    repo_path: String,
    max_entries: Option<usize>,
) -> Result<Vec<ReflogEntry>, String> {
    off_thread(move || GitReader::get_reflog(&repo_path, max_entries.unwrap_or(200))).await
}

#[tauri::command(async)]
pub async fn cmd_get_language_stats(repo_path: String) -> Result<Vec<RepoLanguageStat>, String> {
    off_thread(move || GitReader::get_repo_language_stats(&repo_path)).await
}

#[tauri::command(async)]
pub async fn cmd_scan_coverage(repo_path: String) -> Result<CoverageReport, String> {
    off_thread(move || CoverageScanner::scan(&repo_path)).await
}

#[tauri::command(async)]
pub async fn cmd_get_file_coverage(
    repo_path: String,
    file_path: String,
) -> Result<FileCoverage, String> {
    off_thread(move || CoverageScanner::file_coverage(&repo_path, &file_path)).await
}

#[tauri::command(async)]
pub async fn cmd_scan_deps_health(repo_path: String) -> Result<DepsHealthReport, String> {
    off_thread(move || DepsScanner::scan(&repo_path)).await
}

#[tauri::command(async)]
pub async fn cmd_branch_cleanup_plan(
    repo_path: String,
) -> Result<crate::ops::BranchCleanupPlan, String> {
    off_thread(move || crate::ops::branch_cleanup_plan(&repo_path)).await
}

#[tauri::command(async)]
pub async fn cmd_review_outgoing_commits(
    repo_path: String,
) -> Result<crate::ops::CommitReviewReport, String> {
    off_thread(move || crate::ops::review_outgoing_commits(&repo_path)).await
}

#[tauri::command(async)]
pub async fn cmd_fetch(repo_path: String, remote: Option<String>) -> Result<String, String> {
    off_thread(move || GitWriter::fetch(&repo_path, remote.as_deref())).await
}

#[tauri::command(async)]
pub async fn cmd_pull(
    repo_path: String,
    remote: Option<String>,
    branch: Option<String>,
) -> Result<Guarded<String>, String> {
    off_thread(move || {
        // Pull is gated and fetch is not: a pull merges into the working tree, and
        // the working tree is what the write gate exists to protect. A fetch only
        // moves remote-tracking refs, and gating every one would put a sidecar
        // round trip in front of a background refresh for no decision.
        let mut argv = vec!["git", "pull"];
        if let Some(ref r) = remote {
            argv.push(r.as_str());
        }
        if let Some(ref b) = branch {
            argv.push(b.as_str());
        }
        let policy = guard(&repo_path, &argv)?;
        let output = GitWriter::pull(&repo_path, remote.as_deref(), branch.as_deref())?;
        Ok(Guarded { policy, output })
    })
    .await
}

/// Pushes, after the command gate has judged it. A force push is refused by
/// the harness's hard rules, which is the whole reason this call is gated.
#[tauri::command(async)]
pub async fn cmd_push(
    repo_path: String,
    remote: Option<String>,
    branch: Option<String>,
    force: Option<bool>,
) -> Result<Guarded<String>, String> {
    off_thread(move || {
        let force = force.unwrap_or(false);
        let mut argv = vec!["git", "push"];
        if force {
            argv.push("--force");
        }
        if let Some(ref r) = remote {
            argv.push(r.as_str());
        }
        if let Some(ref b) = branch {
            argv.push(b.as_str());
        }
        let policy = guard(&repo_path, &argv)?;
        let output = GitWriter::push(&repo_path, remote.as_deref(), branch.as_deref(), force)?;
        Ok(Guarded { policy, output })
    })
    .await
}

#[tauri::command(async)]
pub async fn cmd_merge_branch(
    repo_path: String,
    branch_name: String,
    ff_only: Option<bool>,
) -> Result<Guarded<String>, String> {
    off_thread(move || {
        let ff_only = ff_only.unwrap_or(false);
        let mut argv = vec!["git", "merge"];
        if ff_only {
            argv.push("--ff-only");
        }
        argv.push(branch_name.as_str());
        let policy = guard(&repo_path, &argv)?;
        let output = GitWriter::merge_branch(&repo_path, &branch_name, ff_only)?;
        Ok(Guarded { policy, output })
    })
    .await
}

#[tauri::command(async)]
pub async fn cmd_restack(
    repo_path: String,
    branch: String,
    onto: String,
) -> Result<String, String> {
    off_thread(move || GitWriter::restack(&repo_path, &branch, &onto)).await
}

#[tauri::command(async)]
pub async fn cmd_stash_save(repo_path: String, message: Option<String>) -> Result<String, String> {
    off_thread(move || GitWriter::stash_save(&repo_path, message.as_deref())).await
}

#[tauri::command(async)]
pub async fn cmd_stash_pop(repo_path: String) -> Result<String, String> {
    off_thread(move || GitWriter::stash_pop(&repo_path)).await
}

/// Throws away a file's working-tree changes, after the write gate has judged
/// the path. Discarding is a write to the file, and the hard rules that protect
/// credential and restricted paths apply to it exactly as they do to an edit.
#[tauri::command(async)]
pub async fn cmd_discard_changes(
    repo_path: String,
    file_path: String,
) -> Result<Guarded<()>, String> {
    off_thread(move || {
        let policy = crate::harness::check_file(&repo_path, &file_path, "modify");
        if policy.blocks() {
            return Err(policy.refusal());
        }
        GitWriter::discard_changes(&repo_path, &file_path)?;
        Ok(Guarded { policy, output: () })
    })
    .await
}

#[tauri::command(async)]
pub async fn cmd_github_context(repo_path: String) -> GitHubContext {
    // `load_github_context` shells out to `gh` (up to 45s per call), so it
    // must not occupy an async worker thread. A pool failure surfaces the
    // same degraded context a missing CLI would.
    off_thread(move || Ok::<_, String>(load_github_context(&repo_path)))
        .await
        .unwrap_or_else(|e| GitHubContext {
            available: false,
            cli_present: false,
            host: String::new(),
            owner: String::new(),
            repo: String::new(),
            html_url: String::new(),
            pull_requests: Vec::new(),
            workflow_runs: Vec::new(),
            runs_error: None,
            error: Some(e),
            issues: Vec::new(),
            issues_error: None,
            issues_truncated: false,
        })
}

/// Open Dependabot alerts for the Health view, via `gh api`. Like
/// [`cmd_github_context`], the report carries its own error state instead of
/// rejecting: "could not check" must stay distinct from "no alerts".
#[tauri::command(async)]
pub async fn cmd_github_dependabot_alerts(repo_path: String) -> DependabotReport {
    off_thread(move || Ok::<_, String>(load_dependabot_alerts(&repo_path)))
        .await
        .unwrap_or_else(|e| DependabotReport {
            available: false,
            cli_present: false,
            is_github_remote: false,
            slug: String::new(),
            alerts: Vec::new(),
            truncated: false,
            error: Some(e),
        })
}

#[tauri::command(async)]
pub async fn cmd_github_create_issue(
    repo_path: String,
    title: String,
    body: String,
    labels: Vec<String>,
) -> Result<Guarded<String>, String> {
    off_thread(move || {
        validate_issue_payload(&title, &body, &labels)?;
        let mut argv = vec![
            "gh".to_string(),
            "issue".to_string(),
            "create".to_string(),
            "--title".to_string(),
            title.trim().to_string(),
            "--body".to_string(),
            body.clone(),
        ];
        for label in labels
            .iter()
            .map(|label| label.trim())
            .filter(|label| !label.is_empty())
        {
            argv.push("--label".to_string());
            argv.push(label.to_string());
        }
        let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
        let policy = guard(&repo_path, &refs)?;
        let output = create_issue(&repo_path, &title, &body, &labels)?;
        Ok(Guarded { policy, output })
    })
    .await
}

#[tauri::command(async)]
pub async fn cmd_github_checkout_pr(
    repo_path: String,
    number: u64,
) -> Result<Guarded<String>, String> {
    off_thread(move || {
        let n = number.to_string();
        let policy = guard(&repo_path, &["gh", "pr", "checkout", n.as_str()])?;
        let output = checkout_pull_request(&repo_path, number)?;
        Ok(Guarded { policy, output })
    })
    .await
}

#[tauri::command(async)]
pub async fn cmd_resolve_repo(repo_path: String) -> Result<ResolvedRepo, String> {
    off_thread(move || resolve_repo(&repo_path)).await
}

// ---------------------------------------------------------------------------
// Linked worktrees: how agents parallelize, so they are first-class here.
// ---------------------------------------------------------------------------

#[tauri::command(async)]
pub async fn cmd_list_worktrees(repo_path: String) -> Result<Vec<WorktreeInfo>, String> {
    // Spawns one `git status` per worktree (capped), so it belongs on the
    // blocking pool like every other reader.
    off_thread(move || crate::engine::worktree::list_worktrees(&repo_path)).await
}

/// Creates a linked worktree, after the command gate has judged the exact
/// `git worktree add` line this would run.
#[tauri::command(async)]
pub async fn cmd_add_worktree(
    repo_path: String,
    target_path: String,
    new_branch: Option<String>,
    start_point: Option<String>,
    detach: bool,
) -> Result<Guarded<String>, String> {
    off_thread(move || {
        let argv_owned = crate::engine::worktree::add_worktree_argv(
            &target_path,
            new_branch.as_deref(),
            start_point.as_deref(),
            detach,
        );
        let refs: Vec<&str> = argv_owned.iter().map(String::as_str).collect();
        let policy = guard(&repo_path, &refs)?;
        let created = crate::engine::worktree::add_worktree(
            &repo_path,
            &target_path,
            new_branch.as_deref(),
            start_point.as_deref(),
            detach,
        )?;
        Ok(Guarded {
            policy,
            output: created,
        })
    })
    .await
}

/// Removes a linked worktree, gated like every other destructive write.
#[tauri::command(async)]
pub async fn cmd_remove_worktree(
    repo_path: String,
    target_path: String,
    force: bool,
) -> Result<Guarded<()>, String> {
    off_thread(move || {
        let flag = if force { "--force" } else { "remove" };
        let mut argv = vec!["git", "worktree", "remove"];
        if force {
            argv.push(flag);
        }
        argv.push(target_path.as_str());
        let policy = guard(&repo_path, &argv)?;
        crate::engine::worktree::remove_worktree(&repo_path, &target_path, force)?;
        Ok(Guarded { policy, output: () })
    })
    .await
}

#[tauri::command(async)]
pub async fn cmd_watch_repo(
    app: AppHandle,
    state: State<'_, WatcherState>,
    repo_path: String,
) -> Result<String, String> {
    start_watch(app, &state, repo_path)
}

#[tauri::command(async)]
pub async fn cmd_unwatch_repo(
    state: State<'_, WatcherState>,
    repo_path: String,
) -> Result<(), String> {
    unwatch(&state, repo_path)
}

#[tauri::command(async)]
pub async fn cmd_create_tag(
    repo_path: String,
    tag_name: String,
    commit_id: Option<String>,
    message: Option<String>,
) -> Result<(), String> {
    off_thread(move || {
        GitWriter::create_tag(
            &repo_path,
            &tag_name,
            commit_id.as_deref(),
            message.as_deref(),
        )
    })
    .await
}

#[tauri::command(async)]
pub async fn cmd_delete_tag(repo_path: String, tag_name: String) -> Result<(), String> {
    off_thread(move || GitWriter::delete_tag(&repo_path, &tag_name)).await
}

/// Creates (or resumes) an annotated release tag and pushes exactly that tag.
/// Both commands are policy-checked before the first mutation, so a refusal
/// cannot strand a tag created by a previously unchecked second step.
#[tauri::command(async)]
pub async fn cmd_publish_release(
    repo_path: String,
    tag: String,
    message: String,
) -> Result<Guarded<ReleasePublishResult>, String> {
    off_thread(move || {
        let plan = crate::ops::prepare_release(&repo_path, &tag, &message)?;
        let tag_policy = if plan.create_tag {
            Some(guard(
                &repo_path,
                &[
                    "git",
                    "tag",
                    "-a",
                    plan.tag.as_str(),
                    "-m",
                    plan.message.as_str(),
                ],
            )?)
        } else {
            None
        };
        let tag_ref = format!("refs/tags/{}", plan.tag);
        let push_policy = guard(
            &repo_path,
            &["git", "push", plan.remote.as_str(), tag_ref.as_str()],
        )?;

        if plan.create_tag {
            GitWriter::create_tag(&repo_path, &plan.tag, None, Some(&plan.message))?;
        }
        let output =
            GitWriter::push_tag(&repo_path, &plan.remote, &plan.tag).map_err(|error| {
                format!(
                    "Release tag '{}' exists locally but could not be pushed: {}. Retry the release action after fixing connectivity.",
                    plan.tag, error
                )
            })?;
        Ok(Guarded {
            policy: push_policy,
            output: ReleasePublishResult {
                tag: plan.tag,
                remote: plan.remote,
                created_tag: plan.create_tag,
                tag_policy,
                output,
            },
        })
    })
    .await
}

// ---------------------------------------------------------------------------
// MANVI harness: policy-gated Git actions, and the local AI features.
// ---------------------------------------------------------------------------

/// A Git action's result together with the policy decision it ran under.
///
/// The verdict travels with every guarded action so the UI can distinguish an
/// action the gate approved from one that ran with no gate available. They are
/// not the same event and are never rendered the same way.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Guarded<T> {
    pub policy: crate::harness::PolicyVerdict,
    pub output: T,
}

/// Evaluates one command line, refusing the action when a rule fires.
fn guard(repo_path: &str, argv: &[&str]) -> Result<crate::harness::PolicyVerdict, String> {
    let command = crate::harness::render_command(argv);
    let verdict = crate::harness::check_command(repo_path, &command);
    if verdict.blocks() {
        return Err(verdict.refusal());
    }
    Ok(verdict)
}

/// Degraded status used when the probe task itself failed to run (pool join
/// error). Mirrors the shape `HarnessStatus::probe` produces for a dead
/// sidecar, so the UI cannot tell the two apart by structure.
fn harness_join_failure(error: String) -> crate::harness::HarnessStatus {
    crate::harness::HarnessStatus {
        available: false,
        binary: String::new(),
        protocol: 0,
        posture: String::new(),
        ops: Vec::new(),
        error,
        error_code: "unavailable".into(),
    }
}

#[tauri::command(async)]
pub async fn cmd_harness_status() -> crate::harness::HarnessStatus {
    // Probing can spawn the sidecar and wait out a 20s handshake; keep that
    // off the async workers.
    off_thread(|| Ok::<_, String>(crate::harness::HarnessStatus::probe()))
        .await
        .unwrap_or_else(|e| harness_join_failure(e.to_string()))
}

/// Drops the current sidecar and clears its backoff, so the next call starts a
/// fresh one. The UI's "reconnect" affordance after installing MANVI.
#[tauri::command(async)]
pub async fn cmd_harness_reconnect() -> crate::harness::HarnessStatus {
    off_thread(|| {
        crate::harness::sidecar::reset();
        Ok::<_, String>(crate::harness::HarnessStatus::probe())
    })
    .await
    .unwrap_or_else(|e| harness_join_failure(e.to_string()))
}

#[tauri::command(async)]
pub async fn cmd_policy_check_command(
    repo_path: String,
    command: String,
) -> crate::harness::PolicyVerdict {
    let command_for_fallback = command.clone();
    off_thread(move || Ok::<_, String>(crate::harness::check_command(&repo_path, &command)))
        .await
        .unwrap_or_else(|e| {
            crate::harness::PolicyVerdict::unchecked(
                &command_for_fallback,
                &crate::harness::HarnessError::Unavailable(e.to_string()),
            )
        })
}

#[tauri::command(async)]
pub async fn cmd_ai_status(base_url: Option<String>, model: Option<String>) -> crate::ai::AiStatus {
    // Endpoint discovery probes local HTTP servers with real timeouts.
    off_thread(move || Ok::<_, String>(crate::ai::status(base_url.as_deref(), model.as_deref())))
        .await
        .unwrap_or_else(|e| crate::ai::AiStatus {
            harness: harness_join_failure(e.to_string()),
            endpoints: Vec::new(),
            selected: None,
            model_info: None,
            model_detail: String::new(),
            ready: false,
            detail: e.to_string(),
        })
}

fn selection(base_url: Option<String>, model: Option<String>) -> Option<crate::ai::AiSelection> {
    match (base_url, model) {
        (Some(base_url), Some(model)) if !base_url.is_empty() && !model.is_empty() => {
            Some(crate::ai::AiSelection { base_url, model })
        }
        _ => None,
    }
}

#[tauri::command(async)]
pub async fn cmd_ai_generate_commit_message(
    repo_path: String,
    base_url: Option<String>,
    model: Option<String>,
) -> Result<crate::ai::AiGeneration, String> {
    off_thread(move || crate::ai::generate_commit_message(&repo_path, selection(base_url, model)))
        .await
}

#[tauri::command(async)]
pub async fn cmd_ai_explain_commit(
    repo_path: String,
    commit_id: String,
    base_url: Option<String>,
    model: Option<String>,
) -> Result<crate::ai::AiGeneration, String> {
    off_thread(move || {
        crate::ai::explain_commit(&repo_path, &commit_id, selection(base_url, model))
    })
    .await
}

#[tauri::command(async)]
pub async fn cmd_ai_suggest_branch_name(
    repo_path: String,
    base_url: Option<String>,
    model: Option<String>,
) -> Result<crate::ai::AiGeneration, String> {
    off_thread(move || crate::ai::suggest_branch_name(&repo_path, selection(base_url, model))).await
}

#[tauri::command(async)]
pub async fn cmd_ai_fix_health(
    repo_path: String,
    report: String,
    base_url: Option<String>,
    model: Option<String>,
) -> Result<crate::ai::AiGeneration, String> {
    off_thread(move || crate::ai::fix_health(&repo_path, &report, selection(base_url, model))).await
}
