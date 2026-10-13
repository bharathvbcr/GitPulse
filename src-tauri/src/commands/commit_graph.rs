//! Commit graph assembly: the payload, mainline anchoring, lane solving, stack hierarchy.

use super::*;
use std::path::Path;

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
    /// What this graph deliberately does not draw — see [`GraphNotes`] for why
    /// these travel separately from `warnings`. `default` keeps payloads from
    /// before this field deserializable.
    #[serde(default)]
    pub notices: Vec<String>,
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

/// What a graph load has to say about itself, in the two kinds that must not
/// be confused.
///
/// They were one `Vec<String>` and every entry went to the diagnostics ring —
/// the app's crash log. That put a *true statement about the repository*
/// ("36 commits live in `refs/cmux` and this scope does not draw them") in the
/// same place as a *malfunction*, once per repository per launch, and since
/// the sentence embeds a commit count it minted a fresh entry every time a
/// harness wrote another checkpoint. A bounded fault log filling with correct
/// statements is how the one real failure in it stops being visible.
///
/// The rule for which list an entry belongs in: `warnings` is "GitPulse could
/// not do this", `notices` is "GitPulse did not draw this, and here is how to
/// change that". Only the first is a fault.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct GraphNotes {
    /// A facet that could not be read: a probe that failed, a thread that
    /// died. Belongs in diagnostics — someone has to fix it.
    pub warnings: Vec<String>,
    /// A completeness disclosure about an otherwise-healthy load. Belongs on
    /// screen, next to the graph it is about, never in the fault log.
    pub notices: Vec<String>,
}

/// `git rev-parse HEAD` fails this way when the branch has no commits yet —
/// a brand-new repository, or `git checkout --orphan`. That is a true
/// statement about the repository, not a probe GitPulse failed to run, so it
/// belongs in `notices` rather than the diagnostics ring.
pub(super) fn is_unborn_head_error(err: &str) -> bool {
    let lower = err.to_ascii_lowercase();
    lower.contains("ambiguous argument 'head'")
        || lower.contains("needed a single revision")
        || (lower.contains("unknown revision") && lower.contains("head"))
}

/// Whether a failed HEAD probe means the checked-out branch has no commits.
///
/// Git words an unborn branch and a branch whose ref file is unreadable
/// identically (`ambiguous argument 'HEAD'`), so the error text only says
/// "HEAD did not resolve". The branch is unborn when HEAD names a ref that is
/// absent; if that ref is present and still did not resolve, it is damaged,
/// and calling it "no commits yet" would present a fault as a repository fact.
/// Any probe that cannot answer counts as "not unborn": the caller then
/// records the original error as a warning.
pub(super) fn head_is_unborn(repo: &Path, err: &str) -> bool {
    if !is_unborn_head_error(err) {
        return false;
    }
    let Ok(target) = crate::engine::git_cli::git_text(repo, &["symbolic-ref", "-q", "HEAD"]) else {
        return false;
    };
    let Ok(located) =
        crate::engine::git_cli::git_text(repo, &["rev-parse", "--git-path", target.trim()])
    else {
        return false;
    };
    let located = Path::new(located.trim());
    let located = if located.is_absolute() {
        located.to_path_buf()
    } else {
        repo.join(located)
    };
    !located.exists()
}

pub(super) const UNBORN_HEAD_NOTICE: &str =
    "This repository has no commits yet, so the graph has no HEAD marker.";

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
pub(super) const MAINLINE_NAME_FALLBACKS: [&str; 4] = ["main", "master", "trunk", "develop"];

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
        let remotes: Vec<&RefDecoration> = refs
            .iter()
            .filter(|r| {
                // The owning remote was decided with the longest-prefix rule
                // when the ref was listed (`team/fork` owns `team/fork/main`);
                // the first segment is only the fallback for a decoration
                // that carries none.
                r.kind == RefKind::Remote
                    && match r.remote.as_deref() {
                        Some(remote) => r
                            .name
                            .strip_prefix(remote)
                            .and_then(|rest| rest.strip_prefix('/')),
                        None => r.name.split_once('/').map(|(_, short)| short),
                    } == Some(name)
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
    mut notes: GraphNotes,
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
            let simplified = simplify_history_reporting(&raw_commits, &keep);
            // The parent cap protects the solver's width; say when it cut.
            notes.notices.extend(simplified.notice());
            raw_commits = simplified.commits;
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
        warnings: notes.warnings,
        notices: notes.notices,
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
        // Two lists, not one: see [`GraphNotes`]. `warnings` is what someone
        // has to fix; `notices` is what this graph is honestly not drawing.
        let mut notes = GraphNotes::default();
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
                if head_is_unborn(Path::new(&repo_path), &err) {
                    notes.notices.push(UNBORN_HEAD_NOTICE.into());
                } else {
                    notes.warnings.push(format!(
                        "HEAD unavailable ({err}); commit graph may lack the HEAD marker"
                    ));
                }
                None
            }
            Err(_) => {
                notes.warnings.push(
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
                // missing with nothing to explain it. A notice rather than a
                // warning — the listing succeeded and the cap is ours, so
                // there is nothing here for anyone to fix.
                if let Some(note) = listing.truncation_warning() {
                    notes.notices.push(note);
                }
                listing.decorations
            }
            Ok(Err(err)) => {
                notes.warnings.push(format!(
                    "ref decorations unavailable ({err}); branches/tags will not be labeled"
                ));
                Vec::new()
            }
            Err(_) => {
                notes.warnings.push(
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
                notes.warnings.push(format!(
                    "default branch unresolved ({err}); the straight mainline column is \
                     anchored on a conventional branch name or HEAD"
                ));
                None
            }
            Err(_) => {
                notes.warnings.push(
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
                    notes.notices.push(note);
                }
            }
            Ok(Ok(None)) => {}
            Ok(Err(err)) => notes.warnings.push(format!(
                "hidden-ref probe failed ({err}); refs outside branches, remotes and tags may \
                 hold history this graph does not draw"
            )),
            Err(_) => notes.warnings.push(
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
            notes,
        ))
    })
    .await
}

/// Rows to request from the history walker for one graph page: the user's cap
/// plus one has_more probe row (dropped before lanes are solved).
pub(super) fn graph_fetch_limit(max_commits: usize) -> usize {
    max_commits.clamp(1, GitReader::MAX_HISTORY_COMMITS) + 1
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
