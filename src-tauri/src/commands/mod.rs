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
    BlameLine, CommitDetails, CommitFileChange, DiffPayload, DoraReport, FileBlob, KnowledgeReport,
    LanguageStatsReport, PulseReport, ReflogEntry,
};
use crate::engine::git_writer::{validate_oid_or_revision, validate_ref_name, RebaseStep};
use crate::engine::{
    BranchInfo, BranchStatsReport, FileStatus, GitReader, GitWriter, OperationAction, RemoteChange,
    RemoteList, RepoOperation, ResetMode, StashAction, StashEntry, SubmoduleChange, SubmoduleList,
    WorktreeInfo,
};
use crate::github::{
    checkout_pull_request, create_issue, discover_github_remote, issue_create_argv,
    load_dependabot_alerts, load_github_context, pr_checkout_argv, validate_issue_payload,
    DependabotReport, GitHubContext,
};
use crate::graph::{
    mainline_chain_ids, simplify_history, BezierGeometryCalculator, CubicBezierCurve, LaneSolver,
    MainlineHint, RawCommitNode, RefDecoration, RefKind, RefScope, VisualCommitRow,
};
use crate::stack::StackTreeEngine;
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
    pub head_id: Option<String>,
    /// Branches, remotes and tags pointing at rows in this graph.
    pub refs: Vec<crate::graph::RefDecoration>,
    /// True when the walk hit its row cap and older commits exist. The client
    /// offers "load more" instead of silently hiding the rest of history.
    pub has_more: bool,
    /// Degradations recorded while assembling this payload: probes that failed
    /// without failing the load (HEAD resolution, ref decoration listing), so a
    /// degraded facet is never indistinguishable from an honest empty set.
    /// Same shape as `AiGeneration::warnings`; `default` keeps payloads from
    /// before this field deserializable.
    #[serde(default)]
    pub warnings: Vec<String>,
    /// The commit at the top of the pinned mainline: the straight column-0
    /// rail the solver keeps for the default branch (see
    /// [`crate::graph::MainlineHint`]). `None` only when the graph has no
    /// rows. `default` keeps payloads from before the field deserializable.
    #[serde(default)]
    pub mainline_id: Option<String>,
    /// The branch the mainline was anchored on — `main`, `origin/main`, or
    /// the HEAD branch as a fallback — or `None` when no ref resolved and the
    /// newest commit's chain was pinned instead.
    #[serde(default)]
    pub mainline_name: Option<String>,
}

/// The refs that anchor the straight mainline column, resolved once per
/// graph load from the decoration list already in hand.
pub struct ResolvedMainline {
    pub hint: MainlineHint,
    /// `(commit id, display name)` for every hinted branch tip, in the
    /// hint's priority order.
    tips: Vec<(String, String)>,
}

impl ResolvedMainline {
    /// The label for a mainline anchored on commit `id`: the branch name for
    /// a hinted tip, the HEAD ref (a branch name, or `HEAD` when detached)
    /// for the fallback, and nothing when the newest commit was pinned.
    pub fn name_for(&self, id: &str, refs: &[RefDecoration]) -> Option<String> {
        if let Some((_, name)) = self.tips.iter().find(|(tip, _)| tip == id) {
            return Some(name.clone());
        }
        if self.hint.fallback_tip.as_deref() == Some(id) {
            return refs.iter().find(|r| r.is_head).map(|r| r.name.clone());
        }
        None
    }
}

/// Conventional default-branch names, probed in this order when the
/// repository's own default branch could not be resolved.
const MAINLINE_NAME_FALLBACKS: [&str; 4] = ["main", "master", "trunk", "develop"];

/// Decides which refs the lane solver keeps straight.
///
/// The repository's default branch comes first; the conventional names are
/// probed only when that is unknown. The FIRST name with any ref supplies
/// every tip carrying it — the local branch, then each remote-tracking copy
/// (`origin/main`, `upstream/main`) — so the solver anchors on the user's
/// own branch and extends the rail through a remote that is ahead of it.
/// Falling through to a *different* name when the default branch's tips are
/// not loaded would pin the wrong branch, so that never happens: HEAD is the
/// only fallback, for windows (single-branch, path-filtered) that hold none
/// of the mainline's tips.
pub fn resolve_mainline_hint(
    refs: &[RefDecoration],
    default_branch: Option<&str>,
    head_id: Option<&str>,
) -> ResolvedMainline {
    let mut names: Vec<&str> = Vec::new();
    if let Some(name) = default_branch.map(str::trim).filter(|n| !n.is_empty()) {
        names.push(name);
    }
    for name in MAINLINE_NAME_FALLBACKS {
        if !names.contains(&name) {
            names.push(name);
        }
    }

    let mut tips: Vec<(String, String)> = Vec::new();
    for name in names {
        let locals: Vec<&RefDecoration> = refs
            .iter()
            .filter(|r| r.kind == RefKind::Local && r.name == name)
            .collect();
        // Remote-tracking names carry the remote as their first segment.
        let remotes: Vec<&RefDecoration> = refs
            .iter()
            .filter(|r| {
                r.kind == RefKind::Remote
                    && r.name.split_once('/').map(|(_, short)| short) == Some(name)
            })
            .collect();
        if locals.is_empty() && remotes.is_empty() {
            continue;
        }
        // One display name per branch: the local name when the branch exists
        // locally (a remote that is ahead still reads as "main"), else the
        // remote-tracking ref so the label says where the rail comes from.
        let has_local = !locals.is_empty();
        for r in locals {
            tips.push((r.commit_id.clone(), r.name.clone()));
        }
        for r in remotes {
            let label = if has_local {
                name.to_string()
            } else {
                r.name.clone()
            };
            tips.push((r.commit_id.clone(), label));
        }
        break;
    }

    ResolvedMainline {
        hint: MainlineHint {
            branch_tips: tips.iter().map(|(id, _)| id.clone()).collect(),
            fallback_tip: head_id
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from),
        },
        tips,
    }
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

/// Turns one page of raw history into the graph payload.
///
/// `raw_commits` may carry one row past `max_commits` (the has_more probe).
/// The filter's non-path terms are applied here with git-style history
/// simplification: dropped rows hand their lineage to their children
/// ([`simplify_history`]), so survivors stay connected instead of ending in
/// stubs, and the mainline hint is re-anchored on the first survivor of the
/// chain it named — the filtered graph keeps the same branch straight as
/// the unfiltered one. Pure (no git access) so fixtures, smoke checks and
/// unit tests run exactly the code the command runs.
pub fn assemble_commit_graph(
    mut raw_commits: Vec<RawCommitNode>,
    max_commits: usize,
    filter: &CommitFilter,
    refs: Vec<RefDecoration>,
    default_branch: Option<&str>,
    head_id: Option<String>,
    warnings: Vec<String>,
) -> CommitGraphPayload {
    let has_more = raw_commits.len() > max_commits;
    if has_more {
        raw_commits.truncate(max_commits);
    }

    let mainline = resolve_mainline_hint(&refs, default_branch, head_id.as_deref());
    let mut hint = mainline.hint.clone();
    // The tip whose ref labels the rail once the filter has moved the
    // anchor down the chain; None when nothing on the chain survived.
    let mut label_tip: Option<String> = None;
    if !filter.is_empty() {
        let keep: Vec<bool> = raw_commits
            .iter()
            .map(|c| filter.matches_commit(c))
            .collect();
        if keep.iter().any(|kept| !kept) {
            let chain = mainline_chain_ids(&raw_commits, &hint);
            raw_commits = simplify_history(&raw_commits, &keep);
            let kept: HashSet<&str> = raw_commits.iter().map(|c| c.id.as_str()).collect();
            let survivor = chain.iter().find(|id| kept.contains(id.as_str())).cloned();
            if survivor.is_some() {
                label_tip = chain.first().cloned();
            }
            // A chain with no survivor leaves the hint empty on purpose: the
            // solver then pins the newest survivor's own chain rather than
            // an unrelated branch that happens to be loaded.
            hint = MainlineHint {
                branch_tips: survivor.into_iter().collect(),
                fallback_tip: None,
            };
        }
    }

    let rows = LaneSolver::new(12).solve_with_mainline(&raw_commits, &hint);
    // The solver owns which chain was pinned; the payload only reports it,
    // so the two can never disagree.
    let mainline_id = rows.iter().find(|r| r.is_mainline).map(|r| r.id.clone());
    let mainline_name = mainline_id.as_deref().and_then(|id| {
        mainline.name_for(id, &refs).or_else(|| {
            label_tip
                .as_deref()
                .and_then(|tip| mainline.name_for(tip, &refs))
        })
    });
    CommitGraphPayload {
        rows,
        head_id,
        refs,
        has_more,
        warnings,
        mainline_id,
        mainline_name,
    }
}

