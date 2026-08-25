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
    BlameLine, CommitDetails, CommitFileChange, FileBlob, ReflogEntry, RepoLanguageStat,
};
use crate::engine::git_writer::{validate_oid_or_revision, validate_ref_name, RebaseStep};
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
/// command line this would run.
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
        let argv: Vec<&str> = if amend && message.is_empty() {
            vec!["git", "commit", "--amend", "--no-edit"]
        } else {
            let mut argv = vec!["git", "commit", "-m", message.as_str()];
            if amend {
                argv.push("--amend");
            }
            argv
        };
        let policy = guard(&repo_path, &argv)?;
        let output = GitWriter::commit(&repo_path, &message, amend)?;
        Ok(Guarded { policy, output })
    })
    .await
}

#[tauri::command(async)]
pub async fn cmd_checkout_branch(
    repo_path: String,
    branch_name: String,
) -> Result<Guarded<()>, String> {
    off_thread(move || {
        // checkout_branch tries `switch --guess`, then plain `checkout`,
        // falling through on failure. Both can execute, so both are judged;
        // a refusal of either refuses the action (a policy denying
        // `git checkout X` must not be routed around by running
        // `git switch X` instead).
        let attempts: [Vec<String>; 2] = [
            vec![
                "git".into(),
                "switch".into(),
                "--guess".into(),
                branch_name.clone(),
            ],
            vec!["git".into(), "checkout".into(), branch_name.clone()],
        ];
        let mut representative: Option<crate::harness::PolicyVerdict> = None;
        for argv in &attempts {
            let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
            let verdict = guard(&repo_path, &refs)?;
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
        let mut argv = vec!["git", "branch", branch_name.as_str()];
        if let Some(ref sp) = start_point {
            argv.push(sp.as_str());
        }
        let policy = guard(&repo_path, &argv)?;
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

#[tauri::command(async)]
pub async fn cmd_rebase_interactive(
    repo_path: String,
    onto_commit: String,
    steps: Vec<RebaseStep>,
) -> Result<Guarded<()>, String> {
    off_thread(move || {
        // The gate must judge the commands that actually run, and
        // execute_rebase_sequence never runs `git rebase`: it replays the
        // steps with checkout --detach / cherry-pick / commit --amend /
        // branch -f (see GitWriter::execute_rebase_sequence). A rendered
        // `git rebase …` line is a command that does not exist; worse, `-i`
        // names an interactive session this client never opens. Derive the
        // exact planned sequence here and put every line through the gate;
        // a refusal on ANY of them refuses the whole rebase before the
        // first mutation.
        let planned = rebase_planned_commands(&repo_path, &onto_commit, &steps)?;
        let mut representative: Option<crate::harness::PolicyVerdict> = None;
        for argv in &planned {
            let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
            let verdict = guard(&repo_path, &refs)?;
            representative = Some(match representative {
                Some(prev) => strictest_verdict(prev, verdict),
                None => verdict,
            });
        }
        GitWriter::execute_rebase_sequence(&repo_path, &onto_commit, &steps)?;
        Ok(Guarded {
            // Representative-verdict policy for multi-command actions: the
            // strictest pass travels with the result (Blocked can never be
            // attached — it errors out above). A single "last" verdict would
            // surface the trailing `git checkout`, hiding an earlier warning
            // or demotion on the mutating cherry-picks.
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
///
/// Judged argv is the executed argv: GitWriter::push runs
/// `--force-with-lease`, so that exact flag is what gets judged. The
/// harness's `command.force_push` hard rule catches it — verified against a
/// live `manvi serve`: both `git push --force origin main` and
/// `git push --force-with-lease origin main` come back deny/hard. (The old
/// code judged a fictional `--force`; truthful rendering keeps denies intact
/// without belt-and-braces double-gating.)
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
            argv.push("--force-with-lease");
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
        // Same flag order as GitWriter::merge_branch, which always passes
        // --no-edit (it never opens an interactive editor).
        let mut argv = vec!["git", "merge"];
        if ff_only {
            argv.push("--ff-only");
        }
        argv.push("--no-edit");
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

#[tauri::command(async)]
pub async fn cmd_create_tag(
    repo_path: String,
    tag_name: String,
    commit_id: Option<String>,
    message: Option<String>,
) -> Result<Guarded<()>, String> {
    off_thread(move || {
        let mut argv = vec!["git", "tag"];
        if message.is_some() {
            argv.push("-a");
            argv.push(tag_name.as_str());
            argv.push("-m");
            argv.push(message.as_deref().unwrap_or_default());
        } else {
            argv.push(tag_name.as_str());
        }
        if let Some(cid) = commit_id.as_deref() {
            argv.push(cid);
        }
        let policy = guard(&repo_path, &argv)?;
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

#[tauri::command(async)]
pub async fn cmd_delete_tag(repo_path: String, tag_name: String) -> Result<Guarded<()>, String> {
    off_thread(move || {
        let policy = guard(&repo_path, &["git", "tag", "-d", tag_name.as_str()])?;
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

/// The amend argv [`crate::engine::git_writer::GitWriter::commit_inner`] runs
/// for `(message, amend)`. Kept in lockstep with that private fn by test
/// (`commit_amend_argv_mirrors_commit_inner`); if the writer ever changes flag
/// shape or order, both must move together.
fn commit_amend_argv(message: &str, amend: bool) -> Vec<String> {
    let mut args = vec!["git".to_string(), "commit".to_string()];
    if amend && message.is_empty() {
        args.push("--amend".into());
        args.push("--no-edit".into());
    } else {
        args.push("-m".into());
        args.push(message.to_string());
        if amend {
            args.push("--amend".into());
        }
    }
    args
}

/// Mirrors git_writer's private `reworded_message`: rewrites only the subject
/// line, keeping the body intact.
fn reworded_message(original: &str, new_subject: &str) -> String {
    match original.split_once('\n') {
        Some((_, rest)) => format!("{new_subject}\n{rest}"),
        None => new_subject.to_string(),
    }
}

/// The exact mutating commands [`GitWriter::execute_rebase_sequence`] will run
/// for this rebase, in execution order — derived here because engine/ cannot
/// own a shared argv builder without this file growing a second gate seam.
///
/// Mirrored decision points (keep in lockstep with the writer):
/// - validations first (onto, steps non-empty, every step oid);
/// - `checkout --detach <onto>`;
/// - per step: `cherry-pick [cid|-n cid]`, plus the amend each action makes
///   (Squash amends preserving the message via `--amend --no-edit`, Fixup the
///   same, Reword amends `-m <composed> --amend`);
/// - rollback pair that runs whenever a step fails: `cherry-pick --abort`,
///   then `checkout -f <branch|head>` — gated proactively because they are
///   part of what this invocation may execute;
/// - on a branch, the success tail: `branch -f <branch> HEAD`, then
///   `checkout <branch>` (the retry reruns the same argv).
///
/// Read-only probes (status, merge-base ancestry, symbolic-ref, log) are not
/// gated, matching the existing fetch/status precedent. A failing reword
/// composition refuses up front — strictly safer than the writer's
/// mid-sequence failure.
fn rebase_planned_commands(
    repo_path: &str,
    onto_commit: &str,
    steps: &[RebaseStep],
) -> Result<Vec<Vec<String>>, String> {
    let repo = validate_repo(repo_path)?;
    validate_oid_or_revision(onto_commit)?;
    if steps.is_empty() {
        return Err("Rebase sequence is empty".into());
    }
    // Mirror of the writer's own input validations, so a sequence it would
    // refuse never reaches the gate or the subprocess layer.
    if let Some(first) = steps.first() {
        if matches!(
            first.action,
            crate::engine::git_writer::RebaseActionKind::Squash
                | crate::engine::git_writer::RebaseActionKind::Fixup
        ) {
            return Err(format!(
                "Cannot '{}' commit {} without a previous commit to combine into",
                match first.action {
                    crate::engine::git_writer::RebaseActionKind::Squash => "squash",
                    _ => "fixup",
                },
                first.commit_id
            ));
        }
    }
    for step in steps {
        validate_oid_or_revision(&step.commit_id)?;
    }

    let original_head = git_text(&repo, &["rev-parse", "HEAD"])?.trim().to_string();
    let original_branch = git_text(&repo, &["symbolic-ref", "--quiet", "--short", "HEAD"])
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let mut planned: Vec<Vec<String>> = vec![vec![
        "git".into(),
        "checkout".into(),
        "--detach".into(),
        onto_commit.into(),
    ]];

    for step in steps {
        match &step.action {
            crate::engine::git_writer::RebaseActionKind::Pick
            | crate::engine::git_writer::RebaseActionKind::Reword(_) => {
                planned.push(vec![
                    "git".into(),
                    "cherry-pick".into(),
                    step.commit_id.clone(),
                ]);
            }
            crate::engine::git_writer::RebaseActionKind::Squash
            | crate::engine::git_writer::RebaseActionKind::Fixup => {
                planned.push(vec![
                    "git".into(),
                    "cherry-pick".into(),
                    "-n".into(),
                    step.commit_id.clone(),
                ]);
            }
            crate::engine::git_writer::RebaseActionKind::Drop => {}
        }
        match &step.action {
            // Squash folds into the picked commit keeping its message;
            // Fixup keeps it too, discarding the folded commit's own.
            crate::engine::git_writer::RebaseActionKind::Squash
            | crate::engine::git_writer::RebaseActionKind::Fixup => {
                planned.push(commit_amend_argv("", true));
            }
            crate::engine::git_writer::RebaseActionKind::Reword(new_msg) => {
                let original = git_text(&repo, &["log", "-1", "--format=%B", &step.commit_id])?;
                let message = reworded_message(&original, new_msg);
                planned.push(commit_amend_argv(&message, true));
            }
            _ => {}
        }
    }

    // Rollback commands: executed iff some step fails. Judged regardless so a
    // posture that denies them refuses the whole rebase before HEAD moves.
    planned.push(vec!["git".into(), "cherry-pick".into(), "--abort".into()]);
    let restore_target = original_branch
        .clone()
        .unwrap_or_else(|| original_head.clone());
    planned.push(vec![
        "git".into(),
        "checkout".into(),
        "-f".into(),
        restore_target,
    ]);

    // Success tail, only when the session started on a branch.
    if let Some(branch) = original_branch {
        planned.push(vec![
            "git".into(),
            "branch".into(),
            "-f".into(),
            branch.clone(),
            "HEAD".into(),
        ]);
        planned.push(vec!["git".into(), "checkout".into(), branch]);
    }

    Ok(planned)
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

#[cfg(test)]
mod tests {
    use super::*;

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

    /// Lockstep test for commit_amend_argv against GitWriter::commit_inner's
    /// branching: empty-message amend is --amend --no-edit, everything else
    /// is -m <msg> with an optional trailing --amend.
    #[test]
    fn commit_amend_argv_mirrors_commit_inner() {
        assert_eq!(
            commit_amend_argv("feat: thing", false),
            vec!["git", "commit", "-m", "feat: thing"]
        );
        assert_eq!(
            commit_amend_argv("feat: thing", true),
            vec!["git", "commit", "-m", "feat: thing", "--amend"]
        );
        // The Squash/Fixup path amends with no message at all.
        assert_eq!(
            commit_amend_argv("", true),
            vec!["git", "commit", "--amend", "--no-edit"]
        );
    }

    #[test]
    fn reworded_message_keeps_body_rewrites_subject() {
        assert_eq!(reworded_message("old\n\nbody", "new"), "new\n\nbody");
        assert_eq!(reworded_message("subject only", "new"), "new");
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

    /// The planned sequence for pick+squash on a branch mirrors
    /// execute_rebase_sequence exactly: detach onto the base, replay the
    /// steps (squash picks -n and amends --no-edit), then move the branch
    /// and check it back out. The rollback pair is gated proactively.
    #[test]
    fn rebase_planned_commands_mirror_the_writer_sequence_on_a_branch() {
        let (dir, base, c_a, c_b) = init_repo_with_branch_commits();
        let path = dir.path().to_str().unwrap();
        let steps = vec![
            RebaseStep {
                commit_id: c_a.clone(),
                action: crate::engine::git_writer::RebaseActionKind::Pick,
            },
            RebaseStep {
                commit_id: c_b.clone(),
                action: crate::engine::git_writer::RebaseActionKind::Squash,
            },
        ];
        let planned = rebase_planned_commands(path, &base, &steps).unwrap();
        assert_eq!(
            planned,
            vec![
                vec!["git", "checkout", "--detach", &base],
                vec!["git", "cherry-pick", &c_a],
                vec!["git", "cherry-pick", "-n", &c_b],
                vec!["git", "commit", "--amend", "--no-edit"],
                vec!["git", "cherry-pick", "--abort"],
                vec!["git", "checkout", "-f", "main"],
                vec!["git", "branch", "-f", "main", "HEAD"],
                vec!["git", "checkout", "main"],
            ]
        );
    }

    /// Detached sessions have no branch to fast-forward: the restore target
    /// is the detached HEAD oid and there is no branch -f / checkout tail.
    #[test]
    fn rebase_planned_commands_detached_session_restores_to_head_oid() {
        let (dir, base, c_a, c_b) = init_repo_with_branch_commits();
        let path = dir.path().to_str().unwrap();
        let head = {
            let output = std::process::Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(dir.path())
                .output()
                .unwrap();
            String::from_utf8(output.stdout).unwrap().trim().to_string()
        };
        let detached = std::process::Command::new("git")
            .args(["checkout", "-q", "--detach"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        assert!(
            detached.status.success(),
            "detaching failed: {}",
            String::from_utf8_lossy(&detached.stderr)
        );
        let planned = rebase_planned_commands(
            path,
            &base,
            &[
                RebaseStep {
                    commit_id: c_a.clone(),
                    action: crate::engine::git_writer::RebaseActionKind::Pick,
                },
                RebaseStep {
                    commit_id: c_b.clone(),
                    action: crate::engine::git_writer::RebaseActionKind::Fixup,
                },
            ],
        )
        .unwrap();
        assert_eq!(
            planned,
            vec![
                vec!["git", "checkout", "--detach", &base],
                vec!["git", "cherry-pick", &c_a],
                vec!["git", "cherry-pick", "-n", &c_b],
                vec!["git", "commit", "--amend", "--no-edit"],
                vec!["git", "cherry-pick", "--abort"],
                vec!["git", "checkout", "-f", &head],
            ]
        );
    }

    /// A reword step composes its amend message from the original body plus
    /// the new subject, so the gate judges the real `-m` payload.
    #[test]
    fn rebase_planned_commands_reword_composes_the_real_message() {
        let (dir, base, _c_a, _c_b) = init_repo_with_branch_commits();
        // Rewrite HEAD's message so it has a body worth preserving.
        let output = std::process::Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(["commit", "--amend", "-m", "A\n\nbody of A"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        assert!(output.status.success());
        let c_a = {
            let output = std::process::Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(dir.path())
                .output()
                .unwrap();
            String::from_utf8(output.stdout).unwrap().trim().to_string()
        };
        let planned = rebase_planned_commands(
            dir.path().to_str().unwrap(),
            &base,
            &[RebaseStep {
                commit_id: c_a.clone(),
                action: crate::engine::git_writer::RebaseActionKind::Reword(
                    "renamed subject".into(),
                ),
            }],
        )
        .unwrap();
        assert_eq!(planned[2][0], "git");
        assert_eq!(planned[2][1], "commit");
        assert_eq!(planned[2][2], "-m");
        assert!(
            planned[2][3].starts_with("renamed subject\n\nbody of A"),
            "reword must keep the original body, got {:?}",
            planned[2][3]
        );
        assert_eq!(planned[2][4], "--amend");
    }

    #[test]
    fn rebase_planned_commands_refuses_empty_steps_and_invalid_oids() {
        let (dir, base, _c_a, _c_b) = init_repo_with_branch_commits();
        let path = dir.path().to_str().unwrap();
        let err = rebase_planned_commands(path, &base, &[]).unwrap_err();
        assert!(err.contains("empty"));
        let err = rebase_planned_commands(
            path,
            &base,
            &[RebaseStep {
                commit_id: "; rm -rf /".into(),
                action: crate::engine::git_writer::RebaseActionKind::Pick,
            }],
        )
        .unwrap_err();
        assert!(err.contains("Invalid revision"), "{err}");
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

#[tauri::command(async)]
pub async fn cmd_ai_suggest_branch_name(
    repo_path: String,
    base_url: Option<String>,
    model: Option<String>,
) -> Result<crate::ai::AiGeneration, String> {
    off_thread(move || crate::ai::suggest_branch_name(&repo_path, selection(base_url, model))).await
}
