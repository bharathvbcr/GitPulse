use crate::analyzer::{
    CommitFilter, ConventionalCommit, ConventionalCommitParser, CoverageReport, CoverageScanner,
    DepsHealthReport, DepsScanner, FileCoverage, LanguageDetector, LanguageInfo, LineCounts,
    LocCounter,
};
use crate::diff::{
    compute_word_diff, ConflictDocument, ConflictResolver, FilePatch, IntraLineDiff, PatchBuilder,
};
use crate::engine::git_cli::{git_text, resolve_repo, sandbox_write, validate_repo, ResolvedRepo};
use crate::engine::git_reader::{
    BlameLine, BlameResult, CommitDetails, CommitDiffPayload, CommitFileChange, FileBlob,
    ReflogEntry, RepoLanguageStat,
};
use crate::engine::git_writer::{
    checkout_branch_attempts, commit_argv, create_branch_argv, create_tag_argv, delete_tag_argv,
    merge_argv, pull_argv, push_argv, push_tag_argv, rebase_sequence_plan, stash_pop_argv,
    stash_save_argv, validate_ref_name, RebaseStep,
};
use crate::engine::{
    BranchInfo, BranchStatsReport, FileStatus, GitReader, GitWriter, TagInfo, WorktreeInfo,
};
use crate::github::{
    checkout_pull_request, create_issue, discover_github_remote, gh_repo_flags,
    load_dependabot_alerts, load_github_context, validate_issue_payload, DependabotReport,
    GitHubContext,
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
    // The payload also carries `stats_degraded` (numstat lookups failed);
    // the frontend does not know that flag yet, so only the files array is
    // surfaced for now — identical to the old shape.
    off_thread(move || GitReader::get_status_payload(&repo_path).map(|p| p.files)).await
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
        build_commit_graph_payload(
            &repo_path,
            max_commits,
            query.as_deref(),
            revision.as_deref(),
            skip.unwrap_or(0),
        )
    })
    .await
}