/// One page of solved graph rows, with the refs that decorate them.
///
/// `ref_scope` decides which refs seed the walk AND which refs get labelled —
/// one answer for both, so the graph cannot draw a lane it has no name for
/// (see [`crate::graph::ref_scope`]). `None` means the named set: branches,
/// remote-tracking branches, tags and HEAD.
#[tauri::command(async)]
pub async fn cmd_get_commit_graph(
    repo_path: String,
    max_commits: usize,
    query: Option<String>,
    revision: Option<String>,
    skip: Option<usize>,
    ref_scope: Option<RefScope>,
) -> Result<CommitGraphPayload, String> {
    off_thread(move || {
        // One row over the cap: the extra row is the has_more probe and is
        // dropped before lanes are solved. This arithmetic and the reader's
        // page_count_limit clamp must stay in lockstep — if the clamp swallows
        // the extra row, has_more dies at exactly MAX_HISTORY_COMMITS and a
        // ceiling-scale repository truncates silently (both helpers carry
        // tests pinning their side of the contract).
        let fetch_limit = graph_fetch_limit(max_commits);
        // Absent, or from a client built before the field existed: the named
        // set. A MALFORMED value never reaches here — serde refuses the whole
        // argument struct, which is the right answer at an IPC boundary and is
        // why the frontend normalizes the persisted preference before sending
        // it. Widening the graph beyond the refs it can label stays opt-in.
        let ref_scope = ref_scope.unwrap_or_default();
        let mut warnings: Vec<String> = Vec::new();
        let repo = repo_path.clone();
        let rev = revision.clone();
        // Parsing is pure string work, so it happens BEFORE the walk: a `path:`
        // token has to reach git itself. Narrowing a full walk afterwards left
        // every survivor naming parents that had been filtered out, and the
        // lane solver drew the result as disconnected stubs; git's own
        // path-limited log rewrites those parents to the nearest surviving
        // ancestors and keeps the history connected.
        let filter = query
            .as_deref()
            .map(CommitFilter::parse)
            .unwrap_or_default();
        let path_filter = filter.path.clone();
        // History is the long pole; HEAD and ref decorations are independent of
        // it, so they run concurrently instead of serializing three walks behind
        // one another on a large repository.
        // A named-scope walk deliberately leaves some refs out. Probing what
        // it left out only makes sense when the scope is what chose the tips:
        // an explicit `revision` already narrows the walk on the caller's
        // orders, and RefScope::All leaves nothing out to report.
        //
        // Paging is excluded too. What a scope hides is a property of the
        // repository's refs, not of the page being read, so re-probing on
        // every "load more" would spend a subprocess to recompute a warning
        // the first page already carries.
        let probe_hidden =
            revision.is_none() && ref_scope == RefScope::Named && skip.unwrap_or(0) == 0;
        let (history, head, refs, default_branch, hidden) = std::thread::scope(|scope| {
            let history = scope.spawn(move || {
                GitReader::read_commit_history_paged(
                    &repo,
                    skip.unwrap_or(0),
                    fetch_limit,
                    rev.as_deref(),
                    path_filter.as_deref(),
                    ref_scope,
                )
            });
            let repo2 = repo_path.clone();
            let head = scope.spawn(move || GitReader::head_id(&repo2));
            let repo3 = repo_path.clone();
            let refs = scope.spawn(move || crate::graph::list_ref_decorations(&repo3, ref_scope));
            let repo4 = repo_path.clone();
            let default_branch = scope.spawn(move || GitReader::default_branch_name(&repo4));
            let repo5 = repo_path.clone();
            let hidden = scope.spawn(move || {
                probe_hidden
                    .then(|| crate::graph::probe_hidden_history(&repo5))
                    .transpose()
            });
            (
                history
                    .join()
                    .unwrap_or_else(|_| Err("history walker panicked".into())),
                head.join(),
                refs.join(),
                default_branch.join(),
                hidden.join(),
            )
        });
        // A failed HEAD or decoration probe degrades exactly one facet of an
        // otherwise-good payload; record it instead of rendering a fallback
        // indistinguishable from an honest empty set (ai's warnings pattern).
        let head_id = match head {
            Ok(Ok(id)) => Some(id),
            Ok(Err(err)) => {
                warnings.push(format!(
                    "HEAD unavailable ({err}); commit graph may lack the HEAD marker"
                ));
                None
            }
            Err(_) => {
                warnings.push(
                    "background task failed (thread panic): HEAD probe died; commit graph may \
                     lack the HEAD marker"
                        .into(),
                );
                None
            }
        };
        let refs = match refs {
            Ok(Ok(listing)) => {
                // A capped label set must not pass for a complete one: the
                // rows are still drawn, so silence here would leave a chip
                // missing with nothing to explain it.
                if let Some(note) = listing.truncation_warning() {
                    warnings.push(note);
                }
                listing.decorations
            }
            Ok(Err(err)) => {
                warnings.push(format!(
                    "ref decorations unavailable ({err}); branches/tags will not be labeled"
                ));
                Vec::new()
            }
            Err(_) => {
                warnings.push(
                    "background task failed (thread panic): ref decoration walk died; \
                     branches/tags will not be labeled"
                        .into(),
                );
                Vec::new()
            }
        };
        // The default-branch probe only picks WHICH branch is kept straight;
        // losing it degrades to the conventional names, then HEAD — recorded
        // so a mainline anchored on the wrong branch is never a silent guess.
        let default_branch = match default_branch {
            Ok(Ok(name)) => name,
            Ok(Err(err)) => {
                warnings.push(format!(
                    "default branch unresolved ({err}); the straight mainline column is \
                     anchored on a conventional branch name or HEAD"
                ));
                None
            }
            Err(_) => {
                warnings.push(
                    "background task failed (thread panic): default branch probe died; the \
                     straight mainline column is anchored on a conventional branch name or HEAD"
                        .into(),
                );
                None
            }
        };

        // History outside the walked scope is data, not an error: it is named
        // so the reader can tell "GitPulse is not drawing this" apart from
        // "this does not exist" — the same reason a dangling edge draws a stub
        // instead of a line to whatever sits on the next row.
        match hidden {
            Ok(Ok(Some(hidden))) => {
                if let Some(note) = crate::graph::hidden_ref_warning(&hidden) {
                    warnings.push(note);
                }
            }
            Ok(Ok(None)) => {}
            Ok(Err(err)) => warnings.push(format!(
                "hidden-ref probe failed ({err}); refs outside branches, remotes and tags may \
                 hold history this graph does not draw"
            )),
            Err(_) => warnings.push(
                "background task failed (thread panic): hidden-ref probe died; refs outside \
                 branches, remotes and tags may hold history this graph does not draw"
                    .into(),
            ),
        }

        let raw_commits = history?;
        Ok(assemble_commit_graph(
            raw_commits,
            max_commits,
            &filter,
            refs,
            default_branch.as_deref(),
            head_id,
            warnings,
        ))
    })
    .await
}

/// Rows to request from the history walker for one graph page: the user's cap
/// plus one has_more probe row (dropped before lanes are solved).
fn graph_fetch_limit(max_commits: usize) -> usize {
    max_commits.clamp(1, GitReader::MAX_HISTORY_COMMITS) + 1
}

#[tauri::command(async)]
pub async fn cmd_get_file_diff(
    repo_path: String,
    file_path: String,
    is_staged: bool,
    ignore_whitespace: Option<bool>,
) -> Result<DiffPayload, String> {
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
pub async fn cmd_get_commit_diff(
    repo_path: String,
    commit_id: String,
) -> Result<DiffPayload, String> {
    off_thread(move || GitReader::get_commit_diff(&repo_path, &commit_id)).await
}

#[tauri::command(async)]
pub async fn cmd_get_commit_file_diff(
    repo_path: String,
    commit_id: String,
    file_path: String,
) -> Result<DiffPayload, String> {
    off_thread(move || GitReader::get_commit_file_diff(&repo_path, &commit_id, &file_path)).await
}

#[tauri::command(async)]
pub async fn cmd_get_range_diff(
    repo_path: String,
    from: String,
    to: String,
) -> Result<DiffPayload, String> {
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
pub async fn cmd_stage_file(repo_path: String, file_path: String) -> Result<Guarded<()>, String> {
    off_thread(move || {
        // GitWriter uses this literal pathspec, so the harness judges the
        // command that will actually reach Git rather than a paraphrase.
        let spec = format!(":(literal){file_path}");
        let argv = ["git", "add", "--", spec.as_str()];
        let policy = guard(&repo_path, &argv)?;
        GitWriter::stage_file(&repo_path, &file_path)?;
        Ok(Guarded { policy, output: () })
    })
    .await
}

#[tauri::command(async)]
pub async fn cmd_unstage_file(repo_path: String, file_path: String) -> Result<Guarded<()>, String> {
    off_thread(move || {
        let spec = format!(":(literal){file_path}");
        let argv = ["git", "restore", "--staged", "--", spec.as_str()];
        let policy = guard(&repo_path, &argv)?;
        GitWriter::unstage_file(&repo_path, &file_path)?;
        Ok(Guarded { policy, output: () })
    })
    .await
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

/// Stages every tracked change and untracked non-ignored file, then commits,
/// after the harness has judged both command lines this would run.
///
/// Interactive "quick commit" (commit-all) is one mutation, not `stageAll`
/// followed by `cmd_commit`: two lock acquisitions can absorb a concurrent
/// writer's index.
#[tauri::command(async)]
pub async fn cmd_quick_commit(
    repo_path: String,
    message: String,
) -> Result<Guarded<String>, String> {
    off_thread(move || {
        let add_argv: Vec<&str> = std::iter::once("git")
            .chain(GitWriter::QUICK_COMMIT_ADD_ARGV.iter().copied())
            .collect();
        let add_policy = guard(&repo_path, &add_argv)?;
        let commit_owned = commit_amend_argv(&message, false);
        let commit_argv: Vec<&str> = commit_owned.iter().map(String::as_str).collect();
        let commit_policy = guard(&repo_path, &commit_argv)?;
        let policy = strictest_verdict(add_policy, commit_policy);
        let output = GitWriter::quick_commit(&repo_path, &message)?;
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
pub async fn cmd_list_repo_files(repo_path: String) -> Result<Vec<String>, String> {
    off_thread(move || GitReader::list_repo_files(&repo_path)).await
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
) -> Result<crate::stack::StackHierarchyPayload, String> {
    off_thread(move || {
        let branches = GitReader::list_branches(&repo_path)?;
        // Ground truth for the root designation comes from the backend, not a
        // frontend guess: list_branches already resolved the default branch
        // from the primary remote's HEAD (origin, gitlab, company forks all
        // resolve). A nonexistent name here would leave every node rootless.
        let locals: Vec<&BranchInfo> = branches.iter().filter(|b| !b.is_remote).collect();
        let default_branch = locals
            .iter()
            .find(|b| b.is_default)
            .or_else(|| locals.iter().find(|b| b.is_current))
            .map(|b| b.name.clone())
            .unwrap_or_else(|| "main".to_string());

        let mut branch_tips = HashMap::new();
        for b in &locals {
            branch_tips.insert(b.name.clone(), b.tip_commit_id.clone());
        }

        let raw_commits = GitReader::read_commit_history(&repo_path, 2000, None)?;
        let mut commit_parents = HashMap::new();
        for c in raw_commits {
            commit_parents.insert(c.id, c.parent_ids);
        }

        let mut nodes =
            StackTreeEngine::build_stack_hierarchy(&branch_tips, &commit_parents, &default_branch);

        // Deep-history repair: the global window truncates long-lived repos,
        // which used to silently misreport deep-stacked branches as roots.
        // Each still-unresolved branch gets ONE bounded first-parent walk that
        // stops at any known tip; discovered edges are merged into the parent
        // map and the hierarchy is rebuilt once. A walk that exhausts its cap
        // leaves the branch honestly parentless — no invented hierarchy.
        const WALK_CAP: usize = GitReader::MAX_HISTORY_COMMITS;
        let tip_oids: HashSet<String> = branch_tips.values().cloned().collect();
        let mut enriched = false;
        for node in &nodes {
            if node.parent_branch_name.is_some() || node.branch_name == default_branch {
                continue;
            }
            let mut stop_at = tip_oids.clone();
            stop_at.remove(&node.tip_commit_id);
            if let Ok(Some(chain)) =
                GitReader::first_parent_chain(&repo_path, &node.tip_commit_id, &stop_at, WALK_CAP)
            {
                for edge in chain.windows(2) {
                    // Never overwrite richer multi-parent data already
                    // present from the global window.
                    commit_parents
                        .entry(edge[1].clone())
                        .or_insert_with(|| vec![edge[0].clone()]);
                }
                enriched = true;
                // Ok(None) (cap exhausted) and Err (git failed) leave the
                // branch honestly parentless rather than inventing a base.
            }
        }
        if enriched {
            nodes = StackTreeEngine::build_stack_hierarchy(
                &branch_tips,
                &commit_parents,
                &default_branch,
            );
        }

        // The breadcrumb trail describes the checked-out branch's ancestry;
        // with nothing checked out it degrades to the default branch.
        let focus = locals
            .iter()
            .find(|b| b.is_current)
            .map(|b| b.name.as_str())
            .unwrap_or(default_branch.as_str());
        let breadcrumb = StackTreeEngine::get_ancestry_breadcrumbs(&nodes, focus);

        Ok(crate::stack::StackHierarchyPayload {
            nodes,
            breadcrumb,
            default_branch,
        })
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
pub async fn cmd_list_tags(
    repo_path: String,
) -> Result<crate::engine::git_reader::TagList, String> {
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
pub async fn cmd_get_language_stats(repo_path: String) -> Result<LanguageStatsReport, String> {
    off_thread(move || GitReader::get_repo_language_stats(&repo_path)).await
}

#[tauri::command(async)]
pub async fn cmd_get_pulse_report(
    repo_path: String,
    max_commits: Option<usize>,
) -> Result<PulseReport, String> {
    off_thread(move || GitReader::pulse_report(&repo_path, max_commits)).await
}

#[tauri::command(async)]
pub async fn cmd_get_knowledge_report(
    repo_path: String,
    max_files: Option<usize>,
) -> Result<KnowledgeReport, String> {
    off_thread(move || GitReader::knowledge_report(&repo_path, max_files)).await
}

#[tauri::command(async)]
pub async fn cmd_get_dora_report(
    repo_path: String,
    window_days: Option<u32>,
) -> Result<DoraReport, String> {
    off_thread(move || GitReader::dora_report(&repo_path, window_days)).await
}

#[tauri::command(async)]
pub async fn cmd_record_pulse_snapshot(
    repo_path: String,
    snapshot: crate::ledger::PulseSnapshotInput,
) -> Result<(), String> {
    off_thread(move || {
        let repo = validate_repo(&repo_path)?;
        let address = crate::ledger::bindings::repository_address(&repo.to_string_lossy())
            .map_err(|error| error.to_string())?;
        crate::ledger::save_pulse_snapshot(&address.anchor, &snapshot)
            .map_err(|error| error.to_string())
    })
    .await
}

#[tauri::command(async)]
pub async fn cmd_get_pulse_snapshots(
    repo_path: String,
    limit: Option<usize>,
) -> Result<Vec<crate::ledger::PulseSnapshotEntry>, String> {
    off_thread(move || {
        let repo = validate_repo(&repo_path)?;
        let address = crate::ledger::bindings::repository_address(&repo.to_string_lossy())
            .map_err(|error| error.to_string())?;
        crate::ledger::get_pulse_snapshots(&address.anchor, limit)
            .map_err(|error| error.to_string())
    })
    .await
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
pub async fn cmd_fetch(
    repo_path: String,
    remote: Option<String>,
) -> Result<Guarded<String>, String> {
    off_thread(move || {
        let argv: Vec<String> = match remote.as_deref() {
            Some(remote) => vec!["git".into(), "fetch".into(), remote.into()],
            None => vec![
                "git".into(),
                "fetch".into(),
                "--all".into(),
                "--prune".into(),
            ],
        };
        let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
        let policy = guard(&repo_path, &refs)?;
        let output = GitWriter::fetch(&repo_path, remote.as_deref())?;
        Ok(Guarded { policy, output })
    })
    .await
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
    fork_point: Option<String>,
) -> Result<Guarded<String>, String> {
    off_thread(move || {
        // Plan, judge, and execute under ONE mutation-lock span: the upstream
        // resolved here is frozen into both the argv the gate judges and the
        // argv GitWriter executes, so the verdict can no longer describe a
        // line that did not run (the old plan-then-lock TOCTOU).
        let repo = validate_repo(&repo_path)?;
        let repo_lock = crate::engine::git_writer::repo_mutation_lock(&repo);
        let _lock_guard = repo_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        validate_ref_name(&branch)?;
        validate_ref_name(&onto)?;
        // A cascade supplies the parent tip it read the stack at; every other
        // caller passes none and gets the computed fork point.
        let upstream = GitWriter::prepare_restack(&repo, &branch, &onto, fork_point.as_deref())?;
        let planned = [
            "git",
            "rebase",
            "--onto",
            onto.as_str(),
            upstream.as_str(),
            branch.as_str(),
        ];
        let policy = guard(&repo_path, &planned)?;
        let output = GitWriter::execute_restack(&repo, &branch, &onto, &upstream)?;
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
        let mut argv = vec!["git", "stash", "push", "-u"];
        if message.is_some() {
            argv.extend(["-m", message.as_deref().unwrap_or_default()]);
        }
        let policy = guard(&repo_path, &argv)?;
        let output = GitWriter::stash_save(&repo_path, message.as_deref())?;
        Ok(Guarded { policy, output })
    })
    .await
}

#[tauri::command(async)]
pub async fn cmd_stash_pop(repo_path: String) -> Result<Guarded<String>, String> {
    off_thread(move || {
        let policy = guard(&repo_path, &["git", "stash", "pop"])?;
        let output = GitWriter::stash_pop(&repo_path)?;
        Ok(Guarded { policy, output })
    })
    .await
}

// --- stash -------------------------------------------------------------

/// Lists the stash stack. Read-only and ungated.
#[tauri::command(async)]
pub async fn cmd_stash_list(repo_path: String) -> Result<Vec<StashEntry>, String> {
    off_thread(move || crate::engine::stash::list(&repo_path)).await
}

/// Renders the diff a stash entry holds, addressed by object id.
#[tauri::command(async)]
pub async fn cmd_stash_show(repo_path: String, oid: String) -> Result<String, String> {
    off_thread(move || crate::engine::stash::show(&repo_path, &oid)).await
}

/// Applies, pops, or drops a stash entry.
///
/// `expected_oid` is what the client believed `index` held. The engine
/// re-resolves the index under the repository lock and refuses on a mismatch,
/// so a stale list can fail loudly but can never destroy an entry the user did
/// not choose — the stash stack is shared with every other worktree, client
/// and agent touching this repository.
#[tauri::command(async)]
pub async fn cmd_stash_action(
    repo_path: String,
    action: StashAction,
    index: usize,
    expected_oid: String,
) -> Result<Guarded<String>, String> {
    off_thread(move || {
        let (policy, output) = crate::engine::stash::run_action_with(
            &repo_path,
            action,
            index,
            &expected_oid,
            |argv| guard(&repo_path, argv),
        )?;
        Ok(Guarded { policy, output })
    })
    .await
}

// --- replaying and rewinding commits -----------------------------------

/// Replays commits onto the current branch.
///
/// A conflict parks the repository mid-cherry-pick, which `cmd_repo_operation`
/// then reports and the banner offers continue/skip/abort for. That is a
/// supported outcome, not a failure to roll back.
#[tauri::command(async)]
pub async fn cmd_cherry_pick(
    repo_path: String,
    commits: Vec<String>,
    no_commit: Option<bool>,
) -> Result<Guarded<String>, String> {
    off_thread(move || {
        let no_commit = no_commit.unwrap_or(false);
        let argv = GitWriter::replay_argv("cherry-pick", &commits, no_commit);
        let policy = guard(&repo_path, &argv)?;
        let output = GitWriter::cherry_pick(&repo_path, &commits, no_commit)?;
        Ok(Guarded { policy, output })
    })
    .await
}

/// Records the inverse of the given commits as new commits.
#[tauri::command(async)]
pub async fn cmd_revert(
    repo_path: String,
    commits: Vec<String>,
    no_commit: Option<bool>,
) -> Result<Guarded<String>, String> {
    off_thread(move || {
        let no_commit = no_commit.unwrap_or(false);
        let argv = GitWriter::replay_argv("revert", &commits, no_commit);
        let policy = guard(&repo_path, &argv)?;
        let output = GitWriter::revert(&repo_path, &commits, no_commit)?;
        Ok(Guarded { policy, output })
    })
    .await
}

/// Moves the current branch to `target`.
///
/// `ResetMode::Hard` destroys uncommitted work irrecoverably; the mode is an
/// enum so the rendered line the gate judges names the exact flag, and the UI
/// can require its own confirmation from `discards_working_tree`.
#[tauri::command(async)]
pub async fn cmd_reset(
    repo_path: String,
    mode: ResetMode,
    target: String,
) -> Result<Guarded<String>, String> {
    off_thread(move || {
        let argv = GitWriter::reset_argv(mode, &target);
        let policy = guard(&repo_path, &argv)?;
        let output = GitWriter::reset(&repo_path, mode, &target)?;
        Ok(Guarded { policy, output })
    })
    .await
}

// --- remotes -----------------------------------------------------------

/// Lists configured remotes with their fetch/push URLs. Read-only and ungated.
#[tauri::command(async)]
pub async fn cmd_list_remotes(repo_path: String) -> Result<RemoteList, String> {
    off_thread(move || crate::engine::remotes::list(&repo_path)).await
}

/// Adds, removes, renames, repoints, or prunes a remote.
///
/// The gate runs inside the engine, under the repository lock and against the
/// argv that executes — the same discipline every other mutating path follows.
#[tauri::command(async)]
pub async fn cmd_remote_change(
    repo_path: String,
    change: RemoteChange,
) -> Result<Guarded<String>, String> {
    off_thread(move || {
        let (policy, output) = crate::engine::remotes::apply_with(&repo_path, &change, |argv| {
            guard(&repo_path, argv)
        })?;
        Ok(Guarded { policy, output })
    })
    .await
}

// --- submodules --------------------------------------------------------

/// Lists embedded submodules and whether each is usable. Read-only and ungated.
#[tauri::command(async)]
pub async fn cmd_list_submodules(repo_path: String) -> Result<SubmoduleList, String> {
    off_thread(move || crate::engine::submodules::list(&repo_path)).await
}

/// Initializes, syncs, or deinitializes submodules.
#[tauri::command(async)]
pub async fn cmd_submodule_change(
    repo_path: String,
    change: SubmoduleChange,
) -> Result<Guarded<String>, String> {
    off_thread(move || {
        let (policy, output) =
            crate::engine::submodules::apply_with(&repo_path, &change, |argv| {
                guard(&repo_path, argv)
            })?;
        Ok(Guarded { policy, output })
    })
    .await
}

/// Reports the multi-step git operation this worktree is parked in, if any.
///
/// Read-only and ungated: it runs on every status refresh, and putting a
/// sidecar round trip in front of a background read would buy no decision.
#[tauri::command(async)]
pub async fn cmd_repo_operation(repo_path: String) -> Result<Option<RepoOperation>, String> {
    off_thread(move || {
        let repo = validate_repo(&repo_path)?;
        crate::engine::repo_op::detect(&repo)
    })
    .await
}

/// Aborts, continues, or skips the parked operation.
///
/// The gate judges the argv that [`crate::engine::repo_op::action_argv`]
/// renders, and `run_action` re-renders the same line from the same function
/// under the repo lock — so the judged line is the executed line. The kind is
/// re-detected on both sides rather than trusted from the client: a stale UI
/// asking to abort a rebase that has since become a cherry-pick must not send
/// `git rebase --abort`.
#[tauri::command(async)]
pub async fn cmd_repo_operation_action(
    repo_path: String,
    action: OperationAction,
) -> Result<Guarded<String>, String> {
    off_thread(move || {
        // The gate runs inside `run_action_with`, under the repository lock and
        // against the same argv that executes. Detecting and judging out here
        // would let a concurrent abort change the operation in between, so the
        // gate would approve `git rebase --abort` while a cherry-pick abort is
        // what actually ran.
        let (policy, output) =
            crate::engine::repo_op::run_action_with(&repo_path, action, |argv| {
                guard(&repo_path, argv)
            })?;
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
        let op = discard_file_op(&repo_path, &file_path);
        let policy = crate::harness::guard_file(&repo_path, &file_path, op)?;
        GitWriter::discard_changes(&repo_path, &file_path)?;
        Ok(Guarded { policy, output: () })
    })
    .await
}

/// The file operation `GitWriter::discard_changes` will actually perform.
///
/// It runs `git restore` and `git clean -f` against the same path, so the act
/// depends on whether the path is in the index: a tracked file is restored, an
/// untracked one is *removed* by the clean. Declaring "modify" for both sent
/// the policy sidecar — which receives `op` in `policy.check.file` — and the
/// ledger, which records it as `file.modify`, a weaker operation than the one
/// that ran, for exactly the case where the file is destroyed.
///
/// An unreadable index fails closed to the destructive reading: a probe that
/// could not run must not produce the gentler claim.
fn discard_file_op(repo_path: &str, file_path: &str) -> &'static str {
    match GitReader::is_tracked(repo_path, file_path) {
        Ok(true) => "modify",
        Ok(false) | Err(_) => "delete",
    }
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
            prs_truncated: false,
            workflow_runs: Vec::new(),
            runs_error: None,
            runs_truncated: false,
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
        // Validate before anything external runs, then judge the exact argv
        // create_issue executes — built once by the shared `issue_create_argv`
        // builder (program name included) and consumed by both the gate and
        // the executor, so the judged line can never drift from the run one.
        validate_issue_payload(&title, &body, &labels)?;
        let remote = discover_github_remote(&repo_path)?
            .ok_or_else(|| "No GitHub remote configured".to_string())?;
        let argv_owned = issue_create_argv(&remote, &title, &body, &labels);
        let refs: Vec<&str> = argv_owned.iter().map(String::as_str).collect();
        let policy = guard(&repo_path, &refs)?;
        let output = create_issue(&repo_path, &remote, &title, &body, &labels)?;
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
        // pr_checkout_argv refuses a fabricated number before discovery, so
        // an invalid request never produces a judged-but-unexecutable line.
        // One remote discovery feeds both the gate and the executor: the
        // `--repo` the gate approved is exactly the one gh is pinned to.
        let remote = discover_github_remote(&repo_path)?
            .ok_or_else(|| "No GitHub remote configured".to_string())?;
        let argv_owned = pr_checkout_argv(&remote, number)?;
        let refs: Vec<&str> = argv_owned.iter().map(String::as_str).collect();
        let policy = guard(&repo_path, &refs)?;
        let output = checkout_pull_request(&repo_path, &remote, number)?;
        Ok(Guarded { policy, output })
    })
    .await
}

/// The repository's Actions workflows (`gh workflow list`). A separate,
/// lazily-loaded command rather than another section of
/// [`cmd_github_context`]: that call already serializes four gh round trips,
/// and the workflows list is only needed on its own panel.
#[tauri::command(async)]
pub async fn cmd_github_workflows(repo_path: String) -> crate::github::actions::WorkflowsReport {
    off_thread(move || Ok::<_, String>(crate::github::actions::load_workflows_report(&repo_path)))
        .await
        .unwrap_or_else(|e| crate::github::actions::WorkflowsReport::unavailable(false, Some(e)))
}

/// Dispatches a workflow (`gh workflow run <selector> --ref <ref>`), after
/// the command gate has judged the exact line. The selector is the workflow's
/// file path from the listing; only dispatchable workflows succeed upstream.
#[tauri::command(async)]
pub async fn cmd_github_trigger_workflow(
    repo_path: String,
    workflow: String,
    git_ref: String,
) -> Result<Guarded<String>, String> {
    off_thread(move || {
        let remote = discover_github_remote(&repo_path)?
            .ok_or_else(|| "No GitHub remote configured".to_string())?;
        let argv_owned =
            crate::github::actions::trigger_workflow_argv(&remote, &workflow, &git_ref)?;
        let refs: Vec<&str> = argv_owned.iter().map(String::as_str).collect();
        let policy = guard(&repo_path, &refs)?;
        let output =
            crate::github::actions::trigger_workflow(&repo_path, &remote, &workflow, &git_ref)?;
        Ok(Guarded { policy, output })
    })
    .await
}

/// Re-runs a workflow run (`gh run rerun <id>`), gated like every action.
#[tauri::command(async)]
pub async fn cmd_github_rerun_run(
    repo_path: String,
    run_id: u64,
) -> Result<Guarded<String>, String> {
    off_thread(move || {
        let remote = discover_github_remote(&repo_path)?
            .ok_or_else(|| "No GitHub remote configured".to_string())?;
        let argv_owned = crate::github::actions::rerun_run_argv(&remote, run_id)?;
        let refs: Vec<&str> = argv_owned.iter().map(String::as_str).collect();
        let policy = guard(&repo_path, &refs)?;
        let output = crate::github::actions::rerun_workflow_run(&repo_path, &remote, run_id)?;
        Ok(Guarded { policy, output })
    })
    .await
}

/// Cancels an in-flight workflow run (`gh run cancel <id>`), gated.
#[tauri::command(async)]
pub async fn cmd_github_cancel_run(
    repo_path: String,
    run_id: u64,
) -> Result<Guarded<String>, String> {
    off_thread(move || {
        let remote = discover_github_remote(&repo_path)?
            .ok_or_else(|| "No GitHub remote configured".to_string())?;
        let argv_owned = crate::github::actions::cancel_run_argv(&remote, run_id)?;
        let refs: Vec<&str> = argv_owned.iter().map(String::as_str).collect();
        let policy = guard(&repo_path, &refs)?;
        let output = crate::github::actions::cancel_workflow_run(&repo_path, &remote, run_id)?;
        Ok(Guarded { policy, output })
    })
    .await
}

/// CI:local — runs the repository's CI pipeline on this machine.
///
/// Every step is judged by the harness command gate and reaches the ledger
/// with its verdict; a completed run against a clean tree is recorded as a
/// git-native verification note on HEAD. See `ci_local` for both.
#[tauri::command(async)]
pub async fn cmd_ci_local(repo_path: String) -> Result<crate::ci_local::CiLocalReport, String> {
    off_thread(move || crate::ci_local::run_ci_local(&repo_path)).await
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
    // start_watch shells out to git (validate_repo, resolve_git_dir/common
    // dir) and installs OS watches — the same blocking profile as every other
    // reader command, so it belongs on the blocking pool rather than an async
    // worker thread. WatcherState is an Arc-backed Clone; snapshot it so the
    // 'static closure does not borrow the request-scoped State.
    let watcher_state = state.inner().clone();
    off_thread(move || start_watch(app, &watcher_state, repo_path)).await
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
/// Ranking, strictest first:
/// Blocked > Warned > Widened > Granted > Degraded > Demoted > Unchecked > Allowed.
///
/// Blocked never actually reaches here as a survivor — `guard` refuses on it —
/// but the ranking still documents intent. A warning is an explicit rule
/// firing, a demotion is posture noise, an unchecked gate is an absence of a
/// check, and a clean allow carries nothing worth surfacing over its peers.
///
/// The three middle rungs are all "a rule fired and something waived it", and
/// they are ordered by who did the waiving and how long it lasts. `Widened`
/// outranks `Granted` because a grant expires and appended scope does not: the
/// widening is still there in the next run with no grant beside it, so it is
/// the one worth surfacing when a multi-command action produced both.
/// `Degraded` sits below them because no rule fired at all — some simply could
/// not be asked — but above `Demoted`, which is a posture the user chose.
fn strictest_verdict(
    a: crate::harness::PolicyVerdict,
    b: crate::harness::PolicyVerdict,
) -> crate::harness::PolicyVerdict {
    use crate::harness::PolicyStatus::*;
    let rank = |v: &crate::harness::PolicyVerdict| match v.status {
        Blocked => 8,
        Warned => 7,
        Widened => 6,
        Granted => 5,
        Degraded => 4,
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
    off_thread(move || Ok::<_, String>(crate::harness::check_command(&repo_path, &command, None)))
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
pub async fn cmd_ai_coverage_report(
    repo_path: String,
    report: String,
    base_url: Option<String>,
    model: Option<String>,
) -> Result<crate::ai::AiGeneration, String> {
    off_thread(move || crate::ai::coverage_report(&repo_path, &report, selection(base_url, model)))
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `git init` + one commit, plus an untracked file on disk.
    fn repo_with_tracked_and_untracked() -> tempfile::TempDir {
        let dir = tempfile::TempDir::new().unwrap();
        let run = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(dir.path())
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@e")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@e")
                .output()
                .expect("git runs");
        };
        run(&["init", "-b", "main"]);
        std::fs::write(dir.path().join("README.md"), "# t\n").unwrap();
        run(&["add", "."]);
        run(&["commit", "-m", "seed"]);
        std::fs::write(dir.path().join("fresh.txt"), "new\n").unwrap();
        dir
    }

    #[test]
    fn discard_declares_delete_for_the_file_it_will_delete() {
        // `discard_changes` runs `restore` AND `clean -f`. For an untracked
        // path the clean is what acts, and it removes the file — so declaring
        // "modify" asked the policy sidecar to judge, and the ledger to
        // record, a gentler operation than the one that ran.
        let dir = repo_with_tracked_and_untracked();
        let root = dir.path().to_string_lossy().to_string();

        assert_eq!(discard_file_op(&root, "README.md"), "modify");
        assert_eq!(discard_file_op(&root, "fresh.txt"), "delete");
    }

    #[test]
    fn discard_fails_closed_when_the_index_cannot_be_read() {
        // A probe that could not run must not produce the gentler claim.
        assert_eq!(
            discard_file_op("/definitely/not/a/repository", "any.txt"),
            "delete"
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
            task_id: String::new(),
            grant_id: String::new(),
            granted_by: String::new(),
            widened: String::new(),
            degraded: Vec::new(),
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
    fn quick_commit_gates_add_all_then_commit_minus_m() {
        assert_eq!(GitWriter::QUICK_COMMIT_ADD_ARGV, &["add", "--all"]);
        let add_argv: Vec<&str> = std::iter::once("git")
            .chain(GitWriter::QUICK_COMMIT_ADD_ARGV.iter().copied())
            .collect();
        assert_eq!(add_argv, vec!["git", "add", "--all"]);
        assert_eq!(
            commit_amend_argv("feat: all", false),
            vec!["git", "commit", "-m", "feat: all"]
        );
    }

    #[test]
    fn reworded_message_keeps_body_rewrites_subject() {
        assert_eq!(reworded_message("old\n\nbody", "new"), "new\n\nbody");
        assert_eq!(reworded_message("subject only", "new"), "new");
    }

    /// Lockstep contract with GitReader::page_count_limit: the probe row this
    /// fetch-limit appends must survive the reader's clamp, including at
    /// exactly MAX_HISTORY_COMMITS, or has_more dies at the ceiling.
    #[test]
    fn graph_fetch_limit_yields_the_probe_row_even_at_the_ceiling() {
        let max = GitReader::MAX_HISTORY_COMMITS;
        assert_eq!(
            graph_fetch_limit(max),
            max + 1,
            "probe row must exist at exactly the ceiling"
        );
        assert_eq!(graph_fetch_limit(max + 10), max + 1);
        assert_eq!(graph_fetch_limit(0), 2);
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

    /// Bare git runner for tests that only need a side effect (no output).
    fn run_git(cwd: &std::path::Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .current_dir(cwd)
            .output()
            .expect("spawn git");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
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

    /// The restack upstream resolver finds the fork point (here: where feature
    /// branched from main) and refuses invalid refs before anything is judged.
    /// The full rebase line is assembled in [`cmd_restack`] from this frozen
    /// value under one lock span, so planner and executor cannot drift.
    #[test]
    fn restack_upstream_resolution_resolves_fork_point() {
        let (dir, base, _c_a, _c_b) = init_repo_with_branch_commits();
        run_git(dir.path(), &["branch", "feature", &base]);
        let canon = validate_repo(dir.path().to_str().unwrap()).unwrap();
        let upstream = GitWriter::prepare_restack(&canon, "feature", "main", None).unwrap();
        assert_eq!(upstream, base, "upstream must resolve to the fork base");
        // Invalid refs are refused before anything is rendered or judged.
        assert!(GitWriter::prepare_restack(&canon, "-evil", "main", None).is_err());
    }

    /// A cascade names the fork point it read the stack at. It is honoured
    /// when it really is an ancestor, and refused — never silently replaced
    /// by the computed one — when the stack has moved since it was read.
    #[test]
    fn restack_honours_a_caller_supplied_fork_point_and_refuses_a_stale_one() {
        let (dir, base, c_a, _c_b) = init_repo_with_branch_commits();
        run_git(dir.path(), &["branch", "feature", &base]);
        let canon = validate_repo(dir.path().to_str().unwrap()).unwrap();

        // `base` is genuinely an ancestor of `feature`, so it is taken as-is.
        let honoured =
            GitWriter::prepare_restack(&canon, "feature", "main", Some(base.as_str())).unwrap();
        assert_eq!(honoured, base);

        // `c_a` sits on the other branch, so it is not an ancestor of
        // `feature`: the plan the caller built no longer describes this
        // repository and the restack must not proceed on a widened range.
        let err = GitWriter::prepare_restack(&canon, "feature", "main", Some(c_a.as_str()))
            .expect_err("a non-ancestor fork point must be refused");
        assert!(err.contains("no longer the stack on disk"), "{err}");

        // Anything that is not an object id is refused before it reaches argv.
        assert!(
            GitWriter::prepare_restack(&canon, "feature", "main", Some("--exec=boom")).is_err()
        );
    }

    /// The payload carries assembly warnings through a serde round trip when
    /// present, and a payload from before the field existed still deserializes
    /// to an empty vec instead of failing (the `#[serde(default)]` contract).
    #[test]
    fn commit_graph_payload_round_trips_warnings() {
        let warning = "HEAD unavailable (bad object HEAD); commit graph may lack the HEAD marker";
        let payload = CommitGraphPayload {
            rows: Vec::new(),
            head_id: None,
            refs: Vec::new(),
            has_more: false,
            warnings: vec![warning.to_string()],
            mainline_id: Some("abc".to_string()),
            mainline_name: Some("main".to_string()),
        };
        let json = serde_json::to_string(&payload).unwrap();
        assert!(
            json.contains(&format!(r#""warnings":["{warning}"]"#)),
            "warnings must serialize verbatim, got {json}"
        );
        let back: CommitGraphPayload = serde_json::from_str(&json).unwrap();
        assert_eq!(back.warnings, vec![warning]);

        // `folds` was shipped (and never read) by earlier builds; a payload
        // carrying it must still deserialize.
        let legacy = r#"{"rows":[],"folds":[],"head_id":null,"refs":[],"has_more":false}"#;
        let back: CommitGraphPayload = serde_json::from_str(legacy).unwrap();
        assert!(back.warnings.is_empty(), "absent field defaults to empty");
        assert!(back.mainline_id.is_none() && back.mainline_name.is_none());
    }

    fn decoration(name: &str, kind: RefKind, commit_id: &str, is_head: bool) -> RefDecoration {
        RefDecoration {
            name: name.to_string(),
            kind,
            commit_id: commit_id.to_string(),
            is_head,
        }
    }

    /// The default branch supplies every tip carrying its name, local first,
    /// all labelled with the local name; HEAD is the fallback anchor.
    #[test]
    fn mainline_hint_takes_the_default_branch_local_then_remotes() {
        let refs = vec![
            decoration("feature", RefKind::Local, "f1", true),
            decoration("main", RefKind::Local, "m1", false),
            decoration("origin/main", RefKind::Remote, "o1", false),
            decoration("upstream/main", RefKind::Remote, "u1", false),
            decoration("origin/feature", RefKind::Remote, "f1", false),
            decoration("v1", RefKind::Tag, "m1", false),
        ];
        let resolved = resolve_mainline_hint(&refs, Some("main"), Some("f1"));
        assert_eq!(
            resolved.hint,
            MainlineHint {
                branch_tips: vec!["m1".into(), "o1".into(), "u1".into()],
                fallback_tip: Some("f1".into()),
            }
        );
        assert_eq!(resolved.name_for("m1", &refs).as_deref(), Some("main"));
        assert_eq!(
            resolved.name_for("o1", &refs).as_deref(),
            Some("main"),
            "a remote copy of a local branch is still labelled by the branch"
        );
        assert_eq!(
            resolved.name_for("f1", &refs).as_deref(),
            Some("feature"),
            "the HEAD fallback is labelled with the checked-out branch"
        );
        assert_eq!(resolved.name_for("nowhere", &refs), None);
    }

    /// Without a resolved default branch the conventional names are probed
    /// in order; a remote-only branch is labelled by its remote ref.
    #[test]
    fn mainline_hint_falls_back_to_conventional_names_and_remote_only_labels() {
        let refs = vec![
            decoration("origin/master", RefKind::Remote, "r1", false),
            decoration("develop", RefKind::Local, "d1", false),
        ];
        let resolved = resolve_mainline_hint(&refs, None, None);
        assert_eq!(resolved.hint.branch_tips, vec!["r1".to_string()]);
        assert_eq!(resolved.hint.fallback_tip, None);
        assert_eq!(
            resolved.name_for("r1", &refs).as_deref(),
            Some("origin/master")
        );

        // A default branch that has no refs at all must NOT fall through to
        // another name: pinning `develop` when the repository's default is
        // `release` would straighten the wrong branch.
        let resolved = resolve_mainline_hint(&refs, Some("release"), Some("h"));
        assert_eq!(resolved.hint.branch_tips, vec!["r1".to_string()]);
        assert_eq!(
            resolve_mainline_hint(&[], Some("release"), Some("h")).hint,
            MainlineHint {
                branch_tips: Vec::new(),
                fallback_tip: Some("h".into()),
            }
        );
    }

    /// A detached HEAD is labelled `HEAD`; blank ids never become tips.
    #[test]
    fn mainline_hint_detached_head_and_blank_ids() {
        let refs = vec![decoration("HEAD", RefKind::Head, "h1", true)];
        let resolved = resolve_mainline_hint(&refs, Some(""), Some("h1"));
        assert!(resolved.hint.branch_tips.is_empty());
        assert_eq!(resolved.hint.fallback_tip.as_deref(), Some("h1"));
        assert_eq!(resolved.name_for("h1", &refs).as_deref(), Some("HEAD"));
        assert_eq!(
            resolve_mainline_hint(&refs, None, Some("  "))
                .hint
                .fallback_tip,
            None
        );
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

// ---------------------------------------------------------------------------
// Terminal: interactive PTY sessions plus bounded one-shot execution.
// The interactive shell is intentionally *not* gate-routed (a shell can run
// anything, so claiming otherwise would be a check that cannot run reporting
// what a check that ran reports); one-shot git commands still pass through
// harness::guard_command. See `terminal/mod.rs` for the full story.
// ---------------------------------------------------------------------------

/// Spawns an interactive shell in the open repository inside a real PTY.
/// Output streams back as `terminal-output` events; exit lands as
/// `terminal-exit`, both keyed by the returned session id.
#[tauri::command(async)]
#[allow(clippy::too_many_arguments)]
pub async fn cmd_terminal_spawn(
    app: AppHandle,
    state: State<'_, crate::terminal::TerminalSessions>,
    repo_path: String,
    rows: u16,
    cols: u16,
    program: Option<String>,
    args: Option<Vec<String>>,
    env: Option<std::collections::HashMap<String, String>>,
) -> Result<crate::terminal::TerminalSpawned, String> {
    // Fast allocation work only; nothing here blocks long enough to need the
    // thread pool.
    crate::terminal::spawn_session(&app, &state, &repo_path, rows, cols, program, args, env)
}

/// Feeds keystrokes into a live session's PTY.
#[tauri::command(async)]
pub async fn cmd_terminal_write(
    state: State<'_, crate::terminal::TerminalSessions>,
    session_id: String,
    data: String,
) -> Result<(), String> {
    crate::terminal::write_to_session(&state, &session_id, &data)
}

/// Resizes a live session's PTY to the frontend grid.
#[tauri::command(async)]
pub async fn cmd_terminal_resize(
    state: State<'_, crate::terminal::TerminalSessions>,
    session_id: String,
    rows: u16,
    cols: u16,
) -> Result<(), String> {
    crate::terminal::resize_session(&state, &session_id, rows, cols)
}

/// Kills a session's shell and frees its slot.
#[tauri::command(async)]
pub async fn cmd_terminal_kill(
    state: State<'_, crate::terminal::TerminalSessions>,
    session_id: String,
) -> Result<(), String> {
    crate::terminal::kill_session(&state, &session_id)
}

/// Runs one argv to completion with a hard timeout and capped tails.
///
/// Blocking by definition (up to MAX_RUN_TIMEOUT), so this belongs on the
/// same pool as every other long-running reader.
#[tauri::command(async)]
pub async fn cmd_terminal_run(
    repo_path: String,
    args: Vec<String>,
    timeout_secs: Option<u64>,
) -> Result<crate::terminal::TerminalRunResult, String> {
    off_thread(move || crate::terminal::run_terminal(&repo_path, &args, timeout_secs)).await
}

/// Runs one model-authored health or coverage command through the app's
/// purpose-specific allowlist and the MANVI command gate.
///
/// Keeping this separate from `cmd_terminal_run` makes the authority boundary
/// structural: plan text cannot reach the user-owned arbitrary argv console by
/// accidentally omitting a flag or spoofing an origin string.
#[tauri::command(async)]
pub async fn cmd_manvi_run_action(
    repo_path: String,
    args: Vec<String>,
    action_kind: crate::terminal::ManviActionKind,
    timeout_secs: Option<u64>,
) -> Result<crate::terminal::TerminalRunResult, String> {
    off_thread(move || {
        crate::terminal::run_manvi_action(&repo_path, &args, action_kind, timeout_secs)
    })
    .await
}

/// Checks whether a newer GitPulse release has been published.
///
/// Opt-in at the call site: the frontend invokes this only when the user has
/// enabled the preference or pressed "Check now". Nothing here reaches the
/// network on its own.
///
/// Infallible by design — every failure arrives as an [`UpdateCheck`] with
/// `checked: false` and a reason, so a transport error can never be rendered
/// as "you are up to date".
#[tauri::command(async)]
pub fn cmd_check_app_update() -> crate::updates::UpdateCheck {
    crate::updates::check_for_update()
}

// --- the action ledger -------------------------------------------------

/// Reads durable ledger events after `cursor`, oldest first.
///
/// The frontend's action list is a projection of this, not a store of its own:
/// it holds the last cursor it saw and asks for what followed. That is what
/// makes the history survive a reload, a crash, and a restart — and what makes
/// "everything visible is a projection of the log" true rather than aspirational.
#[tauri::command(async)]
pub async fn cmd_ledger_tail(
    repo_path: String,
    cursor: i64,
    limit: u32,
) -> Result<Vec<crate::ledger::LedgerEvent>, String> {
    off_thread(move || {
        let repo = validate_repo(&repo_path)?;
        let address = crate::ledger::bindings::repository_address(&repo.to_string_lossy())
            .map_err(|e| e.to_string())?;
        crate::ledger::tail(&address.anchor, cursor, limit).map_err(|e| e.to_string())
    })
    .await
}

/// Reports whether the ledger is recording for this repository.
///
/// Separate from the events themselves on purpose. A repository whose ledger
/// cannot be opened returns an empty history, and an empty history is exactly
/// what a repository with nothing in it returns; without this, the two would be
/// the same picture.
#[tauri::command(async)]
pub async fn cmd_ledger_status(repo_path: String) -> Result<crate::ledger::LedgerStatus, String> {
    off_thread(move || {
        let repo = validate_repo(&repo_path)?;
        crate::ledger::bindings::repository_status(&repo.to_string_lossy())
            .map_err(|e| e.to_string())
    })
    .await
}

// --- task binding ------------------------------------------------------

/// DevCouncil tasks currently leased in this repository.
///
/// Read-only. GitPulse never acquires, renews or releases a lease: those
/// contend with an active agent's writer lease, and a UI process that takes one
/// strands the task when the window closes.
#[tauri::command(async)]
pub async fn cmd_task_view(repo_path: String) -> Result<crate::tasks::TaskView, String> {
    off_thread(move || {
        let repo = validate_repo(&repo_path)?;
        let address = crate::ledger::bindings::repository_address(&repo.to_string_lossy())
            .map_err(|e| e.to_string())?;
        Ok(crate::tasks::view(&address.anchor))
    })
    .await
}

/// The scope one task declares, or `null` when the store or task is absent.
#[tauri::command(async)]
pub async fn cmd_task_scope(
    repo_path: String,
    task_id: String,
) -> Result<Option<crate::tasks::TaskScope>, String> {
    off_thread(move || {
        let repo = validate_repo(&repo_path)?;
        let address = crate::ledger::bindings::repository_address(&repo.to_string_lossy())
            .map_err(|e| e.to_string())?;
        crate::tasks::scope(&address.anchor, &task_id)
    })
    .await
}

/// Binds a worktree to a task, so every later mutation in it is judged against
/// that task's plan.
///
/// Recorded as a ledger event rather than a setting, so a later audit can say
/// which task a commit was made under — not merely which task the worktree is
/// bound to now.
#[tauri::command(async)]
pub async fn cmd_bind_worktree_task(
    repo_path: String,
    worktree_path: String,
    task_id: String,
) -> Result<i64, String> {
    off_thread(move || {
        crate::ledger::bindings::bind(&repo_path, &worktree_path, &task_id)
            .map_err(|e| e.to_string())
    })
    .await
}

/// Clears a worktree's task binding. The bind stays on the record.
#[tauri::command(async)]
pub async fn cmd_unbind_worktree_task(
    repo_path: String,
    worktree_path: String,
) -> Result<i64, String> {
    off_thread(move || {
        crate::ledger::bindings::unbind(&repo_path, &worktree_path).map_err(|e| e.to_string())
    })
    .await
}

/// The task a worktree is bound to, or `null`.
#[tauri::command(async)]
pub async fn cmd_worktree_task(
    repo_path: String,
    worktree_path: String,
) -> Result<Option<String>, String> {
    off_thread(move || {
        crate::ledger::bindings::resolve(&repo_path, &worktree_path).map_err(|e| e.to_string())
    })
    .await
}

// --- catching up ------------------------------------------------------

/// Replays what happened while GitPulse was closed.
///
/// Two sources, both observed rather than self-reported: git's reflog, which is
/// authoritative for ref movements and survives GitPulse being uninstalled, and
/// agent transcripts, which attribute file edits and commands to a session.
/// Both replays are idempotent, so this is safe to run on every repo open.
#[tauri::command(async)]
pub async fn cmd_catch_up(repo_path: String) -> Result<crate::ingest::CatchUp, String> {
    off_thread(move || {
        let repo = validate_repo(&repo_path)?;
        let address = crate::ledger::bindings::repository_address(&repo.to_string_lossy())
            .map_err(|e| e.to_string())?;
        Ok(crate::ingest::catch_up_into(
            &address.anchor,
            &address.worktree,
        ))
    })
    .await
}

// --- code intelligence (devmap in-process) ----------------------------

/// Status and metadata of the repository's code intelligence graph.
#[tauri::command(async)]
pub async fn cmd_codeintel_status(
    repo_path: String,
) -> Result<crate::codeintel::CodeintelStatus, String> {
    off_thread(move || Ok(crate::codeintel::status(&repo_path))).await
}

/// Symbol search across indexed code files.
#[tauri::command(async)]
pub async fn cmd_codeintel_search(
    repo_path: String,
    query: String,
    token_budget: Option<u32>,
) -> Result<crate::codeintel::CodeintelResponse<crate::codeintel::CodeintelSymbolHit>, String> {
    off_thread(move || Ok(crate::codeintel::search(&repo_path, &query, token_budget))).await
}

/// Blast radius and impact graph for a symbol or file.
#[tauri::command(async)]
pub async fn cmd_codeintel_impact(
    repo_path: String,
    target: String,
    token_budget: Option<u32>,
) -> Result<crate::codeintel::CodeintelResponse<crate::codeintel::CodeintelEdge>, String> {
    off_thread(move || Ok(crate::codeintel::impact(&repo_path, &target, token_budget))).await
}

/// Dead code analysis across the repository.
#[tauri::command(async)]
pub async fn cmd_codeintel_dead_symbols(
    repo_path: String,
    token_budget: Option<u32>,
) -> Result<crate::codeintel::CodeintelResponse<crate::codeintel::CodeintelDeadSymbol>, String> {
    off_thread(move || Ok(crate::codeintel::dead_symbols(&repo_path, token_budget))).await
}

/// One-shot Work-view snapshot: worktrees, agent sessions, collisions, ledger, code graph.
#[tauri::command(async)]
pub async fn cmd_insights_snapshot(
    repo_path: String,
) -> Result<crate::insights::InsightsSnapshot, String> {
    off_thread(move || Ok(crate::insights::snapshot(&repo_path))).await
}

/// Files with uncommitted changes in more than one worktree.
#[tauri::command(async)]
pub async fn cmd_collision_risk(
    repo_path: String,
) -> Result<crate::insights::CollisionRisk, String> {
    off_thread(move || Ok(crate::insights::collision_risk(&repo_path))).await
}

/// The cheap facet for a whole workspace of repositories, in one round trip.
///
/// Two `git` spawns per repository plus a read-only look at each repository's
/// own ledger. Deliberately narrower than `cmd_insights_snapshot`, which
/// probes every worktree and is priced for one repository on screen rather
/// than two dozen. Per-repository failures ride in their own facet.
#[tauri::command(async)]
pub async fn cmd_fleet_snapshot(
    repo_paths: Vec<String>,
) -> Result<crate::insights::FleetSnapshot, String> {
    off_thread(move || Ok(crate::insights::fleet_snapshot(&repo_paths))).await
}

/// Records one expensive scan's result in that repository's own ledger.
///
/// Called after a Fleet scan lands, so the number survives a restart and can
/// be shown with its age. Families are independent: the fields this call
/// leaves null keep whatever was recorded before.
#[tauri::command(async)]
pub async fn cmd_fleet_record_metrics(
    repo_path: String,
    metrics: crate::ledger::FleetMetricsInput,
) -> Result<(), String> {
    off_thread(move || {
        let repo = validate_repo(&repo_path)?;
        crate::ledger::save_fleet_metrics(&repo.to_string_lossy(), &metrics)
            .map_err(|e| e.to_string())
    })
    .await
}

/// MCP 2.0 / Agent Plugins 1.0 installer facts: binary path, plugin manifests, tool catalog.
#[tauri::command]
pub fn cmd_mcp_info() -> crate::insights::McpInfo {
    crate::insights::mcp_info()
}

/// The harness's grant ledger for this repository.
///
/// Read-only, and a file read rather than a protocol op: the serve plane holds
/// no grant ledger, so an op there could only ever answer "none". Revocation is
/// deliberately absent — it mutates state Manvi owns, and a second writer could
/// interleave with the harness's own serialised writes.
#[tauri::command(async)]
pub async fn cmd_grants_view(repo_path: String) -> Result<crate::grants::GrantView, String> {
    off_thread(move || Ok(crate::grants::view(&repo_path))).await
}

/// Discovers the model servers running on this machine.
///
/// Separate from `cmd_ai_status`, which reports the *selected* model. This
/// answers "what could I select", which the AI settings had no way to ask: the
/// harness's capability probe requires a base URL and a model, which is the
/// answer rather than the question.
#[tauri::command(async)]
pub async fn cmd_local_scan() -> Result<crate::harness::ScanResult, String> {
    off_thread(crate::ai::scan_local_servers).await
}

// --- provenance freshness ---------------------------------------------

/// Provenance freshness for one commit.
///
/// A verification note is a claim about a tree at a moment. This says how far
/// the world has moved since — and, when it cannot say, says that instead. The
/// `distance`/`confidence` pair is `null` rather than zero on every failure:
/// zero is the strongest claim the type can make, and handing it out because
/// git could not answer is how a stale badge comes to read as a fresh one.
#[tauri::command(async)]
pub async fn cmd_provenance_freshness(
    repo_path: String,
    commit_sha: String,
    base_branch: Option<String>,
) -> Result<crate::engine::ProvenanceFreshness, String> {
    let repo = validate_repo(&repo_path)?;
    off_thread(move || {
        Ok(crate::engine::provenance::compute_freshness(
            &repo.to_string_lossy(),
            &commit_sha,
            base_branch.as_deref(),
        ))
    })
    .await
}

/// Provenance freshness for many revisions at once, in input order.
///
/// The branch list and the pull-request list each want a badge per row, and a
/// per-row round trip would be a subprocess per branch. The batch resolves
/// every revision in one `git cat-file`, reads each notes ref once, and only
/// then measures — and only the commits that carry a note, because a commit
/// nobody verified has no verification whose age could decay.
///
/// Accepts ref names as well as shas: pull requests arrive as `head_ref`.
#[tauri::command(async)]
pub async fn cmd_provenance_freshness_batch(
    repo_path: String,
    revisions: Vec<String>,
    base_branch: Option<String>,
) -> Result<Vec<crate::engine::ProvenanceFreshness>, String> {
    let repo = validate_repo(&repo_path)?;
    off_thread(move || {
        Ok(crate::engine::provenance::freshness_batch(
            &repo.to_string_lossy(),
            &revisions,
            base_branch.as_deref(),
        ))
    })
    .await
}

#[cfg(test)]
mod assemble_tests {
    use super::*;
    use crate::graph::{MAINLINE_COLOR, MAINLINE_COLUMN};

    fn commit(id: &str, parents: &[&str], author: &str, summary: &str) -> RawCommitNode {
        RawCommitNode {
            id: id.to_string(),
            parent_ids: parents.iter().map(|p| p.to_string()).collect(),
            timestamp: 1,
            author_name: author.to_string(),
            author_email: format!("{}@example.com", author.to_lowercase()),
            summary: summary.to_string(),
        }
    }

    /// main: m0 (merge of feature f1) -> m1 -> m2 -> m3; f1 forks from m2.
    fn history() -> Vec<RawCommitNode> {
        vec![
            commit("m0", &["m1", "f1"], "Ada", "feat: merge feature"),
            commit("f1", &["m2"], "Bob", "fix: feature work"),
            commit("m1", &["m2"], "Ada", "chore: main work"),
            commit("m2", &["m3"], "Bob", "fix: shared base"),
            commit("m3", &[], "Ada", "feat: root"),
        ]
    }

    fn main_ref() -> Vec<RefDecoration> {
        vec![RefDecoration {
            name: "main".to_string(),
            kind: RefKind::Local,
            commit_id: "m0".to_string(),
            is_head: true,
        }]
    }

    fn assemble(query: &str) -> CommitGraphPayload {
        assemble_commit_graph(
            history(),
            10,
            &CommitFilter::parse(query),
            main_ref(),
            Some("main"),
            Some("m0".to_string()),
            Vec::new(),
        )
    }

    fn ids(payload: &CommitGraphPayload) -> Vec<&str> {
        payload.rows.iter().map(|r| r.id.as_str()).collect()
    }

    /// Every stub in a filtered graph must point outside the payload: a
    /// dropped parent is relinked, never left as a fading line.
    fn assert_connected(payload: &CommitGraphPayload) {
        let loaded: HashSet<&str> = payload.rows.iter().map(|r| r.id.as_str()).collect();
        for row in &payload.rows {
            for (k, conn) in row.connections.iter().enumerate() {
                let parent = row.parent_ids.get(k).map(String::as_str).unwrap_or("");
                assert!(
                    !conn.is_dangling || !loaded.contains(parent),
                    "{} -> {parent}: stub points at a loaded row",
                    row.id
                );
            }
        }
        for row in payload.rows.iter().filter(|r| r.is_mainline) {
            assert_eq!(
                row.lane, MAINLINE_COLUMN,
                "{} left the mainline column",
                row.id
            );
            assert_eq!(
                row.color_index, MAINLINE_COLOR,
                "{} left the mainline colour",
                row.id
            );
        }
    }

    #[test]
    fn no_filter_keeps_the_window_and_pins_main() {
        let payload = assemble("");
        assert_eq!(ids(&payload), ["m0", "f1", "m1", "m2", "m3"]);
        assert_eq!(payload.mainline_id.as_deref(), Some("m0"));
        assert_eq!(payload.mainline_name.as_deref(), Some("main"));
        assert!(!payload.has_more);
        assert_connected(&payload);
        let on_rail: Vec<&str> = payload
            .rows
            .iter()
            .filter(|r| r.is_mainline)
            .map(|r| r.id.as_str())
            .collect();
        assert_eq!(on_rail, ["m0", "m1", "m2", "m3"]);
    }

    /// Pre-fix, `retain` left m0 naming f1 and m1 naming m2 — both gone —
    /// so the filtered graph was two stubs and a root.
    #[test]
    fn author_filter_relinks_survivors_and_keeps_main_straight() {
        let payload = assemble("author:ada");
        assert_eq!(ids(&payload), ["m0", "m1", "m3"]);
        assert_connected(&payload);
        let m0 = &payload.rows[0];
        assert_eq!(
            m0.parent_ids,
            ["m1", "m3"],
            "f1's lineage collapses onto m3"
        );
        assert!(m0.connections.iter().all(|c| !c.is_dangling));
        assert_eq!(payload.rows[1].parent_ids, ["m3"]);
        assert!(
            payload.rows.iter().all(|r| r.is_mainline),
            "every survivor is on main"
        );
        assert_eq!(payload.mainline_name.as_deref(), Some("main"));
    }

    /// The filter drops main's tip: the rail re-anchors on the first
    /// survivor of the ORIGINAL chain and keeps the branch's name.
    #[test]
    fn a_filter_that_drops_the_tip_re_anchors_on_the_chain_s_first_survivor() {
        let payload = assemble("author:bob");
        assert_eq!(ids(&payload), ["f1", "m2"]);
        assert_connected(&payload);
        assert_eq!(payload.mainline_id.as_deref(), Some("m2"));
        assert_eq!(payload.mainline_name.as_deref(), Some("main"));
        assert!(payload.rows[1].is_mainline && !payload.rows[0].is_mainline);
        assert_eq!(payload.rows[0].parent_ids, ["m2"]);
        assert!(
            payload.rows[1].parent_ids.is_empty(),
            "m3 dropped: m2 is the filtered root"
        );
        assert!(payload.rows[1].is_root);
    }

    /// Nothing on main survives: the newest survivor's own chain is pinned
    /// and the rail is unnamed rather than mislabelled `main`.
    #[test]
    fn a_filter_with_no_main_survivor_pins_the_newest_survivor_unnamed() {
        let payload = assemble("sha:f1");
        assert_eq!(ids(&payload), ["f1"]);
        assert_eq!(payload.mainline_id.as_deref(), Some("f1"));
        assert_eq!(payload.mainline_name, None);
        assert_connected(&payload);
    }

    /// Free text and conventional types go through the same path.
    #[test]
    fn text_and_type_filters_stay_connected() {
        let payload = assemble("fix:");
        assert_eq!(ids(&payload), ["f1", "m2"]);
        assert_connected(&payload);
        let payload = assemble("root merge");
        assert!(
            payload.rows.is_empty(),
            "conjunction over words matches nothing here"
        );
        let payload = assemble("feat:");
        assert_eq!(ids(&payload), ["m0", "m3"]);
        assert_eq!(
            payload.rows[0].parent_ids,
            ["m3"],
            "m1, m2 and f1 all collapse onto m3"
        );
        assert_connected(&payload);
    }

    /// `path:` is git's job (the walk already rewrote parents); on its own
    /// it must not trigger a second rewrite or move the anchor.
    #[test]
    fn a_path_only_filter_does_not_resimplify() {
        let payload = assemble("path:src");
        assert_eq!(ids(&payload), ["m0", "f1", "m1", "m2", "m3"]);
        assert_eq!(payload.rows[0].parent_ids, ["m1", "f1"]);
        assert_eq!(payload.mainline_name.as_deref(), Some("main"));
    }

    /// The has_more probe row is dropped before filtering, so a filter can
    /// never resurrect the row past the cap.
    #[test]
    fn the_probe_row_is_cut_before_the_filter_runs() {
        let payload = assemble_commit_graph(
            history(),
            4,
            &CommitFilter::parse("author:ada"),
            main_ref(),
            Some("main"),
            None,
            vec!["HEAD unavailable".to_string()],
        );
        assert!(payload.has_more);
        assert_eq!(ids(&payload), ["m0", "m1"]);
        // m3 is past the cap: the relinked edge to it is an honest stub.
        assert_eq!(payload.rows[1].parent_ids, ["m3"]);
        assert!(payload.rows[1].connections[0].is_dangling);
        assert_eq!(payload.warnings, ["HEAD unavailable"]);
        assert_connected(&payload);
    }

    /// A stale persisted repository path is untrusted input. Before this
    /// guard, asking for its ledger status created
    /// `<missing>/.devcouncil/ledger.sqlite`, turning a broken session restore
    /// into an unexpected filesystem mutation outside any repository.
    #[test]
    fn ledger_commands_refuse_a_missing_repo_without_creating_it() {
        let root = tempfile::tempdir().expect("temporary parent");
        let missing = root.path().join("removed-repository");

        let status_result = tauri::async_runtime::block_on(cmd_ledger_status(
            missing.to_string_lossy().into_owned(),
        ));
        let tail_result = tauri::async_runtime::block_on(cmd_ledger_tail(
            missing.to_string_lossy().into_owned(),
            0,
            100,
        ));
        let catch_up_result =
            tauri::async_runtime::block_on(cmd_catch_up(missing.to_string_lossy().into_owned()));
        let snapshot_result = tauri::async_runtime::block_on(cmd_record_pulse_snapshot(
            missing.to_string_lossy().into_owned(),
            crate::ledger::PulseSnapshotInput {
                day: "2026-09-03".to_string(),
                total_commits: 1,
                total_loc: 1,
                bus_factor: 1,
                coverage_pct: None,
                snapshot_json: "{}".to_string(),
            },
        ));
        let snapshots_result = tauri::async_runtime::block_on(cmd_get_pulse_snapshots(
            missing.to_string_lossy().into_owned(),
            Some(10),
        ));

        assert!(status_result.is_err(), "ledger status must reject it");
        assert!(tail_result.is_err(), "ledger tail must reject it");
        assert!(catch_up_result.is_err(), "catch-up must reject it");
        assert!(snapshot_result.is_err(), "snapshot writes must reject it");
        assert!(snapshots_result.is_err(), "snapshot reads must reject it");
        assert!(
            !missing.exists(),
            "a read must not create the missing repository or its ledger"
        );
    }

    /// Opening a linked worktree must address the same durable journal as the
    /// main checkout. Otherwise an append notification and a later reload use
    /// different cursors, making sibling actions either disappear or replay.
    #[test]
    fn ledger_commands_resolve_a_linked_worktree_to_the_family_anchor() {
        let main = tempfile::tempdir().expect("main checkout");
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .args([
                    "-c",
                    "user.name=GitPulse",
                    "-c",
                    "user.email=gitpulse@test.local",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args)
                .current_dir(main.path())
                .output()
                .expect("spawn git");
            assert!(
                output.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&["init", "-b", "main"]);
        std::fs::write(main.path().join("seed.txt"), "seed").expect("seed repository");
        git(&["add", "seed.txt"]);
        git(&["commit", "-m", "seed"]);

        let parent = tempfile::tempdir().expect("worktree parent");
        let linked = parent.path().join("linked");
        git(&[
            "worktree",
            "add",
            "--detach",
            linked.to_str().expect("utf8 worktree"),
        ]);
        let main_path = main.path().canonicalize().expect("main canonical");
        let linked_path = linked.canonicalize().expect("linked canonical");
        crate::ledger::append(crate::ledger::Draft {
            repo_path: main_path.to_string_lossy().into_owned(),
            worktree_path: Some(linked_path.to_string_lossy().into_owned()),
            action: "file.modify".to_string(),
            actor_kind: Some(crate::ledger::ActorKind::Human),
            outcome: Some(crate::ledger::Outcome::Ok),
            ..Default::default()
        })
        .expect("family row");

        let status = tauri::async_runtime::block_on(cmd_ledger_status(
            linked_path.to_string_lossy().into_owned(),
        ))
        .expect("status from linked worktree");
        assert_eq!(
            status.path,
            main_path
                .join(".devcouncil/ledger.sqlite")
                .display()
                .to_string()
        );
        let events = tauri::async_runtime::block_on(cmd_ledger_tail(
            linked_path.to_string_lossy().into_owned(),
            0,
            100,
        ))
        .expect("tail from linked worktree");
        assert_eq!(events.len(), 1, "the shared row must appear exactly once");
        assert_eq!(events[0].repo_path, main_path.to_string_lossy());
        assert_eq!(
            events[0].worktree_path.as_deref(),
            Some(linked_path.to_string_lossy().as_ref())
        );

        tauri::async_runtime::block_on(cmd_record_pulse_snapshot(
            linked_path.to_string_lossy().into_owned(),
            crate::ledger::PulseSnapshotInput {
                day: "2026-09-03".to_string(),
                total_commits: 7,
                total_loc: 42,
                bus_factor: 2,
                coverage_pct: Some(88.5),
                snapshot_json: r#"{"source":"linked"}"#.to_string(),
            },
        ))
        .expect("record snapshot through linked worktree");
        let snapshots = tauri::async_runtime::block_on(cmd_get_pulse_snapshots(
            main_path.to_string_lossy().into_owned(),
            Some(10),
        ))
        .expect("read shared snapshots from main checkout");
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].repo_path, main_path.to_string_lossy());
        assert_eq!(snapshots[0].total_commits, 7);
    }
}