/// Walks one page of history and assembles the graph payload around it.
///
/// Pagination goes through [`GitReader::read_commit_history_probe`] so
/// `has_more` stays truthful when `max_commits` equals
/// [`GitReader::MAX_HISTORY_COMMITS`] exactly: the old limit+1 dance had its
/// probe row clamped back down at the cap, so a full page reported "no more
/// history" while older commits existed. The probe fetches CAP+1 before any
/// clamping and truncates internally; lane solving still sees exactly
/// `min(max_commits, cap)` rows either way.
fn build_commit_graph_payload(
    repo_path: &str,
    max_commits: usize,
    query: Option<&str>,
    revision: Option<&str>,
    skip: usize,
) -> Result<CommitGraphPayload, String> {
    let limit = max_commits.clamp(1, GitReader::MAX_HISTORY_COMMITS);
    let repo = repo_path.to_string();
    let rev = revision.map(str::to_string);
    // History is the long pole; HEAD and ref decorations are independent of
    // it, so they run concurrently instead of serializing three walks behind
    // one another on a large repository.
    let (history, head_id, refs) = std::thread::scope(|scope| {
        let history = scope.spawn(move || {
            GitReader::read_commit_history_probe(&repo, skip, limit, rev.as_deref())
        });
        let repo2 = repo_path.to_string();
        let head = scope.spawn(move || GitReader::head_id(&repo2).ok());
        let repo3 = repo_path.to_string();
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

    let (mut raw_commits, has_more) = history?;

    let filter = query.map(CommitFilter::parse).unwrap_or_default();
    if let Some(ref path) = filter.path {
        let touching: HashSet<String> = GitReader::commits_touching_path(repo_path, path, limit)?
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

/// Commit diff as a structured payload: normal-sized files inline in
/// `content`, anything left out (oversized, binary, budget-exhausted)
/// reported in `skipped_files` with its numstat counts. Wire-contract change
/// from the old raw diff string; the frontend consumes the new shape.
#[tauri::command(async)]
pub async fn cmd_get_commit_diff(
    repo_path: String,
    commit_id: String,
) -> Result<CommitDiffPayload, String> {
    off_thread(move || GitReader::get_commit_diff_payload(&repo_path, &commit_id)).await
}

#[tauri::command(async)]
pub async fn cmd_get_commit_file_diff(
    repo_path: String,
    commit_id: String,
    file_path: String,
) -> Result<String, String> {
    off_thread(move || GitReader::get_commit_file_diff(&repo_path, &commit_id, &file_path)).await
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
        let policy = crate::harness::guard_file(&repo_path, &file_path, "modify")?;
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
        // This is a mutating git action (index rewrite via `git apply
        // --cached`), so it goes through the same command gate as every
        // other mutation. The argv is exactly what
        // GitWriter::apply_patch_to_index runs. The verdict is not returned:
        // this command's IPC shape predates Guarded<()>, and changing it
        // would break the frontend contract — an allow proceeds silently, a
        // refusal surfaces as Err either way.
        guard(
            &repo_path,
            &[
                "git",
                "apply",
                "--cached",
                "--unidiff-zero",
                "--recount",
                "-",
            ],
        )?;
        GitWriter::apply_patch_to_index(&repo_path, &patch)
    })
    .await
}

#[tauri::command(async)]
pub async fn cmd_unstage_selective_patch(
    repo_path: String,
    file_patch: FilePatch,
) -> Result<(), String> {
    off_thread(move || {
        PatchBuilder::validate_file_patch(&file_patch)?;
        let patch = PatchBuilder::build_selective_patch(&file_patch, false);
        guard(
            &repo_path,
            &[
                "git",
                "apply",
                "--cached",
                "--unidiff-zero",
                "--recount",
                "-",
            ],
        )?;
        GitWriter::apply_patch_to_index(&repo_path, &patch)
    })
    .await
}

/// Commits the index, after the harness's command gate has judged the exact
/// command line this would run — including the empty-message amend form
/// (`--amend --no-edit`) the writer uses for internal squash flows.
#[tauri::command(async)]
pub async fn cmd_commit(
    repo_path: String,
    message: String,
    amend: bool,
) -> Result<Guarded<String>, String> {
    off_thread(move || {
        // Mirror GitWriter::commit_inner's branching exactly so the judged
        // line is the line that would run: an amend with an empty message is
        // `commit --amend --no-edit`, never a fictional `-m ''`.
        let argv = commit_argv(&message, amend);
        let policy = guard(&repo_path, &argv_refs(&argv))?;
        let output = GitWriter::commit(&repo_path, &message, amend)?;
        Ok(Guarded { policy, output })
    })
    .await
}

/// Checks out a branch, after the gate has judged every strategy the writer
/// may attempt (`switch --guess`, then two `checkout` fallbacks). All of
/// them are gated before the first runs because any one could be the one
/// that executes.
#[tauri::command(async)]
pub async fn cmd_checkout_branch(
    repo_path: String,
    branch_name: String,
) -> Result<Guarded<()>, String> {
    off_thread(move || {
        // checkout_branch tries `switch --guess`, then plain `checkout`,
        // falling through on failure. Both can execute, so both are judged
        // from the shared builder; a refusal of either refuses the action (a
        // policy denying `git checkout X` must not be routed around by
        // running `git switch X` instead). The surfaced verdict is the
        // strictest across the attempts.
        let attempts = checkout_branch_attempts(&branch_name);
        let mut representative: Option<crate::harness::PolicyVerdict> = None;
        for attempt in &attempts {
            let verdict = guard(&repo_path, &argv_refs(attempt))?;
            representative = Some(match representative {
                Some(prev) => strictest_verdict(prev, verdict),
                None => verdict,
            });
        }
        GitWriter::checkout_branch(&repo_path, &branch_name)?;
        Ok(Guarded {
            policy: representative.expect("at least one attempt is always gated"),
            output: (),
        })
    })
    .await
}

#[tauri::command(async)]
pub async fn cmd_create_branch(
    repo_path: String,
    branch_name: String,
    start_point: Option<String>,
) -> Result<Guarded<()>, String> {
    off_thread(move || {
        let argv = create_branch_argv(&branch_name, start_point.as_deref())?;
        let policy = guard(&repo_path, &argv_refs(&argv))?;
        GitWriter::create_branch(&repo_path, &branch_name, start_point.as_deref())?;
        Ok(Guarded { policy, output: () })
    })
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
pub fn cmd_preview_conflict(document: ConflictDocument) -> String {
    ConflictResolver::render_preview(&document)
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

/// Blame for a 1-based inclusive line window, so the UI can blame a visible
/// region of a huge file without pulling the whole porcelain stream. Ranges
/// beyond the reader's cap come back clamped with `truncated: true` instead
/// of silently succeeding or failing.
///
/// NOTE for the lib.rs owner: register this one-liner in
/// `tauri::generate_handler!` (next to `cmd_get_file_blame`):
///     cmd_blame_range,
#[tauri::command(async)]
pub async fn cmd_blame_range(
    repo_path: String,
    file_path: String,
    start_line: usize,
    end_line: usize,
) -> Result<BlameResult, String> {
    off_thread(move || GitReader::get_blame_range(&repo_path, &file_path, start_line, end_line))
        .await
}

/// Rebases the current branch onto `onto_commit` by replaying `steps`.
///
/// The old gate judged only `git rebase --onto <X>` while execution ran a
/// detach, N cherry-picks, amends, a branch reset and a checkout. Now the
/// writer's planner composes every mutating argv up front, each one is
/// passed through the guard, and execution starts only if no verdict is a
/// denial — a refusal aborts before any mutation, not mid-sequence.
#[tauri::command(async)]
pub async fn cmd_rebase_interactive(
    repo_path: String,
    onto_commit: String,
    steps: Vec<RebaseStep>,
) -> Result<Guarded<()>, String> {
    off_thread(move || {
        // Discovered once (read-only) so the gated plan and the executed
        // plan are built from identical inputs.
        let original_branch = GitWriter::rebase_original_branch(&repo_path)?;
        let plan = rebase_sequence_plan(&onto_commit, &steps, original_branch.as_deref())?;
        // The gate must judge the commands that actually run: detach,
        // cherry-pick/amend lines and (on a branch) the finalize pair. A
        // refusal on ANY of them refuses the whole rebase before the first
        // mutation.
        let mut representative: Option<crate::harness::PolicyVerdict> = None;
        for cmd in &plan {
            let verdict = guard(&repo_path, &argv_refs(&cmd.argv))?;
            representative = Some(match representative {
                Some(prev) => strictest_verdict(prev, verdict),
                None => verdict,
            });
        }
        GitWriter::execute_rebase_sequence_for_branch(
            &repo_path,
            &onto_commit,
            &steps,
            original_branch.as_deref(),
        )?;
        Ok(Guarded {
            // Representative-verdict policy for multi-command actions: the
            // strictest pass travels with the result (Blocked can never be
            // attached — it errors out above). A single "first" or "last"
            // verdict would hide a warning or demotion on another mutating
            // line.
            policy: representative.expect("at least one planned command is always gated"),
            output: (),
        })
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

/// Full disk-usage scan of the repository (git internals, build/cache
/// artifacts, large files, worktrees, stale-branch weight). The walk is
/// budgeted and never follows symlinks; see `crate::storage`.
#[tauri::command(async)]
pub async fn cmd_storage_scan(repo_path: String) -> Result<crate::storage::StorageReport, String> {
    off_thread(move || crate::storage::scan_storage(&repo_path)).await
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
        let argv = pull_argv(remote.as_deref(), branch.as_deref());
        let policy = guard(&repo_path, &argv_refs(&argv))?;
        let output = GitWriter::pull(&repo_path, remote.as_deref(), branch.as_deref())?;
        Ok(Guarded { policy, output })
    })
    .await
}

/// Pushes, after the command gate has judged it. A force push is refused by
/// the harness's hard rules, which is the whole reason this call is gated.
/// The gate line comes from [`push_argv`], so what is judged is exactly what
/// executes — including `--force-with-lease` on force pushes. Verified
/// against a live `manvi serve`: both `git push --force origin main` and
/// `git push --force-with-lease origin main` come back deny/hard, so truthful
/// rendering keeps denies intact without belt-and-braces double-gating.
#[tauri::command(async)]
pub async fn cmd_push(
    repo_path: String,
    remote: Option<String>,
    branch: Option<String>,
    force: Option<bool>,
) -> Result<Guarded<String>, String> {
    off_thread(move || {
        let force = force.unwrap_or(false);
        let argv = push_argv(remote.as_deref(), branch.as_deref(), force);
        let policy = guard(&repo_path, &argv_refs(&argv))?;
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
        // merge_argv includes the --no-edit flag the writer always adds; the
        // gate judges the real command line, not an idealized one.
        let argv = merge_argv(&branch_name, ff_only);
        let policy = guard(&repo_path, &argv_refs(&argv))?;
        let output = GitWriter::merge_branch(&repo_path, &branch_name, ff_only)?;
        Ok(Guarded { policy, output })
    })
    .await
}

/// Restacks `branch` onto the tip of `onto`, after the gate has judged the
/// exact `rebase --onto` line the writer executes.
#[tauri::command(async)]
pub async fn cmd_restack(
    repo_path: String,
    branch: String,
    onto: String,
) -> Result<Guarded<String>, String> {
    off_thread(move || {
        // GitWriter::restack executes `rebase --onto <onto> <upstream>
        // <branch>` after resolving upstream from the reflog; the gate sees
        // that full line, not a truncated prefix. The resolution mirrors
        // restack's exactly (fork-point, then plain merge-base, then onto);
        // residual TOCTOU: HEAD could move between this read and the
        // writer's own re-resolution under the mutation lock.
        let planned = restack_planned_argv(&repo_path, &branch, &onto)?;
        let refs: Vec<&str> = planned.iter().map(String::as_str).collect();
        let policy = guard(&repo_path, &refs)?;
        let output = GitWriter::restack(&repo_path, &branch, &onto)?;
        Ok(Guarded { policy, output })
    })
    .await
}

#[tauri::command(async)]
pub async fn cmd_stash_save(
    repo_path: String,
    message: Option<String>,
) -> Result<Guarded<String>, String> {
    off_thread(move || {
        // Stashing rewrites the index and working tree, so it is a mutation
        // the gate judges like any other.
        let argv = stash_save_argv(message.as_deref());
        let policy = guard(&repo_path, &argv_refs(&argv))?;
        let output = GitWriter::stash_save(&repo_path, message.as_deref())?;
        Ok(Guarded { policy, output })
    })
    .await
}

#[tauri::command(async)]
pub async fn cmd_stash_pop(repo_path: String) -> Result<Guarded<String>, String> {
    off_thread(move || {
        let argv = stash_pop_argv();
        let policy = guard(&repo_path, &argv_refs(&argv))?;
        let output = GitWriter::stash_pop(&repo_path)?;
        Ok(Guarded { policy, output })
    })
    .await
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
        let policy = crate::harness::guard_file(&repo_path, &file_path, "modify")?;
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
            releases: Vec::new(),
            releases_error: None,
            releases_truncated: false,
            warnings: Vec::new(),
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
        // Judge the exact argv create_issue runs. github::run_gh appends the
        // remote pinning flags (--repo, and --hostname off github.com) after
        // the user-shaped arguments, so they are part of the executed line
        // and belong in the judged one too.
        let remote = discover_github_remote(&repo_path)?
            .ok_or_else(|| "No GitHub remote configured".to_string())?;
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
        argv.extend(gh_repo_flags(&remote));
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
        // checkout_pull_request runs `gh pr checkout <n> --repo <slug>
        // [--hostname <host>]`; the pinning flags are judged with it. A
        // missing GitHub remote fails here exactly as it would inside
        // checkout_pull_request, just before the gate instead of after.
        let remote = discover_github_remote(&repo_path)?
            .ok_or_else(|| "No GitHub remote configured".to_string())?;
        let mut argv_owned: Vec<String> =
            vec!["gh".into(), "pr".into(), "checkout".into(), n.clone()];
        argv_owned.extend(gh_repo_flags(&remote));
        let refs: Vec<&str> = argv_owned.iter().map(String::as_str).collect();
        let policy = guard(&repo_path, &refs)?;
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
        let argv_owned = crate::engine::worktree::remove_worktree_argv(&target_path, force);
        let refs: Vec<&str> = argv_owned.iter().map(String::as_str).collect();
        let policy = guard(&repo_path, &refs)?;
        crate::engine::worktree::remove_worktree(&repo_path, &target_path, force)?;
        Ok(Guarded { policy, output: () })
    })
    .await
}

#[tauri::command(async)]
pub async fn cmd_lock_worktree(
    repo_path: String,
    target_path: String,
    reason: Option<String>,
) -> Result<Guarded<()>, String> {
    off_thread(move || {
        let argv_owned =
            crate::engine::worktree::lock_worktree_argv(&target_path, reason.as_deref());
        let refs: Vec<&str> = argv_owned.iter().map(String::as_str).collect();
        let policy = guard(&repo_path, &refs)?;
        crate::engine::worktree::lock_worktree(&repo_path, &target_path, reason.as_deref())?;
        Ok(Guarded { policy, output: () })
    })
    .await
}

#[tauri::command(async)]
pub async fn cmd_unlock_worktree(
    repo_path: String,
    target_path: String,
) -> Result<Guarded<()>, String> {
    off_thread(move || {
        let argv_owned = crate::engine::worktree::unlock_worktree_argv(&target_path);
        let refs: Vec<&str> = argv_owned.iter().map(String::as_str).collect();
        let policy = guard(&repo_path, &refs)?;
        crate::engine::worktree::unlock_worktree(&repo_path, &target_path)?;
        Ok(Guarded { policy, output: () })
    })
    .await
}

#[tauri::command(async)]
pub async fn cmd_prune_worktree(repo_path: String) -> Result<Guarded<()>, String> {
    off_thread(move || {
        let argv_owned = crate::engine::worktree::prune_worktree_argv();
        let refs: Vec<&str> = argv_owned.iter().map(String::as_str).collect();
        let policy = guard(&repo_path, &refs)?;
        crate::engine::worktree::prune_worktree(&repo_path)?;
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

/// Creates a tag (annotated when a message is given), after the gate has
/// judged the exact `git tag` line.
#[tauri::command(async)]
pub async fn cmd_create_tag(
    repo_path: String,
    tag_name: String,
    commit_id: Option<String>,
    message: Option<String>,
) -> Result<Guarded<()>, String> {
    off_thread(move || {
        let argv = create_tag_argv(&tag_name, commit_id.as_deref(), message.as_deref());
        let policy = guard(&repo_path, &argv_refs(&argv))?;
        GitWriter::create_tag(
            &repo_path,
            &tag_name,
            commit_id.as_deref(),
            message.as_deref(),
        )?;
        Ok(Guarded { policy, output: () })
    })
    .await
}

/// Deletes a tag — a destructive ref write, gated like one.
#[tauri::command(async)]
pub async fn cmd_delete_tag(repo_path: String, tag_name: String) -> Result<Guarded<()>, String> {
    off_thread(move || {
        let argv = delete_tag_argv(&tag_name);
        let policy = guard(&repo_path, &argv_refs(&argv))?;
        GitWriter::delete_tag(&repo_path, &tag_name)?;
        Ok(Guarded { policy, output: () })
    })
    .await
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
                &argv_refs(&create_tag_argv(&plan.tag, None, Some(&plan.message))),
            )?)
        } else {
            None
        };
        // Judged via push_tag_argv, the same builder the writer executes, so
        // the fully-qualified refs/tags/<tag> refspec cannot drift.
        let push_policy = guard(
            &repo_path,
            &argv_refs(&push_tag_argv(&plan.remote, &plan.tag)),
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
///
/// Thin delegate to the harness's canonical owner (`crate::harness::
/// guard_command`), so this file keeps no second copy of render→check→refuse.
fn guard(repo_path: &str, argv: &[&str]) -> Result<crate::harness::PolicyVerdict, String> {
    crate::harness::guard_command(repo_path, argv)
}

/// Picks the verdict a multi-command action should report.
///
/// Ranking, strictest first: Blocked > Warned > Demoted > Unchecked > Allowed.
/// Blocked never actually reaches here as a survivor — `guard` refuses on it —
/// but the ranking still documents intent. A warning is an explicit rule
/// firing, a demotion is posture noise, an unchecked gate is an absence of a
/// check, and a clean allow carries nothing worth surfacing over its peers.
fn strictest_verdict(
    a: crate::harness::PolicyVerdict,
    b: crate::harness::PolicyVerdict,
) -> crate::harness::PolicyVerdict {
    use crate::harness::PolicyStatus::*;
    let rank = |v: &crate::harness::PolicyVerdict| match v.status {
        Blocked => 5,
        Warned => 4,
        Demoted => 3,
        Unchecked => 2,
        Allowed => 1,
    };
    if rank(&a) >= rank(&b) {
        a
    } else {
        b
    }
}

/// The full argv [`GitWriter::restack`] executes, including the upstream it
/// resolves from the reflog (fork-point, plain merge-base, then onto).
/// Mirrors that private resolution; residual TOCTOU between this read and the
/// writer's re-resolution under the mutation lock is accepted and documented
/// at the call site.
fn restack_planned_argv(repo_path: &str, branch: &str, onto: &str) -> Result<Vec<String>, String> {
    let repo = validate_repo(repo_path)?;
    validate_ref_name(branch)?;
    validate_ref_name(onto)?;
    let upstream = [
        vec!["merge-base", "--fork-point", onto, branch],
        vec!["merge-base", onto, branch],
    ]
    .into_iter()
    .find_map(|argv| {
        git_text(&repo, &argv)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    })
    .unwrap_or_else(|| onto.to_string());
    Ok(vec![
        "git".into(),
        "rebase".into(),
        "--onto".into(),
        onto.into(),
        upstream,
        branch.into(),
    ])
}

/// Borrows a builder-produced argv for [`guard`].
fn argv_refs(argv: &[String]) -> Vec<&str> {
    argv.iter().map(String::as_str).collect()
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
pub async fn cmd_ai_fix_health(
    repo_path: String,
    report: String,
    base_url: Option<String>,
    model: Option<String>,
) -> Result<crate::ai::AiGeneration, String> {
    off_thread(move || crate::ai::fix_health(&repo_path, &report, selection(base_url, model))).await
}

#[tauri::command(async)]
pub async fn cmd_ai_suggest_branch_name(
    repo_path: String,
    base_url: Option<String>,
    model: Option<String>,
) -> Result<crate::ai::AiGeneration, String> {
    off_thread(move || crate::ai::suggest_branch_name(&repo_path, selection(base_url, model))).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init_repo() -> tempfile::TempDir {
        let dir = tempfile::TempDir::new().unwrap();
        let output = std::process::Command::new("git")
            .args(["init", "-q", "-b", "main"])
            .current_dir(dir.path())
            .output()
            .expect("git init");
        assert!(output.status.success());
        std::fs::write(dir.path().join("tracked.txt"), "base\n").unwrap();
        let output = std::process::Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(["add", "--", "tracked.txt"])
            .current_dir(dir.path())
            .output()
            .expect("git add");
        assert!(output.status.success());
        let output = std::process::Command::new("git")
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-m",
                "init",
            ])
            .current_dir(dir.path())
            .output()
            .expect("git commit");
        assert!(
            output.status.success(),
            "commit failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        dir
    }

    /// Creates `count` chained commits in one `git fast-import` stream — the
    /// only fast way to exercise history pagination at MAX_HISTORY_COMMITS
    /// scale (100k+ `git commit` spawns would take minutes). Starts from an
    /// empty repository so the imported chain owns `main`.
    fn init_repo_with_imported_history(count: usize) -> tempfile::TempDir {
        let dir = tempfile::TempDir::new().unwrap();
        let output = std::process::Command::new("git")
            .args(["init", "-q", "-b", "main"])
            .current_dir(dir.path())
            .output()
            .expect("git init");
        assert!(output.status.success());
        let mut stream = String::with_capacity(count * 48 + 64);
        for i in 0..count {
            let msg = format!("imported commit {i}");
            stream.push_str("commit refs/heads/main\n");
            stream.push_str(&format!("mark :{}\n", i + 1));
            stream.push_str("committer t <t@t> 1700000000 +0000\n");
            stream.push_str(&format!("data {}\n{msg}\n", msg.len()));
            if i > 0 {
                stream.push_str(&format!("from :{}\n", i));
            }
            stream.push('\n');
        }
        let mut child = std::process::Command::new("git")
            .args(["fast-import", "--quiet"])
            .current_dir(dir.path())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn git fast-import");
        use std::io::Write as _;
        child
            .stdin
            .take()
            .expect("fast-import stdin")
            .write_all(stream.as_bytes())
            .expect("feed fast-import stream");
        let out = child.wait_with_output().expect("wait fast-import");
        assert!(
            out.status.success(),
            "fast-import failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        dir
    }

    /// Regression for the truthful-pagination fix: when a page's limit equals
    /// MAX_HISTORY_COMMITS exactly, the old fetch_limit+1 dance had its probe
    /// row clamped back to the cap, so a full page reported has_more=false and
    /// silently hid the rest of history. The probe keeps the extra row.
    #[test]
    fn commit_graph_reports_more_history_when_page_equals_the_cap() {
        let dir = init_repo_with_imported_history(GitReader::MAX_HISTORY_COMMITS + 3);
        let path = dir.path().to_str().unwrap();
        let payload =
            build_commit_graph_payload(path, GitReader::MAX_HISTORY_COMMITS, None, None, 0)
                .expect("graph at cap");
        assert!(
            payload.has_more,
            "older commits exist beyond the cap; has_more must say so"
        );
        assert_eq!(payload.rows.len(), GitReader::MAX_HISTORY_COMMITS);

        // A small page on the same repository reports both directions right.
        let small = build_commit_graph_payload(path, 10, None, None, 0).expect("small page");
        assert!(small.has_more);
        assert_eq!(small.rows.len(), 10);
        // Deepest reachable page: the reader clamps skips at the cap as well,
        // so skipping "past" it still lands on its final three commits.
        let tail = build_commit_graph_payload(path, 10, None, None, GitReader::MAX_HISTORY_COMMITS)
            .expect("tail page");
        assert!(!tail.has_more);
        assert_eq!(tail.rows.len(), 3);
    }

    /// Wire-contract pin for cmd_get_commit_diff: the command now returns the
    /// structured payload, so its serialized keys are part of the frontend
    /// contract and must not drift silently.
    #[test]
    fn commit_diff_payload_serializes_the_documented_wire_keys() {
        let dir = init_repo();
        std::fs::write(dir.path().join("tracked.txt"), "changed\n").unwrap();
        let output = std::process::Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(["commit", "-am", "second"])
            .current_dir(dir.path())
            .output()
            .expect("second commit");
        assert!(output.status.success());
        let oid = String::from_utf8(
            std::process::Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(dir.path())
                .output()
                .expect("rev-parse")
                .stdout,
        )
        .unwrap();

        let payload = GitReader::get_commit_diff_payload(dir.path().to_str().unwrap(), oid.trim())
            .expect("payload");
        assert!(!payload.truncated && payload.skipped_files.is_empty());
        assert_eq!(payload.total_files, 1);
        assert_eq!(payload.total_additions, 1);
        assert_eq!(payload.total_deletions, 1);
        assert!(payload.content.contains("changed"));

        let json = serde_json::to_value(&payload).unwrap();
        let mut keys: Vec<String> = json
            .as_object()
            .expect("payload serializes to an object")
            .keys()
            .cloned()
            .collect();
        keys.sort();
        assert_eq!(
            keys,
            vec![
                "content",
                "included_files",
                "skipped_files",
                "total_additions",
                "total_deletions",
                "total_files",
                "truncated"
            ]
        );
    }

    fn verdict(status: crate::harness::PolicyStatus) -> crate::harness::PolicyVerdict {
        crate::harness::PolicyVerdict {
            status,
            checked: true,
            target: "git x".into(),
            rule: String::new(),
            severity: String::new(),
            reason: String::new(),
            demoted: String::new(),
            detail: String::new(),
            detail_code: String::new(),
        }
    }

    #[test]
    fn strictest_verdict_ranks_blocked_over_clean_passes() {
        use crate::harness::PolicyStatus::*;
        let allowed = strictest_verdict(verdict(Allowed), verdict(Allowed));
        assert_eq!(allowed.status, Allowed);
        let warned = strictest_verdict(verdict(Allowed), verdict(Warned));
        assert_eq!(warned.status, Warned);
        let demoted = strictest_verdict(verdict(Demoted), verdict(Unchecked));
        assert_eq!(demoted.status, Demoted);
        let blocked = strictest_verdict(verdict(Allowed), verdict(Blocked));
        assert_eq!(blocked.status, Blocked);
    }

    fn init_repo_with_branch_commits() -> (tempfile::TempDir, String, String, String) {
        let dir = tempfile::TempDir::new().unwrap();
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(args)
                .current_dir(dir.path())
                .output()
                .expect("spawn git");
            assert!(
                output.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8(output.stdout).unwrap().trim().to_string()
        };
        git(&["init", "-q", "-b", "main"]);
        std::fs::write(dir.path().join("base.txt"), "base\n").unwrap();
        git(&["add", "--", "base.txt"]);
        git(&["commit", "-m", "base"]);
        let base = git(&["rev-parse", "HEAD"]);
        std::fs::write(dir.path().join("a.txt"), "a\n").unwrap();
        git(&["add", "--", "a.txt"]);
        git(&["commit", "-m", "A"]);
        let c_a = git(&["rev-parse", "HEAD"]);
        std::fs::write(dir.path().join("b.txt"), "b\n").unwrap();
        git(&["add", "--", "b.txt"]);
        git(&["commit", "-m", "B"]);
        let c_b = git(&["rev-parse", "HEAD"]);
        (dir, base, c_a, c_b)
    }

    /// The restack planner resolves the same upstream restack does (here:
    /// the fork base of feature from main) and renders the full four-arg
    /// rebase line, not a prefix.
    #[test]
    fn restack_planned_argv_includes_resolved_upstream() {
        let (dir, base, _c_a, _c_b) = init_repo_with_branch_commits();
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .args(args)
                .current_dir(dir.path())
                .output()
                .unwrap();
            assert!(output.status.success());
            String::from_utf8(output.stdout).unwrap().trim().to_string()
        };
        git(&["branch", "feature", &base]);
        let argv = restack_planned_argv(dir.path().to_str().unwrap(), "feature", "main").unwrap();
        assert_eq!(
            argv.iter().take(4).map(String::as_str).collect::<Vec<_>>(),
            vec!["git", "rebase", "--onto", "main"]
        );
        assert_eq!(argv.len(), 6);
        assert_eq!(argv[5], "feature");
        // Upstream resolves to the fork point (the common ancestor), a full oid.
        assert_eq!(argv[4], base, "upstream must resolve to the fork base");
        // Invalid refs are refused before anything is rendered or judged.
        assert!(restack_planned_argv(dir.path().to_str().unwrap(), "-evil", "main").is_err());
    }
}
