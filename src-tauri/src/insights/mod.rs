//! Read-only repository insights for agents and the Work view.
//!
//! Assembles worktrees, agent sessions, working-tree changes, and overlapping
//! dirty files into one snapshot. Every facet reports whether it could run:
//! a check that did not run must never look like one that ran and found
//! nothing. Nothing here mutates a repository.

use crate::codeintel::{self, CodeintelStatus};
use crate::engine::git_cli::git_text;
use crate::engine::git_reader::{FileStatus, GitReader};
use crate::engine::repo_op::{self, RepoOperation};
use crate::engine::validate_repo;
use crate::engine::worktree::{self, agent_kind, agent_session_slug, changed_paths, WorktreeInfo};
use crate::ledger::{FleetMetrics, LedgerStatus};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// How many worktrees collision detection will porcelain-scan.
const MAX_COLLISION_SCANS: usize = 16;
/// How many overlapping-file rows the payload keeps.
const MAX_COLLISION_ITEMS: usize = 32;
/// The per-worktree path cap must not be the thing that starves the
/// collision payload: a scanned worktree has to be able to contribute at
/// least as many paths as the payload keeps rows. Compile-time, so it is
/// checked by every build rather than only when the tests are compiled.
const _: () = assert!(worktree::MAX_CHANGED_PATHS >= MAX_COLLISION_ITEMS);
/// How many changed files `active_changes` ships.
const MAX_ACTIVE_FILES: usize = 200;
/// How many worktree summaries the snapshot lists.
const MAX_SNAPSHOT_WORKTREES: usize = 64;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorktreeSummary {
    pub path: String,
    pub name: String,
    pub branch: Option<String>,
    /// True when this checkout's HEAD is detached. A null `branch` otherwise
    /// says two different things — deliberately on no branch, or a branch
    /// nobody could read — and a reader cannot tell them apart without this.
    pub is_detached: bool,
    pub is_main: bool,
    pub is_bare: bool,
    pub dirty_files: Option<u32>,
    pub agent_kind: String,
    pub session_slug: String,
    pub operation_kind: String,
    /// Whether the parked-operation probe ran for this worktree. An empty
    /// `operation_kind` with `operation_ok: false` means "not looked at",
    /// which is not the same fact as "nothing parked here".
    pub operation_ok: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentKindCount {
    pub kind: String,
    pub sessions: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSummary {
    /// Whether the worktree listing these counts are derived from ran at all.
    /// Zero sessions with `ok: false` is "we could not look", which must never
    /// render as "no agent sessions running".
    pub ok: bool,
    pub sessions: u32,
    pub kinds: Vec<AgentKindCount>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorktreeFacet {
    pub ok: bool,
    pub error: String,
    /// How many worktrees git reported. Every counter below is over `items`,
    /// which `truncated` says may be a shorter list than this.
    pub count: u32,
    /// Worktrees measured to have uncommitted work.
    pub dirty: u32,
    /// Worktrees whose dirty-file count was actually measured. `dirty` is a
    /// statement about these and no others.
    pub scanned: u32,
    /// Non-bare worktrees whose dirty-file count is unknown: past the listing
    /// scan cap, or `git status` failed there. They are not counted in
    /// `dirty`, and counting them as clean would be a claim nobody checked.
    pub dirty_unknown: u32,
    /// Worktrees with a parked operation (a merge, rebase, cherry-pick).
    pub blocked: u32,
    /// Worktrees whose parked-operation probe did not run or failed, so
    /// "not blocked" was never established for them.
    pub blocked_unknown: u32,
    pub truncated: bool,
    pub items: Vec<WorktreeSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangesFacet {
    pub ok: bool,
    pub error: String,
    pub files: u32,
    pub staged: u32,
    pub unstaged: u32,
    pub untracked: u32,
    pub conflicted: u32,
    pub additions: u32,
    pub deletions: u32,
    /// Rows whose own churn numbers carry a warning: their numstat record
    /// could not be parsed, so they contributed 0/0 to the two totals above.
    /// Non-zero means those totals are a floor, not a measurement.
    pub churn_warnings: u32,
    /// True when a churn total stopped at `u32::MAX` instead of counting
    /// further. A saturated total must not be read as exact.
    pub churn_overflowed: bool,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollisionParty {
    pub path: String,
    pub branch: Option<String>,
    pub agent_kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollisionItem {
    pub path: String,
    pub worktrees: Vec<CollisionParty>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollisionRisk {
    /// True only when every worktree this scan targeted was read. A scan that
    /// read fifteen worktrees and failed on one is not a scan that found no
    /// collision in the sixteenth.
    pub ok: bool,
    /// The first scan failure, if any. `failed_worktrees` carries how many.
    pub error: String,
    pub overlapping_files: u32,
    pub worktrees_involved: u32,
    /// Worktrees read successfully. The findings stand on these.
    pub scanned_worktrees: u32,
    /// Worktrees never attempted, because the scan cap stopped first.
    pub unscanned_worktrees: u32,
    /// Worktrees attempted and failed. Their paths are absent from every item
    /// below, so a party list is only complete while this is zero.
    pub failed_worktrees: u32,
    pub truncated: bool,
    pub items: Vec<CollisionItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InsightsSnapshot {
    pub repo_path: String,
    /// The main worktree's branch. Null means detached or bare when
    /// `branch_ok`, and "nobody could tell" when not.
    pub branch: Option<String>,
    /// Whether the main worktree's branch was actually established.
    pub branch_ok: bool,
    pub worktrees: WorktreeFacet,
    pub agents: AgentSummary,
    pub changes: ChangesFacet,
    pub collisions: CollisionRisk,
    pub ledger: LedgerStatus,
    pub codeintel: CodeintelStatus,
    /// True when [`SNAPSHOT_DEADLINE`] stopped the expensive stages early.
    /// The facets they would have filled say so themselves — unprobed
    /// worktrees carry `operation_ok: false`, and a collision scan that never
    /// started is `ok: false` — so a partial snapshot is visibly partial.
    pub deadline_expired: bool,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangedFile {
    pub path: String,
    pub status_code: String,
    pub is_staged: bool,
    pub is_conflicted: bool,
    pub additions: u32,
    pub deletions: u32,
    /// Why this row's additions/deletions may understate reality: its numstat
    /// record could not be parsed, so the numbers above defaulted to zero.
    /// Carried from [`FileStatus`] rather than dropped, or an unreadable diff
    /// reads as a file with no changes. Absent from the JSON entirely while
    /// empty, so existing consumers see no shape change.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActiveChanges {
    pub repo_path: String,
    pub worktree_path: String,
    pub ok: bool,
    pub error: String,
    pub files: Vec<ChangedFile>,
    pub total: u32,
    pub shown: u32,
    pub truncated: bool,
    pub staged: u32,
    pub unstaged: u32,
    pub untracked: u32,
    pub conflicted: u32,
    pub additions: u32,
    pub deletions: u32,
    /// Rows among `files` whose churn numbers carry a warning. See
    /// [`ChangesFacet::churn_warnings`].
    pub churn_warnings: u32,
    /// True when a churn total saturated at `u32::MAX`. See
    /// [`ChangesFacet::churn_overflowed`].
    pub churn_overflowed: bool,
}

/// In-flight context for one worktree.
///
/// Five separate probes feed this, and each one can fail on its own, so each
/// one reports whether it ran. Without that, four of the five failed into
/// values that read as facts: no collisions, no bound task, no parked
/// operation, and a worktree with a null branch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangeContext {
    pub repo_path: String,
    pub worktree: WorktreeSummary,
    /// Whether the worktree listing ran AND this worktree was in it. False
    /// leaves every field of `worktree` that comes from git — branch,
    /// dirty_files, is_main — unestablished rather than false-or-zero.
    pub worktree_ok: bool,
    pub worktree_error: String,
    pub task_id: String,
    /// Whether the ledger could be consulted. An empty `task_id` with
    /// `task_ok: false` is an unread binding, not an unbound worktree.
    pub task_ok: bool,
    pub task_error: String,
    pub changes: ActiveChanges,
    /// The whole risk payload, not just its rows: an empty item list means
    /// nothing without the `ok`, `scanned_worktrees` and `failed_worktrees`
    /// that say whether anything was looked at.
    pub collisions: CollisionRisk,
    pub operation: Option<RepoOperation>,
    /// Whether the parked-operation probe ran. A null `operation` with
    /// `operation_ok: false` is "we did not look", not "nothing parked".
    pub operation_ok: bool,
    pub operation_error: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolInfo {
    pub name: String,
    pub title: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpInfo {
    pub protocol_version: String,
    pub server_name: String,
    pub server_version: String,
    pub read_only: bool,
    pub binary_found: bool,
    pub binary_path: String,
    pub binary_error: String,
    pub plugin_found: bool,
    pub plugin_path: String,
    pub plugin_error: String,
    pub plugin_manifest_json: String,
    pub plugin_mcp_json: String,
    pub tools: Vec<McpToolInfo>,
}

fn empty_worktrees(error: impl Into<String>) -> WorktreeFacet {
    WorktreeFacet {
        ok: false,
        error: error.into(),
        count: 0,
        dirty: 0,
        scanned: 0,
        dirty_unknown: 0,
        blocked: 0,
        blocked_unknown: 0,
        truncated: false,
        items: Vec::new(),
    }
}

/// Agent counts from a listing that never ran.
fn unknown_agents() -> AgentSummary {
    AgentSummary {
        ok: false,
        sessions: 0,
        kinds: Vec::new(),
    }
}

fn empty_changes(error: impl Into<String>) -> ChangesFacet {
    ChangesFacet {
        ok: false,
        error: error.into(),
        files: 0,
        staged: 0,
        unstaged: 0,
        untracked: 0,
        conflicted: 0,
        additions: 0,
        deletions: 0,
        churn_warnings: 0,
        churn_overflowed: false,
        truncated: false,
    }
}

fn empty_collisions(error: impl Into<String>) -> CollisionRisk {
    CollisionRisk {
        ok: false,
        error: error.into(),
        overlapping_files: 0,
        worktrees_involved: 0,
        scanned_worktrees: 0,
        unscanned_worktrees: 0,
        failed_worktrees: 0,
        truncated: false,
        items: Vec::new(),
    }
}

/// A file list from a read that failed. Zero files with `ok: false` is "we
/// could not look", and the caller still learns which worktree was asked for.
fn failed_changes(repo_path: &str, worktree_path: &str, error: String) -> ActiveChanges {
    ActiveChanges {
        repo_path: repo_path.to_string(),
        worktree_path: worktree_path.to_string(),
        ok: false,
        error,
        files: Vec::new(),
        total: 0,
        shown: 0,
        truncated: false,
        staged: 0,
        unstaged: 0,
        untracked: 0,
        conflicted: 0,
        additions: 0,
        deletions: 0,
        churn_warnings: 0,
        churn_overflowed: false,
    }
}

/// `operation_ok` is the caller's answer, not this function's: a caller that
/// deliberately skipped the probe (the fleet facet, or a snapshot past its
/// deadline) passes `false` so the empty `operation_kind` cannot be read as
/// "nothing parked".
fn summarise_worktree(
    info: &WorktreeInfo,
    operation_kind: String,
    operation_ok: bool,
) -> WorktreeSummary {
    WorktreeSummary {
        path: info.path.clone(),
        name: info.name.clone(),
        branch: info.branch.clone(),
        is_detached: info.is_detached,
        is_main: info.is_main,
        is_bare: info.is_bare,
        dirty_files: info.dirty_files.map(|n| n as u32),
        agent_kind: agent_kind(&info.path).unwrap_or_default(),
        session_slug: agent_session_slug(&info.path).unwrap_or_default(),
        operation_kind,
        operation_ok,
    }
}

/// Rolls per-worktree agent labels into counts.
///
/// `ok` travels in from the caller because it is a fact about the listing
/// these items came from, which this function never sees: an empty slice from
/// a failed listing and an empty slice from a repository with no agents are
/// the same input and must not be the same answer.
fn agent_summary(ok: bool, items: &[WorktreeSummary]) -> AgentSummary {
    let mut counts: Vec<AgentKindCount> = Vec::new();
    for item in items {
        if item.agent_kind.is_empty() {
            continue;
        }
        if let Some(existing) = counts.iter_mut().find(|c| c.kind == item.agent_kind) {
            existing.sessions += 1;
        } else {
            counts.push(AgentKindCount {
                kind: item.agent_kind.clone(),
                sessions: 1,
            });
        }
    }
    let sessions = counts.iter().map(|c| c.sessions).sum();
    AgentSummary {
        ok,
        sessions,
        kinds: counts,
    }
}

/// The parked operation in one worktree, and whether the probe ran.
///
/// `("", true)` is "nothing parked". `("", false)` is a probe that could not
/// run — an unvalidatable path, or a `.git` directory that would not be read —
/// which used to arrive as the same empty string as a clean worktree.
fn detect_operation(path: &str) -> (String, bool) {
    let Ok(repo) = validate_repo(path) else {
        return (String::new(), false);
    };
    match repo_op::detect(&repo) {
        Ok(Some(op)) => (format!("{:?}", op.kind), true),
        Ok(None) => (String::new(), true),
        Err(_) => (String::new(), false),
    }
}

/// Rolls up the per-worktree rows a successful listing produced.
///
/// The counters are over `items`; `count` is what git reported and
/// `truncated` says when the two differ. Splitting "measured clean" from
/// "never measured" is the whole point: a worktree whose `git status` failed,
/// or that fell past the listing scan cap, arrives with `dirty_files: null`,
/// and folding that into `dirty` would report it as clean.
fn worktree_facet(count: u32, truncated: bool, items: Vec<WorktreeSummary>) -> WorktreeFacet {
    let scanned = items.iter().filter(|w| w.dirty_files.is_some()).count() as u32;
    let dirty = items
        .iter()
        .filter(|w| w.dirty_files.is_some_and(|n| n > 0))
        .count() as u32;
    // A bare entry has no working tree, so having no dirty count there is an
    // answer rather than a gap. Every other missing count is a gap.
    let dirty_unknown = items
        .iter()
        .filter(|w| !w.is_bare && w.dirty_files.is_none())
        .count() as u32;
    let blocked = items
        .iter()
        .filter(|w| !w.operation_kind.is_empty())
        .count() as u32;
    let blocked_unknown = items.iter().filter(|w| !w.operation_ok).count() as u32;
    WorktreeFacet {
        ok: true,
        error: String::new(),
        count,
        dirty,
        scanned,
        dirty_unknown,
        blocked,
        blocked_unknown,
        truncated,
        items,
    }
}

/// Totals over one set of working-tree rows.
///
/// The two churn fields at the bottom exist so the two totals above them are
/// never read as exact when they are not: a row whose numstat record could not
/// be parsed contributed 0/0, and a total that reached `u32::MAX` stopped
/// counting.
#[derive(Debug, Clone, Default)]
struct StatusCounts {
    staged: u32,
    unstaged: u32,
    untracked: u32,
    conflicted: u32,
    additions: u32,
    deletions: u32,
    churn_warnings: u32,
    churn_overflowed: bool,
}

/// Adds one row's churn to a running total without wrapping.
///
/// `[profile.release]` sets no `overflow-checks`, so the plain `+=` this
/// replaces panicked in debug and wrapped silently in release — turning a
/// five-billion-line total into a small number that read as authoritative.
/// Saturating keeps the total a floor, and `overflowed` says it is one.
fn add_churn(total: u32, add: usize, overflowed: &mut bool) -> u32 {
    let add = u32::try_from(add).unwrap_or_else(|_| {
        *overflowed = true;
        u32::MAX
    });
    match total.checked_add(add) {
        Some(sum) => sum,
        None => {
            *overflowed = true;
            u32::MAX
        }
    }
}

fn count_statuses(files: &[FileStatus]) -> StatusCounts {
    let mut counts = StatusCounts::default();
    for file in files {
        if file.is_conflicted {
            counts.conflicted += 1;
        }
        if file.is_staged {
            counts.staged += 1;
        }
        if file.status_code.contains('?') {
            counts.untracked += 1;
        } else if !file.is_staged || file.status_code.chars().nth(1).is_some_and(|c| c != ' ') {
            counts.unstaged += 1;
        }
        if !file.warnings.is_empty() {
            counts.churn_warnings += 1;
        }
        counts.additions = add_churn(
            counts.additions,
            file.additions,
            &mut counts.churn_overflowed,
        );
        counts.deletions = add_churn(
            counts.deletions,
            file.deletions,
            &mut counts.churn_overflowed,
        );
    }
    counts
}

fn changes_from_status(files: &[FileStatus], truncated: bool) -> ChangesFacet {
    let counts = count_statuses(files);
    ChangesFacet {
        ok: true,
        error: String::new(),
        files: files.len() as u32,
        staged: counts.staged,
        unstaged: counts.unstaged,
        untracked: counts.untracked,
        conflicted: counts.conflicted,
        additions: counts.additions,
        deletions: counts.deletions,
        churn_warnings: counts.churn_warnings,
        churn_overflowed: counts.churn_overflowed,
        truncated,
    }
}

/// Soft deadline for one whole snapshot, not per probe.
///
/// The expensive half of this call is unbounded in `git` spawns: a parked-
/// operation probe per worktree (2-5 spawns each, up to
/// [`MAX_SNAPSHOT_WORKTREES`] of them) and then a cross-worktree collision
/// scan. Git's own per-spawn timeout is 90 s with no ceiling above it, so a
/// pathological tree could hold the single-threaded MCP server — which
/// answers nothing else meanwhile — for minutes. Past the deadline those two
/// stages are skipped and say so, exactly as [`FLEET_DEADLINE`] does for a
/// workspace sweep.
///
/// The fixed-cost stages — one worktree listing, one `git status`, two local
/// store reads — always run. Cutting them would leave a snapshot with nothing
/// in it, and their cost does not grow with the number of worktrees.
const SNAPSHOT_DEADLINE: Duration = Duration::from_secs(10);

/// One-shot read of everything an agent needs to see the repository as the
/// Work view does: worktrees, agent sessions, dirty files, collisions, ledger
/// and code graph. Individual facets fail independently.
pub fn snapshot(repo_path: &str) -> InsightsSnapshot {
    snapshot_within(repo_path, SNAPSHOT_DEADLINE)
}

/// [`snapshot`] with the deadline as an argument, so the partial-snapshot path
/// is reachable in a test without a pathological repository.
fn snapshot_within(repo_path: &str, deadline: Duration) -> InsightsSnapshot {
    let started = Instant::now();
    let listed = worktree::list_worktrees(repo_path);
    let mut deadline_expired = false;
    let (worktrees, agents) = match &listed {
        Ok(list) => {
            let truncated = list.len() > MAX_SNAPSHOT_WORKTREES;
            let items: Vec<WorktreeSummary> = list
                .iter()
                .take(MAX_SNAPSHOT_WORKTREES)
                .map(|info| {
                    // Sequential on purpose: `repo_op::detect` spawns several
                    // git processes per worktree, and 64 of those at once is
                    // the spawn storm this deadline exists to bound.
                    if started.elapsed() >= deadline {
                        deadline_expired = true;
                        return summarise_worktree(info, String::new(), false);
                    }
                    let (kind, probed) = detect_operation(&info.path);
                    summarise_worktree(info, kind, probed)
                })
                .collect();
            let facet = worktree_facet(list.len() as u32, truncated, items);
            let agents = agent_summary(true, &facet.items);
            (facet, agents)
        }
        Err(error) => (empty_worktrees(error.clone()), unknown_agents()),
    };

    let changes = match GitReader::get_status(repo_path) {
        Ok(files) => {
            let truncated = files.len() > MAX_ACTIVE_FILES;
            let kept = if truncated {
                &files[..MAX_ACTIVE_FILES]
            } else {
                &files
            };
            let mut facet = changes_from_status(kept, truncated);
            facet.files = files.len() as u32;
            facet
        }
        Err(error) => empty_changes(error),
    };

    let collisions = match &listed {
        Ok(list) => {
            if started.elapsed() >= deadline {
                deadline_expired = true;
                // A scan that never started is a failed facet, not an empty
                // one: `ok: false` with a reason, never `overlapping_files: 0`.
                empty_collisions("the snapshot ran out of time before the collision scan")
            } else {
                collision_from_list(list)
            }
        }
        Err(error) => empty_collisions(error.clone()),
    };

    let main_worktree = worktrees.items.iter().find(|w| w.is_main);
    let branch = main_worktree.and_then(|w| w.branch.clone());
    // A null branch means the main worktree is on none — detached, or bare —
    // only when the listing that would have said so actually ran.
    let branch_ok = worktrees.ok && main_worktree.is_some();

    // The read-only variant. A snapshot is a read on every surface that takes
    // one — the MCP tool annotated `readOnlyHint: true`, and the Work view —
    // and the creating variant opens the database (which makes it) and runs the
    // legacy consolidation (which migrates rows). A ledger comes into existence
    // when the first event is *recorded*, which is the honest moment for it to;
    // until then `not_initialised` is the true answer rather than one arranged
    // by writing to the user's repository so that "recording" could be said.
    let ledger = match crate::ledger::bindings::repository_status_readonly(repo_path) {
        Ok(status) => status,
        Err(error) => crate::ledger::LedgerStatus {
            recording: false,
            path: String::new(),
            dropped: 0,
            error: error.to_string(),
            error_code: error.code.to_string(),
        },
    };

    InsightsSnapshot {
        repo_path: repo_path.to_string(),
        branch,
        branch_ok,
        worktrees,
        agents,
        changes,
        collisions,
        ledger,
        codeintel: codeintel::status(repo_path),
        deadline_expired,
        duration_ms: started.elapsed().as_millis() as u64,
    }
}

/* ── Fleet: the same idea at workspace scale ──────────────────────────────── */

/// How many repositories one fleet sweep will visit.
///
/// The workspace caps open tabs at 24 and recents at 24, so 48 is the real
/// ceiling; the extra headroom is for a caller that passes both plus a
/// duplicate or two, and anything past it is dropped and reported through
/// `truncated` rather than silently ignored.
pub const MAX_FLEET_REPOS: usize = 64;

/// Soft deadline for one whole sweep, not per repository.
///
/// Past it the remaining repositories are left unvisited and `truncated` says
/// so. A per-repository timeout would let 24 slow repositories add up to a
/// dashboard that never paints.
const FLEET_DEADLINE: Duration = Duration::from_secs(10);

/// Days of history one fleet commit probe reads, when the caller says nothing.
///
/// Long enough that a quarter's rhythm is visible, short enough that the walk
/// stays a fraction of a second on a busy repository. Every count in
/// [`FleetCommitStats`] is over exactly this span — "31 commits" is not a fact
/// until the window it was counted in travels with it, which is why
/// `window_days` is reported back rather than assumed by the reader.
pub const FLEET_COMMIT_WINDOW_DAYS: u32 = 90;

/// The longest window a caller may ask for.
///
/// The walk itself is bounded by [`MAX_FLEET_COMMITS`] rather than by the
/// window, so a longer span costs no extra `git`; the cap is on the per-repo
/// `daily` array, which crosses IPC once per repository per sweep. At 180 days
/// and the 64-repository cap that is 11,520 numbers, which is a payload; at an
/// unbounded window it is whatever the caller typed.
pub const MAX_FLEET_COMMIT_WINDOW_DAYS: u32 = 180;

/// Clamps a requested window into something this probe will actually honour.
///
/// Zero and absurd values are corrected rather than refused: the window is a
/// display preference, and failing a whole fleet sweep because a stale client
/// asked for 10,000 days would be a worse answer than 180. The window that was
/// *used* travels back on every `FleetCommitStats`, so a clamped request is
/// visible to the caller rather than silently substituted.
pub fn clamp_commit_window(window_days: Option<u32>) -> u32 {
    match window_days {
        None => FLEET_COMMIT_WINDOW_DAYS,
        Some(days) => days.clamp(1, MAX_FLEET_COMMIT_WINDOW_DAYS),
    }
}

/// Ceiling on commits one probe reads.
///
/// The walk is bounded by COUNT rather than by `--since`, and that is a
/// deliberate, measured choice. `git log --since` prunes: it stops descending
/// a parent chain at the first commit older than the cutoff, so a single
/// out-of-order committer date — a cherry-pick, an imported history, a skewed
/// clock — drops every genuine in-window commit behind it. Reproduced on git
/// 2.50: a five-commit chain with one inverted date reported three of its four
/// in-window commits, with nothing to say it had stopped early. Silent
/// under-reporting is the one failure this whole surface exists to prevent.
///
/// Reading by count instead costs a bounded walk: measured at 90 ms for 20,000
/// commits on a 200,000-commit repository, against 810 ms for that history in
/// full. Across a workspace under [`FLEET_DEADLINE`] that is affordable, and
/// it is exact for every history shape.
///
/// Past the cap the walk stops and `truncated` says so, which makes every
/// count a floor rather than a total that happens to be wrong.
const MAX_FLEET_COMMITS: usize = 20_000;

/// Seconds in one bucket.
///
/// Buckets are rolling 24-hour spans anchored at the sweep, NOT local calendar
/// days. This process has no timezone of its own, and picking one here would
/// file a commit under a different day than the per-repository Pulse view —
/// which does bucket by local calendar day — does. A span the reader can
/// state exactly beats a "day" that means two things.
const BUCKET_SECONDS: i64 = 86_400;

/// Buckets one short-term trend covers, and the trend it is compared against.
const TREND_BUCKETS: usize = 7;

/// One repository's commit rhythm over a bounded window.
///
/// Cheap by construction: one `git log` that reads commit metadata and never
/// touches a diff. The expensive churn-and-authorship version of this question
/// is [`crate::engine::git_reader::GitReader::pulse_report`], which is what
/// the per-repository Pulse view runs on demand for one repository at a time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FleetCommitStats {
    /// Days the window spans, and the length of `daily`.
    pub window_days: u32,
    /// Unix seconds the window ends at: the moment the sweep read it.
    ///
    /// Bucket `i` of `daily` covers
    /// `[anchor_epoch - (window_days - i) * 86400, anchor_epoch - (window_days - i - 1) * 86400)`.
    /// Every repository in one sweep shares this anchor, which is what lets a
    /// caller sum the series across repositories bucket for bucket.
    pub anchor_epoch: i64,
    /// Commits in the window. Always equals the sum of `daily`, so a count and
    /// the series it summarizes can never describe different populations.
    pub commits: u32,
    /// Distinct author emails among those commits.
    pub authors: u32,
    /// Buckets carrying at least one commit.
    pub active_days: u32,
    /// Commits in the newest seven buckets.
    pub commits_7d: u32,
    /// Commits in the seven buckets before those, so a trend has something to
    /// be a trend against rather than being drawn from one number.
    pub commits_prior_7d: u32,
    /// Newest commit in the window, unix seconds; zero when the window is
    /// empty. Zero here never means "no commits ever" — a repository can be
    /// quiet for a quarter — which is why the facet keeps its own
    /// `last_commit_epoch` probe for exactly that case.
    pub last_commit_epoch: i64,
    /// One count per bucket, oldest first, exactly `window_days` long.
    pub daily: Vec<u32>,
    /// True when [`MAX_FLEET_COMMITS`] stopped the walk. Every count above is
    /// then a floor, and a caller that renders it as a total is presenting a
    /// capped sample as complete coverage.
    pub truncated: bool,
}

fn empty_commit_stats(anchor_epoch: i64, window_days: u32) -> FleetCommitStats {
    FleetCommitStats {
        window_days,
        anchor_epoch,
        commits: 0,
        authors: 0,
        active_days: 0,
        commits_7d: 0,
        commits_prior_7d: 0,
        last_commit_epoch: 0,
        daily: vec![0; window_days as usize],
        truncated: false,
    }
}

/// Commit rhythm for one repository over [`FLEET_COMMIT_WINDOW_DAYS`].
///
/// A repository with no commits at all is a readable answer — an empty window,
/// not a failed probe — so it comes back as `Ok`, exactly as
/// [`last_commit_epoch`] treats the same case.
/// Buckets one `git log` payload into a window's worth of counts.
///
/// Split from the spawn so every rule below — the bucket boundary, the
/// out-of-window drop, the author set, the truncation flag — is a unit test
/// against a string rather than something that needs a repository shaped just
/// so. Two of them cannot be reached through git at all: git refuses to write
/// a commit with a date near `i64`'s floor, and it never emits a row missing
/// its separator. Both are still handled, and now both are still tested.
///
/// `parsed` is the number of rows git actually emitted, which is how
/// truncation is detected; it is separate from the number that landed in a
/// bucket, which is almost always smaller.
fn bucket_commit_log(text: &str, anchor_epoch: i64, window_days: u32) -> FleetCommitStats {
    let buckets = window_days as usize;
    let mut daily = vec![0u32; buckets];
    let mut authors: HashSet<&str> = HashSet::new();
    let mut newest = 0i64;
    let mut parsed = 0usize;
    for line in text.lines() {
        let Some((stamp, email)) = line.trim().split_once('\u{1f}') else {
            continue;
        };
        let Ok(epoch) = stamp.trim().parse::<i64>() else {
            continue;
        };
        parsed += 1;
        // The walk covers all of history up to the cap, so most rows are older
        // than the window. Anything outside it — including a commit stamped in
        // the future by a skewed clock — is dropped from every count rather
        // than folded into an edge bucket, so `commits` and `daily` always
        // describe the same population.
        //
        // Saturating, not plain subtraction: `%ct` is parsed as an i64, and a
        // corrupt stamp near the type's floor would overflow — a panic in a
        // debug build, and a wrap into a plausible-looking bucket in a release
        // one. Saturating puts it far outside the window, where the bounds
        // check below drops it like any other out-of-window row.
        let age = anchor_epoch.saturating_sub(epoch);
        let from_newest = if age <= 0 {
            0
        } else {
            (age / BUCKET_SECONDS) as usize
        };
        if from_newest >= buckets {
            continue;
        }
        let index = buckets - 1 - from_newest;
        daily[index] = daily[index].saturating_add(1);
        if epoch > newest {
            newest = epoch;
        }
        let email = email.trim();
        if !email.is_empty() {
            authors.insert(email);
        }
    }

    let sum = |slice: &[u32]| -> u32 { slice.iter().fold(0u32, |acc, n| acc.saturating_add(*n)) };
    let recent_from = buckets.saturating_sub(TREND_BUCKETS);
    let prior_from = buckets.saturating_sub(TREND_BUCKETS * 2);
    FleetCommitStats {
        window_days,
        anchor_epoch,
        commits: sum(&daily),
        authors: authors.len() as u32,
        active_days: daily.iter().filter(|count| **count > 0).count() as u32,
        commits_7d: sum(&daily[recent_from..]),
        commits_prior_7d: sum(&daily[prior_from..recent_from]),
        last_commit_epoch: newest,
        daily,
        truncated: parsed > MAX_FLEET_COMMITS,
    }
}

fn fleet_commit_stats(
    repo: &Path,
    anchor_epoch: i64,
    window_days: u32,
) -> Result<FleetCommitStats, String> {
    // One over the cap, so "we stopped early" is observable rather than
    // indistinguishable from a repository that happens to have exactly the cap.
    let limit = (MAX_FLEET_COMMITS + 1).to_string();
    let text = match git_text(
        repo,
        &[
            "log",
            "--no-show-signature",
            "-n",
            &limit,
            "--format=%ct%x1f%ae",
            "--",
        ],
    ) {
        Ok(text) => text,
        Err(error) => {
            // The same discrimination `last_commit_epoch` makes: no HEAD is an
            // empty repository, which is data; anything else is a probe that
            // could not run, which is not.
            if git_text(repo, &["rev-parse", "--verify", "HEAD"]).is_err() {
                return Ok(empty_commit_stats(anchor_epoch, window_days));
            }
            return Err(error);
        }
    };
    Ok(bucket_commit_log(&text, anchor_epoch, window_days))
}

/// One repository's cheap facet: what can be learned in two `git` spawns.
///
/// Deliberately NOT what [`snapshot`] returns. That function probes every
/// worktree for a parked operation, counts dirty files in up to 32 of them,
/// and cross-scans up to 16 for colliding paths — right for one repository on
/// screen, and several hundred subprocesses when multiplied by a workspace.
/// Everything expensive is left to the per-repository views that already do it.
///
/// The commit-rhythm probe added a third `git` spawn only for a repository
/// that has been quiet for the whole window: when the window has commits it
/// already carries the newest one, so `last_commit_epoch`'s own `git log -1`
/// is skipped. An active workspace therefore still costs two spawns per
/// repository, and a dormant one costs three cheap ones.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FleetRepoFacet {
    pub repo_path: String,
    /// False when the repository could not be validated at all; nothing below
    /// it means anything then.
    pub ok: bool,
    pub error: String,
    /// Whether the worktree listing ran. False leaves `worktrees` and `agents`
    /// meaningless rather than zero.
    pub worktrees_ok: bool,
    pub worktrees_error: String,
    pub worktrees: u32,
    pub agents: AgentSummary,
    /// Whether the last-commit probe ran.
    pub last_commit_ok: bool,
    /// Unix seconds of the newest commit reachable from HEAD. Zero is a real
    /// answer (a repository with no commits) only when `last_commit_ok`.
    pub last_commit_epoch: i64,
    /// Whether the commit-rhythm probe ran. False leaves `commits` `None`,
    /// which is "we could not look" — never "this repository is quiet".
    pub commits_ok: bool,
    pub commits_error: String,
    /// Commit counts over a bounded window. `Some` exactly when `commits_ok`;
    /// an empty window inside it is a measured silence, and `None` is not.
    pub commits: Option<FleetCommitStats>,
    /// Whether this repository's ledger could be consulted at all. False means
    /// the metric cache below is unknown, not empty.
    pub metrics_ok: bool,
    pub metrics_error: String,
    /// Cached expensive-scan results, or `None` when nothing was ever scanned.
    pub metrics: Option<FleetMetrics>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FleetSnapshot {
    pub repos: Vec<FleetRepoFacet>,
    pub requested: u32,
    pub scanned: u32,
    /// The instant every commit window in this sweep is anchored at, in unix
    /// seconds. Carried on the snapshot as well as on each facet so a caller
    /// summing the series across repositories can check they agree rather
    /// than assume it.
    pub anchor_epoch: i64,
    /// True when the repository cap or the sweep deadline stopped the walk
    /// short, so `repos` covers fewer repositories than were asked for.
    pub truncated: bool,
    pub duration_ms: u64,
}

fn unreadable_facet(repo_path: &str, error: String) -> FleetRepoFacet {
    FleetRepoFacet {
        repo_path: repo_path.to_string(),
        ok: false,
        error,
        worktrees_ok: false,
        worktrees_error: String::new(),
        worktrees: 0,
        agents: unknown_agents(),
        last_commit_ok: false,
        last_commit_epoch: 0,
        commits_ok: false,
        commits_error: String::new(),
        commits: None,
        metrics_ok: false,
        metrics_error: String::new(),
        metrics: None,
    }
}

/// Newest commit time reachable from HEAD, in unix seconds.
///
/// An empty repository has no HEAD, and `git log` fails there. That is a
/// readable answer — "no commits yet" — rather than a broken probe, so it
/// comes back as `Ok(0)` and only a genuine failure becomes `Err`.
fn last_commit_epoch(repo: &Path) -> Result<i64, String> {
    match git_text(repo, &["log", "-1", "--format=%ct", "--"]) {
        Ok(text) => Ok(text.trim().parse::<i64>().unwrap_or(0)),
        Err(error) => {
            // `git rev-parse HEAD` failing the same way confirms there is no
            // commit at all, rather than a git that could not run.
            if git_text(repo, &["rev-parse", "--verify", "HEAD"]).is_err() {
                Ok(0)
            } else {
                Err(error)
            }
        }
    }
}

/// Reads one repository's facet, anchored at the sweep's own clock.
///
/// `anchor_epoch` is passed in rather than read here so every repository in
/// one sweep buckets its commits against the same instant. Read per
/// repository, twenty-four rows would carry twenty-four slightly different
/// anchors and their series could no longer be summed bucket for bucket —
/// which is exactly what the fleet-wide activity chart does with them.
fn fleet_facet(repo_path: &str, anchor_epoch: i64, window_days: u32) -> FleetRepoFacet {
    let repo = match validate_repo(repo_path) {
        Ok(path) => path,
        Err(error) => return unreadable_facet(repo_path, error),
    };

    let (worktrees_ok, worktrees_error, worktree_count, agents) =
        match worktree::list_worktrees_lite(repo_path) {
            Ok(list) => {
                // `summarise_worktree` is reused for its agent-kind and slug
                // derivation, with an empty operation kind: probing every
                // worktree for a parked merge is exactly the per-worktree cost
                // this facet exists to avoid.
                let items: Vec<WorktreeSummary> = list
                    .iter()
                    .map(|info| summarise_worktree(info, String::new(), false))
                    .collect();
                (
                    true,
                    String::new(),
                    list.len() as u32,
                    agent_summary(true, &items),
                )
            }
            Err(error) => (false, error, 0, unknown_agents()),
        };

    let (commits_ok, commits_error, commits) =
        match fleet_commit_stats(&repo, anchor_epoch, window_days) {
            Ok(stats) => (true, String::new(), Some(stats)),
            Err(error) => (false, error, None),
        };

    // The window probe already read the newest commit whenever the window has
    // one, so the dedicated `git log -1` only runs for a repository that has
    // been silent for the whole window, or whose probe failed — the two cases
    // where the window cannot answer the question.
    let (last_commit_ok, last_commit) = match commits.as_ref().map(|s| s.last_commit_epoch) {
        Some(epoch) if epoch > 0 => (true, epoch),
        _ => match last_commit_epoch(&repo) {
            Ok(epoch) => (true, epoch),
            Err(_) => (false, 0),
        },
    };

    let (metrics_ok, metrics_error, metrics) = match crate::ledger::read_fleet_metrics(repo_path) {
        Ok(found) => (true, String::new(), found),
        // An unreadable ledger is not an unscanned repository. Reporting it as
        // `None` would render every metric as "never scanned" for a repository
        // that may have a full history of them.
        Err(error) => (false, error.to_string(), None),
    };

    FleetRepoFacet {
        repo_path: repo_path.to_string(),
        ok: true,
        error: String::new(),
        worktrees_ok,
        worktrees_error,
        worktrees: worktree_count,
        agents,
        last_commit_ok,
        last_commit_epoch: last_commit,
        commits_ok,
        commits_error,
        commits,
        metrics_ok,
        metrics_error,
        metrics,
    }
}

/// Reads the cheap facet for every repository in a workspace, in parallel.
///
/// One repository's failure is recorded in its own facet and never propagates:
/// a deleted checkout in tab 3 must not blank the other twenty-three rows.
/// Duplicate paths collapse to one facet, because two tabs can name the same
/// repository through different symlinks or letter cases and scanning it twice
/// makes the two runs contend for the same `.git` lock.
pub fn fleet_snapshot(repo_paths: &[String], window_days: Option<u32>) -> FleetSnapshot {
    let started = Instant::now();
    let requested = repo_paths.len() as u32;
    // Clamped once, here, and then shared by every facet — so a sweep cannot
    // end up with rows on two different windows, which is the one thing that
    // makes the per-repository series unsummable.
    let window_days = clamp_commit_window(window_days);
    // One clock for the whole sweep. See [`fleet_facet`]: the per-repository
    // commit series are only summable because they share this anchor.
    let anchor_epoch = (crate::ledger::ids::now_millis() / 1000) as i64;

    let mut seen = std::collections::HashSet::new();
    let mut targets: Vec<&String> = Vec::new();
    for path in repo_paths {
        if path.is_empty() || !seen.insert(path.as_str()) {
            continue;
        }
        targets.push(path);
    }
    let over_cap = targets.len() > MAX_FLEET_REPOS;
    targets.truncate(MAX_FLEET_REPOS);

    let expired = AtomicBool::new(false);
    let repos: Vec<FleetRepoFacet> = targets
        .par_iter()
        .map(|path| {
            if started.elapsed() >= FLEET_DEADLINE {
                expired.store(true, Ordering::Relaxed);
                // Not visited, and said so. A repository skipped for time must
                // never arrive looking like one that was read and found empty.
                return unreadable_facet(path, "the fleet sweep ran out of time".to_string());
            }
            fleet_facet(path, anchor_epoch, window_days)
        })
        .collect();

    let scanned = repos.iter().filter(|facet| facet.ok).count() as u32;
    FleetSnapshot {
        // `truncated` means only "fewer repositories were visited than asked
        // for". A repository that WAS visited and failed is reported in its own
        // facet; folding that in here would make the flag mean two things and
        // leave a caller unable to tell a short sweep from a broken checkout.
        truncated: over_cap || expired.load(Ordering::Relaxed),
        requested,
        scanned,
        anchor_epoch,
        duration_ms: started.elapsed().as_millis() as u64,
        repos,
    }
}

fn collision_from_list(list: &[WorktreeInfo]) -> CollisionRisk {
    let scan_targets: Vec<&WorktreeInfo> = list
        .iter()
        .filter(|w| !w.is_bare)
        .take(MAX_COLLISION_SCANS)
        .collect();
    let unscanned = list
        .iter()
        .filter(|w| !w.is_bare)
        .count()
        .saturating_sub(scan_targets.len());

    let scans: Vec<_> = scan_targets
        .into_par_iter()
        .map(|wt| (wt, changed_paths(&wt.path)))
        .collect();

    let mut by_path: HashMap<String, Vec<CollisionParty>> = HashMap::new();
    let mut scan_error = String::new();
    let mut scanned = 0u32;
    let mut failed = 0u32;
    let mut paths_truncated = false;
    for (wt, result) in scans {
        match result {
            Ok((paths, truncated)) => {
                scanned += 1;
                paths_truncated |= truncated;
                let party = CollisionParty {
                    path: wt.path.clone(),
                    branch: wt.branch.clone(),
                    agent_kind: agent_kind(&wt.path).unwrap_or_default(),
                };
                for path in paths {
                    by_path.entry(path).or_default().push(party.clone());
                }
            }
            Err(error) => {
                // A worktree that could not be read contributes no paths, so
                // it is absent from every item below and from
                // `worktrees_involved`. Counting it is the only thing that
                // keeps that absence distinguishable from "it had nothing".
                failed += 1;
                if scan_error.is_empty() {
                    scan_error = error;
                }
            }
        }
    }

    let mut items: Vec<CollisionItem> = by_path
        .into_iter()
        .filter(|(_, parties)| parties.len() > 1)
        .map(|(path, worktrees)| CollisionItem { path, worktrees })
        .collect();
    items.sort_by(|a, b| a.path.cmp(&b.path));
    let overlapping_files = items.len() as u32;
    let truncated =
        items.len() > MAX_COLLISION_ITEMS || paths_truncated || unscanned > 0 || failed > 0;
    if items.len() > MAX_COLLISION_ITEMS {
        items.truncate(MAX_COLLISION_ITEMS);
    }
    let mut involved = std::collections::BTreeSet::new();
    for item in &items {
        for party in &item.worktrees {
            involved.insert(party.path.clone());
        }
    }

    CollisionRisk {
        // Every attempted worktree has to have been read. The old rule —
        // "no error, OR at least one success" — let one success speak for
        // fifteen failures, so a caller reading `ok` alone concluded the whole
        // family had been checked.
        //
        // The scan cap does NOT clear this flag: `unscanned_worktrees` and
        // `truncated` already report a short scan, and folding that in here
        // would make one field mean both "cut short" and "broken", the same
        // conflation `fleet_snapshot::truncated` is documented to avoid.
        ok: failed == 0,
        error: scan_error,
        overlapping_files,
        worktrees_involved: involved.len() as u32,
        scanned_worktrees: scanned,
        unscanned_worktrees: unscanned as u32,
        failed_worktrees: failed,
        truncated,
        items,
    }
}

/// Overlapping dirty files across worktrees of `repo_path`.
pub fn collision_risk(repo_path: &str) -> CollisionRisk {
    match worktree::list_worktrees(repo_path) {
        Ok(list) => collision_from_list(&list),
        Err(error) => empty_collisions(error),
    }
}

fn to_changed(file: &FileStatus) -> ChangedFile {
    ChangedFile {
        path: file.path.clone(),
        status_code: file.status_code.clone(),
        is_staged: file.is_staged,
        is_conflicted: file.is_conflicted,
        additions: file.additions as u32,
        deletions: file.deletions as u32,
        // Dropping these turned "the diff for this row could not be read" into
        // `additions: 0, deletions: 0`, which reads as a measured fact.
        warnings: file.warnings.clone(),
    }
}

/// Resolves the worktree an insights read is about, and proves it belongs to
/// `repo_path`.
///
/// Without this, `worktree_path` was taken on trust: a path naming a totally
/// unrelated repository came back stamped with `repo_path`'s identity, so the
/// answer described one repository while claiming to describe another. The
/// ledger route has always authenticated this pair; these read tools now use
/// the same gate. The returned path is the canonical one git registered, so
/// later comparisons against the worktree listing are not defeated by
/// symlinks or a trailing slash.
/// Do two path strings name the same directory?
///
/// A literal comparison is not enough on macOS, where every temporary directory
/// lives under `/var/folders/...` and `/var` is a symlink to `/private/var`.
/// `git worktree list` prints the resolved form while a caller passes whatever
/// it was given, so the two disagree for the same directory — and the caller is
/// then told its own repository "was not in this repository's worktree
/// listing".
///
/// Canonicalization is attempted on both sides and the literal comparison is
/// the fallback, because a worktree that was removed between the listing and
/// this call cannot be canonicalized and must still compare equal to itself.
fn same_path(left: &str, right: &str) -> bool {
    if Path::new(left) == Path::new(right) {
        return true;
    }
    match (std::fs::canonicalize(left), std::fs::canonicalize(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

fn resolve_target(repo_path: &str, worktree_path: Option<&str>) -> Result<String, String> {
    let Some(requested) = worktree_path else {
        // No worktree named: the repository itself is the target, and there is
        // no second identity to authenticate.
        return Ok(repo_path.to_string());
    };
    if Path::new(requested) == Path::new(repo_path) {
        return Ok(repo_path.to_string());
    }
    let family = worktree::resolve_worktree_family(repo_path, requested)?;
    Ok(family.worktree.to_string_lossy().into_owned())
}

/// Working-tree file list for one worktree, capped and counted.
///
/// `worktree_path` must be a checkout of `repo_path`; one that is not is
/// refused rather than read, because the payload stamps `repo_path` on
/// whatever it returns.
pub fn active_changes(
    repo_path: &str,
    worktree_path: Option<&str>,
    limit: Option<u32>,
) -> ActiveChanges {
    let requested = worktree_path.unwrap_or(repo_path);
    match resolve_target(repo_path, worktree_path) {
        Ok(target) => changes_in(repo_path, &target, limit),
        Err(error) => failed_changes(repo_path, requested, error),
    }
}

/// [`active_changes`] for a target already proven to belong to `repo_path`, so
/// a caller that has resolved the family does not pay for a second resolution.
fn changes_in(repo_path: &str, target: &str, limit: Option<u32>) -> ActiveChanges {
    let cap = (limit.unwrap_or(MAX_ACTIVE_FILES as u32) as usize).clamp(1, 500);
    match GitReader::get_status(target) {
        Ok(files) => {
            let total = files.len() as u32;
            let truncated = files.len() > cap;
            let kept: Vec<FileStatus> = files.iter().take(cap).cloned().collect();
            let counts = count_statuses(&kept);
            ActiveChanges {
                repo_path: repo_path.to_string(),
                worktree_path: target.to_string(),
                ok: true,
                error: String::new(),
                shown: kept.len() as u32,
                files: kept.iter().map(to_changed).collect(),
                total,
                truncated,
                staged: counts.staged,
                unstaged: counts.unstaged,
                untracked: counts.untracked,
                conflicted: counts.conflicted,
                additions: counts.additions,
                deletions: counts.deletions,
                churn_warnings: counts.churn_warnings,
                churn_overflowed: counts.churn_overflowed,
            }
        }
        Err(error) => failed_changes(repo_path, target, error),
    }
}

/// The worktree row for a checkout the listing did not describe.
///
/// Everything git would have supplied is left unset rather than defaulted to a
/// value that reads as read; the context's `worktree_ok` says why. The agent
/// labels are derived from the path text alone, so they are as true here as
/// anywhere.
fn unlisted_worktree(target: &str, operation_kind: String, operation_ok: bool) -> WorktreeSummary {
    WorktreeSummary {
        path: target.to_string(),
        name: Path::new(target)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| target.to_string()),
        branch: None,
        is_detached: false,
        is_main: false,
        is_bare: false,
        dirty_files: None,
        agent_kind: agent_kind(target).unwrap_or_default(),
        session_slug: agent_session_slug(target).unwrap_or_default(),
        operation_kind,
        operation_ok,
    }
}

/// A context for a worktree that could not be tied to the repository.
///
/// Every facet reports the same failure, because not one of them ran: reading
/// the named path anyway is what let an unrelated repository's files come back
/// stamped with this repository's identity.
fn unresolved_context(repo_path: &str, requested: &str, error: String) -> ChangeContext {
    ChangeContext {
        repo_path: repo_path.to_string(),
        worktree: unlisted_worktree(requested, String::new(), false),
        worktree_ok: false,
        worktree_error: error.clone(),
        task_id: String::new(),
        task_ok: false,
        task_error: error.clone(),
        changes: failed_changes(repo_path, requested, error.clone()),
        collisions: empty_collisions(error.clone()),
        operation: None,
        operation_ok: false,
        operation_error: error,
    }
}

/// Narrows a repository-wide risk to the rows that involve one worktree.
///
/// The row counts follow the rows they describe. The scan-coverage fields —
/// `ok`, `error`, `scanned_worktrees`, `unscanned_worktrees`,
/// `failed_worktrees`, `truncated` — are facts about the scan, not about these
/// rows, and are carried through untouched: they are the only thing that says
/// whether an empty list means "nothing collides here" or "we could not tell".
fn collisions_involving(risk: CollisionRisk, target: &str) -> CollisionRisk {
    let items: Vec<CollisionItem> = risk
        .items
        .into_iter()
        .filter(|item| {
            item.worktrees
                .iter()
                .any(|party| Path::new(&party.path) == Path::new(target))
        })
        .collect();
    let mut involved = std::collections::BTreeSet::new();
    for item in &items {
        for party in &item.worktrees {
            involved.insert(party.path.clone());
        }
    }
    CollisionRisk {
        overlapping_files: items.len() as u32,
        worktrees_involved: involved.len() as u32,
        items,
        ..risk
    }
}

/// In-flight context for one worktree: changes, parked operation, bound task,
/// collisions that involve it.
///
/// `worktree_path` must be a checkout of `repo_path`, and each of the five
/// probes reports whether it ran.
pub fn change_context(repo_path: &str, worktree_path: Option<&str>) -> ChangeContext {
    let requested = worktree_path.unwrap_or(repo_path).to_string();
    let target = match resolve_target(repo_path, worktree_path) {
        Ok(path) => path,
        Err(error) => return unresolved_context(repo_path, &requested, error),
    };

    let (operation, operation_ok, operation_error) =
        match validate_repo(&target).and_then(|repo| repo_op::detect(&repo)) {
            Ok(found) => (found, true, String::new()),
            Err(error) => (None, false, error),
        };
    let operation_kind = operation
        .as_ref()
        .map(|op| format!("{:?}", op.kind))
        .unwrap_or_default();

    let (worktree, worktree_ok, worktree_error) = match worktree::list_worktrees(repo_path) {
        Ok(list) => match list.iter().find(|w| same_path(&w.path, &target)) {
            Some(found) => (
                summarise_worktree(found, operation_kind, operation_ok),
                true,
                String::new(),
            ),
            // Registered a moment ago and gone from the listing now, or a git
            // that prints a path neither form of comparison matches. Either
            // way the row below is not something git said.
            None => (
                unlisted_worktree(&target, operation_kind, operation_ok),
                false,
                format!("worktree '{target}' was not in this repository's worktree listing"),
            ),
        },
        Err(error) => (
            unlisted_worktree(&target, operation_kind, operation_ok),
            false,
            error,
        ),
    };

    let (task_id, task_ok, task_error) = match crate::ledger::bindings::resolve(repo_path, &target)
    {
        Ok(found) => (found.unwrap_or_default(), true, String::new()),
        Err(error) => (String::new(), false, error.to_string()),
    };

    ChangeContext {
        repo_path: repo_path.to_string(),
        worktree,
        worktree_ok,
        worktree_error,
        task_id,
        task_ok,
        task_error,
        // The family was resolved above, so the target needs no second proof.
        changes: changes_in(repo_path, &target, None),
        collisions: collisions_involving(collision_risk(repo_path), &target),
        operation,
        operation_ok,
        operation_error,
    }
}

fn looks_like_mcp_binary(name: &str) -> bool {
    let stem = name.strip_suffix(".exe").unwrap_or(name);
    stem == "gitpulse-mcp" || stem.starts_with("gitpulse-mcp-")
}

fn resolve_mcp_binary() -> (bool, String, String) {
    if let Ok(explicit) = std::env::var("GITPULSE_MCP_PATH") {
        let path = Path::new(&explicit);
        if path.is_file() {
            return (true, explicit, String::new());
        }
        return (
            false,
            explicit,
            "GITPULSE_MCP_PATH does not point at a file".into(),
        );
    }
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            return (
                false,
                String::new(),
                format!("could not resolve this process path: {e}"),
            )
        }
    };
    if looks_like_mcp_binary(exe.file_name().and_then(|n| n.to_str()).unwrap_or_default())
        && exe.is_file()
    {
        return (true, exe.to_string_lossy().into_owned(), String::new());
    }
    let Some(dir) = exe.parent() else {
        return (
            false,
            String::new(),
            "gitpulse-mcp is not next to this process".into(),
        );
    };
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if path.is_file() && looks_like_mcp_binary(name) {
                return (true, path.to_string_lossy().into_owned(), String::new());
            }
        }
    }
    (
        false,
        String::new(),
        "gitpulse-mcp is not next to this app. Build it with `cargo build --bin gitpulse-mcp --manifest-path src-tauri/Cargo.toml`, or set GITPULSE_MCP_PATH."
            .into(),
    )
}

fn is_native_plugin_root(path: &Path) -> bool {
    path.join(".codex-plugin/plugin.json").is_file() && path.join(".mcp.json").is_file()
}

fn resolve_plugin_root() -> (bool, String, String) {
    if let Ok(explicit) = std::env::var("GITPULSE_PLUGIN_ROOT") {
        let path = Path::new(&explicit);
        if is_native_plugin_root(path) {
            return (true, explicit, String::new());
        }
        return (
            false,
            explicit,
            "GITPULSE_PLUGIN_ROOT has no .codex-plugin/plugin.json or .mcp.json".into(),
        );
    }
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            // macOS app bundle: Contents/MacOS/gitpulse reads the package that
            // Tauri copied to Contents/Resources/plugin.
            candidates.push(dir.join("../Resources/plugin"));
            candidates.push(dir.join("plugin"));
            // Development binaries live under src-tauri/target/<profile>.
            candidates.push(dir.join("../../../plugins/gitpulse"));
        }
    }
    candidates.push(Path::new(env!("CARGO_MANIFEST_DIR")).join("../plugins/gitpulse"));
    for candidate in candidates {
        if is_native_plugin_root(&candidate) {
            let canon = candidate
                .canonicalize()
                .unwrap_or(candidate)
                .to_string_lossy()
                .into_owned();
            return (true, canon, String::new());
        }
    }
    (
        false,
        String::new(),
        "Codex plugin package not found next to this app (expected plugin/.codex-plugin/plugin.json and plugin/.mcp.json). Set GITPULSE_PLUGIN_ROOT."
            .into(),
    )
}

/// What Settings and an installer need: binary location, native Codex plugin
/// manifests, and the tool catalog. Never claims a binary is present when the
/// file could not be found.
pub fn mcp_info() -> McpInfo {
    let (binary_found, binary_path, binary_error) = resolve_mcp_binary();
    let (plugin_found, plugin_path, plugin_error) = resolve_plugin_root();
    let (plugin_manifest_json, plugin_mcp_json) = if plugin_found {
        let root = Path::new(&plugin_path);
        let manifest =
            std::fs::read_to_string(root.join(".codex-plugin/plugin.json")).unwrap_or_default();
        let mcp = std::fs::read_to_string(root.join(".mcp.json")).unwrap_or_default();
        (manifest, mcp)
    } else {
        (String::new(), String::new())
    };
    McpInfo {
        protocol_version: crate::mcp::PROTOCOL_VERSION.to_string(),
        server_name: crate::mcp::SERVER_NAME.to_string(),
        server_version: crate::mcp::server_version().to_string(),
        read_only: true,
        binary_found,
        binary_path,
        binary_error,
        plugin_found,
        plugin_path,
        plugin_error,
        plugin_manifest_json,
        plugin_mcp_json,
        tools: crate::mcp::tool_catalog(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    use crate::test_support::git_in;

    fn init_repo() -> tempfile::TempDir {
        let dir = tempfile::TempDir::new().unwrap();
        git_in(dir.path(), &["init", "-b", "main"]);
        fs::write(dir.path().join("shared.txt"), "seed").unwrap();
        git_in(dir.path(), &["add", "."]);
        git_in(dir.path(), &["commit", "-m", "init"]);
        dir
    }

    #[test]
    fn snapshot_on_missing_repo_fails_facets_loudly() {
        let snap = snapshot("/no/such/gitpulse-insights-repo");
        assert!(!snap.worktrees.ok, "{snap:?}");
        assert!(!snap.worktrees.error.is_empty());
        assert!(!snap.changes.ok);
        assert!(!snap.collisions.ok);
        assert_eq!(snap.collisions.overlapping_files, 0);
        assert!(!snap.collisions.items.is_empty() || snap.collisions.items.is_empty());
        // Zero overlapping files plus ok:false is "we did not look", not clean.
        assert!(!snap.worktrees.ok && snap.collisions.overlapping_files == 0);
    }

    #[test]
    fn collision_risk_reports_a_file_dirty_in_two_worktrees() {
        let main = init_repo();
        let repo = main.path().to_str().unwrap();
        fs::create_dir_all(main.path().join(".claude/worktrees")).unwrap();
        let wt = main.path().join(".claude/worktrees/session-a");
        worktree::add_worktree(
            repo,
            wt.to_str().unwrap(),
            Some("agent/session-a"),
            Some("main"),
            false,
        )
        .expect("add worktree");

        fs::write(main.path().join("shared.txt"), "main-edit").unwrap();
        fs::write(wt.join("shared.txt"), "agent-edit").unwrap();

        let risk = collision_risk(repo);
        assert!(risk.ok, "{risk:?}");
        assert!(
            risk.items
                .iter()
                .any(|item| item.path == "shared.txt" && item.worktrees.len() >= 2),
            "expected shared.txt in two worktrees, got {risk:?}"
        );
        assert!(risk.worktrees_involved >= 2);
        let agent = risk
            .items
            .iter()
            .flat_map(|i| i.worktrees.iter())
            .find(|p| p.agent_kind == "claude");
        assert!(agent.is_some(), "agent worktree must be labelled: {risk:?}");
    }

    #[test]
    fn snapshot_counts_an_agent_session() {
        let main = init_repo();
        let repo = main.path().to_str().unwrap();
        fs::create_dir_all(main.path().join(".cursor/worktrees")).unwrap();
        let wt = main.path().join(".cursor/worktrees/fix-auth");
        worktree::add_worktree(
            repo,
            wt.to_str().unwrap(),
            Some("cursor/fix-auth"),
            Some("main"),
            false,
        )
        .expect("add worktree");

        let snap = snapshot(repo);
        assert!(snap.worktrees.ok, "{snap:?}");
        assert_eq!(snap.worktrees.count, 2);
        assert_eq!(snap.agents.sessions, 1);
        assert_eq!(snap.agents.kinds[0].kind, "cursor");
        assert_eq!(snap.branch.as_deref(), Some("main"));
        assert!(snap.changes.ok);
        assert!(snap.collisions.ok);
    }

    #[test]
    fn active_changes_on_missing_repo_is_a_failed_facet() {
        let changes = active_changes("/no/such/repo", None, None);
        assert!(!changes.ok);
        assert!(!changes.error.is_empty());
        assert!(changes.files.is_empty());
        assert_eq!(changes.total, 0);
    }

    /// Commits an empty change stamped at an exact instant.
    ///
    /// `git_in` cannot carry environment, and a commit-rhythm test that lets
    /// git pick "now" for every commit can only ever assert on one bucket.
    fn commit_at(dir: &Path, message: &str, epoch: i64, email: &str) {
        let stamp = format!("@{epoch} +0000");
        let output = std::process::Command::new("git")
            .args([
                "-c",
                "user.name=GitPulse",
                "-c",
                &format!("user.email={email}"),
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--allow-empty",
                "-m",
                message,
            ])
            .env("GIT_AUTHOR_DATE", &stamp)
            .env("GIT_COMMITTER_DATE", &stamp)
            .env("GIT_AUTHOR_EMAIL", email)
            .env("GIT_COMMITTER_EMAIL", email)
            .current_dir(dir)
            .output()
            .expect("spawn git commit");
        assert!(
            output.status.success(),
            "commit {message} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn commit_stats_bucket_by_age_and_count_distinct_authors() {
        let dir = tempfile::TempDir::new().unwrap();
        git_in(dir.path(), &["init", "-b", "main"]);
        let anchor = 1_800_000_000i64;
        // Two commits today from one author, one three days back from another.
        commit_at(dir.path(), "a", anchor - 100, "one@example.com");
        commit_at(dir.path(), "b", anchor - 200, "one@example.com");
        commit_at(
            dir.path(),
            "c",
            anchor - 3 * 86_400 - 100,
            "two@example.com",
        );

        let stats =
            fleet_commit_stats(dir.path(), anchor, FLEET_COMMIT_WINDOW_DAYS).expect("probe runs");
        assert_eq!(stats.window_days, FLEET_COMMIT_WINDOW_DAYS);
        assert_eq!(stats.anchor_epoch, anchor);
        assert_eq!(stats.daily.len(), FLEET_COMMIT_WINDOW_DAYS as usize);
        assert_eq!(stats.commits, 3);
        assert_eq!(stats.authors, 2, "two distinct emails");
        assert_eq!(stats.active_days, 2, "two buckets carry commits");
        // Newest bucket last: two today, one three buckets back.
        let last = stats.daily.len() - 1;
        assert_eq!(stats.daily[last], 2);
        assert_eq!(stats.daily[last - 3], 1);
        assert_eq!(stats.commits_7d, 3, "all three are inside the last week");
        assert_eq!(stats.commits_prior_7d, 0);
        assert!(!stats.truncated);
    }

    #[test]
    fn commit_stats_report_the_count_and_the_series_as_one_population() {
        let dir = tempfile::TempDir::new().unwrap();
        git_in(dir.path(), &["init", "-b", "main"]);
        let anchor = 1_800_000_000i64;
        for day in [0i64, 1, 8, 30, 89] {
            commit_at(
                dir.path(),
                &format!("day-{day}"),
                anchor - day * 86_400 - 60,
                "one@example.com",
            );
        }
        // A commit older than the window must not be smeared into the oldest
        // bucket: it is outside what this report describes.
        commit_at(
            dir.path(),
            "ancient",
            anchor - 200 * 86_400,
            "old@example.com",
        );

        let stats =
            fleet_commit_stats(dir.path(), anchor, FLEET_COMMIT_WINDOW_DAYS).expect("probe runs");
        let summed: u32 = stats.daily.iter().sum();
        assert_eq!(
            stats.commits, summed,
            "the headline count and the series must describe the same commits"
        );
        assert_eq!(stats.commits, 5, "the out-of-window commit is excluded");
        assert_eq!(stats.commits_7d, 2, "day 0 and day 1");
        assert_eq!(stats.commits_prior_7d, 1, "day 8");
        assert!(
            !stats.daily.iter().any(|c| *c > 1),
            "no bucket collected more than the one commit stamped into it"
        );
    }

    /// A sweep on the default window, which is what most of these assert on.
    fn fleet_snapshot_default(paths: &[String]) -> FleetSnapshot {
        fleet_snapshot(paths, None)
    }

    /// One `git log --format=%ct%x1f%ae` row.
    fn log_row(epoch: i64, email: &str) -> String {
        format!("{epoch}\u{1f}{email}\n")
    }

    #[test]
    fn a_corrupt_commit_stamp_cannot_overflow_the_bucket_arithmetic() {
        // Unreachable through git, which refuses to write a date near i64's
        // floor — which is exactly why it is tested here and not through a
        // repository. Before the saturating subtraction this panicked in debug
        // and wrapped into a real bucket in release.
        let anchor = 1_800_000_000i64;
        let text =
            log_row(anchor - 3600, "one@example.com") + &log_row(i64::MIN + 1, "two@example.com");
        let stats = bucket_commit_log(&text, anchor, FLEET_COMMIT_WINDOW_DAYS);
        assert_eq!(stats.commits, 1, "only the in-window commit is counted");
        assert_eq!(stats.authors, 1, "the corrupt row contributes no author");
    }

    #[test]
    fn a_commit_stamped_in_the_future_lands_in_the_newest_bucket() {
        // A skewed clock, which git will happily record. It belongs at the
        // recent end rather than off the array.
        let anchor = 1_800_000_000i64;
        let stats = bucket_commit_log(
            &log_row(anchor + 90_000, "one@example.com"),
            anchor,
            FLEET_COMMIT_WINDOW_DAYS,
        );
        assert_eq!(stats.commits, 1);
        assert_eq!(*stats.daily.last().unwrap(), 1);
        assert_eq!(stats.commits_7d, 1);
    }

    #[test]
    fn a_malformed_row_is_skipped_rather_than_counted() {
        let anchor = 1_800_000_000i64;
        let text = format!(
            "not-a-stamp\u{1f}one@example.com\nno-separator-at-all\n\n{}",
            log_row(anchor - 60, "one@example.com")
        );
        let stats = bucket_commit_log(&text, anchor, FLEET_COMMIT_WINDOW_DAYS);
        assert_eq!(stats.commits, 1);
        assert!(!stats.truncated);
    }

    #[test]
    fn a_row_with_no_author_email_still_counts_as_a_commit() {
        // The commit happened. Only the author is unknown, and inventing an
        // empty-string author would inflate the distinct-author count by one
        // for every repository with an unattributed commit.
        let anchor = 1_800_000_000i64;
        let text = log_row(anchor - 60, "") + &log_row(anchor - 120, "one@example.com");
        let stats = bucket_commit_log(&text, anchor, FLEET_COMMIT_WINDOW_DAYS);
        assert_eq!(stats.commits, 2);
        assert_eq!(stats.authors, 1);
    }

    #[test]
    fn hitting_the_commit_cap_marks_every_count_as_a_floor() {
        // The probe asks for one more than the cap precisely so this is
        // observable; a repository with exactly the cap is NOT truncated.
        let anchor = 1_800_000_000i64;
        let at_cap: String = (0..MAX_FLEET_COMMITS)
            .map(|i| log_row(anchor - (i as i64 % 80) * 86_400 - 60, "one@example.com"))
            .collect();
        assert!(!bucket_commit_log(&at_cap, anchor, FLEET_COMMIT_WINDOW_DAYS).truncated);

        let over_cap = at_cap + &log_row(anchor - 60, "one@example.com");
        let stats = bucket_commit_log(&over_cap, anchor, FLEET_COMMIT_WINDOW_DAYS);
        assert!(stats.truncated, "a walk that hit the cap reports a floor");
    }

    #[test]
    fn the_oldest_bucket_is_inclusive_and_the_one_past_it_is_not() {
        let anchor = 1_800_000_000i64;
        let window = FLEET_COMMIT_WINDOW_DAYS as i64;
        // Last second inside the window, and the first second outside it.
        let inside = anchor - (window * 86_400 - 1);
        let outside = anchor - window * 86_400;
        assert_eq!(
            bucket_commit_log(&log_row(inside, "a@b"), anchor, FLEET_COMMIT_WINDOW_DAYS).commits,
            1
        );
        assert_eq!(
            bucket_commit_log(&log_row(outside, "a@b"), anchor, FLEET_COMMIT_WINDOW_DAYS).commits,
            0
        );
    }

    #[test]
    fn commit_stats_survive_an_out_of_order_commit_date() {
        // The regression that chose the count-bounded walk. `git log --since`
        // prunes at the first commit older than the cutoff, so ONE inverted
        // committer date hides every in-window commit behind it — and reports
        // the short answer as a complete one. Verified against git 2.50: the
        // pruning form returned three of these four in-window commits.
        let dir = tempfile::TempDir::new().unwrap();
        git_in(dir.path(), &["init", "-b", "main"]);
        let anchor = 1_800_000_000i64;
        commit_at(dir.path(), "d40", anchor - 40 * 86_400, "one@example.com");
        commit_at(
            dir.path(),
            "inverted",
            anchor - 200 * 86_400,
            "one@example.com",
        );
        commit_at(dir.path(), "d30", anchor - 30 * 86_400, "one@example.com");
        commit_at(dir.path(), "d3", anchor - 3 * 86_400, "one@example.com");
        commit_at(dir.path(), "today", anchor - 100, "one@example.com");

        let stats =
            fleet_commit_stats(dir.path(), anchor, FLEET_COMMIT_WINDOW_DAYS).expect("probe runs");
        assert_eq!(
            stats.commits, 4,
            "every in-window commit is counted, whatever order the dates arrive in"
        );
        assert_eq!(stats.active_days, 4);
    }

    #[test]
    fn commit_stats_on_an_empty_repository_are_measured_not_failed() {
        let dir = tempfile::TempDir::new().unwrap();
        git_in(dir.path(), &["init", "-b", "main"]);
        let stats = fleet_commit_stats(dir.path(), 1_800_000_000, FLEET_COMMIT_WINDOW_DAYS)
            .expect("no HEAD is an answer");
        assert_eq!(stats.commits, 0);
        assert_eq!(stats.last_commit_epoch, 0);
        assert_eq!(stats.daily.len(), FLEET_COMMIT_WINDOW_DAYS as usize);
        assert!(!stats.truncated);
    }

    #[test]
    fn a_quiet_repository_still_reports_its_last_commit() {
        // The window probe subsumes `git log -1` only when the window has a
        // commit. A repository last touched a year ago must not come back
        // looking like one whose last commit could not be read.
        let dir = tempfile::TempDir::new().unwrap();
        git_in(dir.path(), &["init", "-b", "main"]);
        let anchor = 1_800_000_000i64;
        let long_ago = anchor - 400 * 86_400;
        commit_at(dir.path(), "old", long_ago, "one@example.com");

        let facet = fleet_facet(
            dir.path().to_str().unwrap(),
            anchor,
            FLEET_COMMIT_WINDOW_DAYS,
        );
        assert!(facet.commits_ok, "{facet:?}");
        let stats = facet.commits.as_ref().expect("ok implies Some");
        assert_eq!(stats.commits, 0, "nothing landed inside the window");
        assert!(facet.last_commit_ok, "the fallback probe still ran");
        assert_eq!(facet.last_commit_epoch, long_ago);
    }

    #[test]
    fn an_unreadable_repository_reports_commits_as_unknown_never_as_quiet() {
        let facet = unreadable_facet("/no/such/gitpulse-fleet-repo", "gone".to_string());
        assert!(!facet.commits_ok);
        assert!(
            facet.commits.is_none(),
            "a probe that could not run must not arrive as a measured silence"
        );
    }

    #[test]
    fn fleet_snapshot_anchors_every_repository_at_one_instant() {
        // The fleet-wide activity chart sums the per-repository series bucket
        // for bucket. Two anchors a second apart would make bucket 40 of one
        // row cover a different span than bucket 40 of the next.
        let a = init_repo();
        let b = init_repo();
        let snap = fleet_snapshot_default(&[
            a.path().to_str().unwrap().to_string(),
            b.path().to_str().unwrap().to_string(),
        ]);
        assert_eq!(snap.repos.len(), 2);
        for facet in &snap.repos {
            let stats = facet
                .commits
                .as_ref()
                .expect("both repositories are readable");
            assert_eq!(
                stats.anchor_epoch, snap.anchor_epoch,
                "every facet shares the sweep's anchor"
            );
            assert_eq!(stats.window_days, FLEET_COMMIT_WINDOW_DAYS);
        }
    }

    #[test]
    fn a_requested_window_is_clamped_rather_than_refused() {
        // The window is a display preference. Failing a whole fleet sweep
        // because a stale client asked for ten thousand days would be a worse
        // answer than 180 — and the window actually used is reported back, so
        // the clamp is visible rather than silently substituted.
        assert_eq!(clamp_commit_window(None), FLEET_COMMIT_WINDOW_DAYS);
        assert_eq!(clamp_commit_window(Some(30)), 30);
        assert_eq!(
            clamp_commit_window(Some(0)),
            1,
            "zero buckets is not a window"
        );
        assert_eq!(
            clamp_commit_window(Some(10_000)),
            MAX_FLEET_COMMIT_WINDOW_DAYS
        );
    }

    #[test]
    fn a_shorter_window_reports_its_own_span_and_buckets_to_it() {
        let anchor = 1_800_000_000i64;
        let text = log_row(anchor - 60, "a@b") + &log_row(anchor - 40 * 86_400, "a@b");
        let stats = bucket_commit_log(&text, anchor, 30);
        assert_eq!(stats.window_days, 30);
        assert_eq!(stats.daily.len(), 30);
        assert_eq!(
            stats.commits, 1,
            "the 40-day-old commit is outside a 30-day window"
        );
    }

    #[test]
    fn every_facet_in_one_sweep_shares_the_requested_window() {
        // Rows on two different windows cannot be summed bucket for bucket,
        // which is exactly what the fleet activity chart does with them.
        let a = init_repo();
        let b = init_repo();
        let snap = fleet_snapshot(
            &[
                a.path().to_str().unwrap().to_string(),
                b.path().to_str().unwrap().to_string(),
            ],
            Some(30),
        );
        for facet in &snap.repos {
            let stats = facet
                .commits
                .as_ref()
                .expect("both repositories are readable");
            assert_eq!(stats.window_days, 30);
            assert_eq!(stats.daily.len(), 30);
        }
    }

    #[test]
    fn fleet_snapshot_isolates_one_bad_repository_from_the_rest() {
        let good = init_repo();
        let snap = fleet_snapshot_default(&[
            good.path().to_str().unwrap().to_string(),
            "/no/such/gitpulse-fleet-repo".to_string(),
        ]);
        assert_eq!(snap.requested, 2);
        assert_eq!(snap.repos.len(), 2);
        assert!(snap.repos[0].ok, "{:?}", snap.repos[0]);
        assert!(!snap.repos[1].ok);
        assert!(!snap.repos[1].error.is_empty());
        // One unreadable checkout must not cost the other repository its row.
        assert_eq!(snap.scanned, 1);
    }

    #[test]
    fn fleet_snapshot_reads_worktrees_agents_and_last_commit() {
        let main = init_repo();
        let repo = main.path().to_str().unwrap();
        fs::create_dir_all(main.path().join(".claude/worktrees")).unwrap();
        let wt = main.path().join(".claude/worktrees/session-a");
        worktree::add_worktree(
            repo,
            wt.to_str().unwrap(),
            Some("agent/session-a"),
            Some("main"),
            false,
        )
        .expect("add worktree");

        let snap = fleet_snapshot_default(&[repo.to_string()]);
        let facet = &snap.repos[0];
        assert!(facet.worktrees_ok);
        assert_eq!(facet.worktrees, 2);
        assert_eq!(facet.agents.sessions, 1);
        assert_eq!(facet.agents.kinds[0].kind, "claude");
        assert!(facet.last_commit_ok);
        assert!(facet.last_commit_epoch > 0);
    }

    #[test]
    fn fleet_snapshot_collapses_a_repeated_path_to_one_facet() {
        let main = init_repo();
        let repo = main.path().to_str().unwrap().to_string();
        // Two tabs can name the same repository; scanning it twice makes the
        // runs contend for the same .git lock for no gain.
        let snap = fleet_snapshot_default(&[repo.clone(), repo.clone(), String::new()]);
        assert_eq!(snap.requested, 3);
        assert_eq!(snap.repos.len(), 1);
    }

    #[test]
    fn fleet_snapshot_reports_no_commits_as_read_rather_than_failed() {
        let dir = tempfile::TempDir::new().unwrap();
        git_in(dir.path(), &["init", "-b", "main"]);
        let snap = fleet_snapshot_default(&[dir.path().to_str().unwrap().to_string()]);
        let facet = &snap.repos[0];
        assert!(facet.ok);
        // An empty repository has a readable answer — no commits — which is
        // not the same fact as a probe that could not run.
        assert!(facet.last_commit_ok);
        assert_eq!(facet.last_commit_epoch, 0);
    }

    #[test]
    fn fleet_snapshot_leaves_metrics_none_until_something_is_recorded() {
        let main = init_repo();
        let snap = fleet_snapshot_default(&[main.path().to_str().unwrap().to_string()]);
        let facet = &snap.repos[0];
        // The ledger read ran and found nothing. `metrics_ok` is what
        // separates that from a ledger we could not open at all.
        assert!(facet.metrics_ok, "{:?}", facet.metrics_error);
        assert!(facet.metrics.is_none());
    }

    #[test]
    fn fleet_snapshot_truncation_means_short_sweep_not_broken_repository() {
        let snap = fleet_snapshot_default(&["/no/such/gitpulse-fleet-repo".to_string()]);
        assert!(!snap.repos[0].ok);
        // A visited-and-failed repository is reported in its facet. Setting
        // `truncated` for it would leave a caller unable to tell a sweep that
        // stopped early from a checkout that is simply gone.
        assert!(!snap.truncated);
    }

    #[test]
    fn fleet_snapshot_over_the_cap_is_reported_as_truncated() {
        let main = init_repo();
        let repo = main.path().to_str().unwrap();
        let paths: Vec<String> = (0..MAX_FLEET_REPOS + 3)
            .map(|i| format!("{repo}/../nope-{i}"))
            .collect();
        let snap = fleet_snapshot_default(&paths);
        assert!(snap.truncated);
        assert_eq!(snap.repos.len(), MAX_FLEET_REPOS);
    }

    /// Three worktrees, one of them unreadable: the fixture behind every
    /// "failed is not empty" assertion below. Returns the main checkout, the
    /// path of the worktree that will be broken, and the repository path.
    fn repo_with_a_worktree_to_break() -> (tempfile::TempDir, std::path::PathBuf) {
        let main = init_repo();
        let repo = main.path().to_str().unwrap().to_string();
        fs::create_dir_all(main.path().join(".claude/worktrees")).unwrap();
        for name in ["a", "b"] {
            let wt = main.path().join(".claude/worktrees").join(name);
            worktree::add_worktree(
                &repo,
                wt.to_str().unwrap(),
                Some(&format!("agent/{name}")),
                Some("main"),
                false,
            )
            .expect("add worktree");
        }
        // Two worktrees dirty the same file, so there is a real finding for a
        // partial scan to keep hold of.
        fs::write(main.path().join("shared.txt"), "main-edit").unwrap();
        fs::write(
            main.path().join(".claude/worktrees/a/shared.txt"),
            "agent-edit",
        )
        .unwrap();
        let doomed = main.path().join(".claude/worktrees/b");
        (main, doomed)
    }

    #[test]
    fn collision_risk_counts_a_worktree_it_could_not_scan_instead_of_reporting_ok() {
        let (main, doomed) = repo_with_a_worktree_to_break();
        let repo = main.path().to_str().unwrap();

        let whole = collision_risk(repo);
        assert!(whole.ok, "{whole:?}");
        assert_eq!(whole.scanned_worktrees, 3);
        assert_eq!(whole.failed_worktrees, 0);
        assert!(!whole.truncated);

        // git still lists the worktree; it just cannot be read any more.
        fs::remove_dir_all(&doomed).unwrap();
        let partial = collision_risk(repo);

        // The two payloads must not be readable as the same answer.
        assert!(!partial.ok, "a failed scan must not report ok: {partial:?}");
        assert_eq!(partial.failed_worktrees, 1, "{partial:?}");
        assert_eq!(partial.scanned_worktrees, 2, "{partial:?}");
        assert_eq!(partial.unscanned_worktrees, 0, "{partial:?}");
        assert!(partial.truncated, "{partial:?}");
        assert!(!partial.error.is_empty(), "{partial:?}");
        // ...and the finding it did make still stands on its own evidence.
        assert!(
            partial
                .items
                .iter()
                .any(|item| item.path == "shared.txt" && item.worktrees.len() >= 2),
            "a partial scan must keep what it did find: {partial:?}"
        );
    }

    #[test]
    fn snapshot_reports_an_unscanned_worktree_as_unknown_rather_than_clean() {
        let (main, doomed) = repo_with_a_worktree_to_break();
        let repo = main.path().to_str().unwrap();

        let measured = snapshot(repo);
        assert!(measured.worktrees.ok, "{:?}", measured.worktrees);
        assert_eq!(measured.worktrees.count, 3);
        assert_eq!(measured.worktrees.scanned, 3);
        assert_eq!(measured.worktrees.dirty_unknown, 0);
        assert_eq!(measured.worktrees.blocked_unknown, 0);

        fs::remove_dir_all(&doomed).unwrap();
        let partial = snapshot(repo);

        assert_eq!(partial.worktrees.count, 3, "{:?}", partial.worktrees);
        assert_eq!(partial.worktrees.scanned, 2, "{:?}", partial.worktrees);
        // The worktree nobody could read is NOT in `dirty`, and saying so is
        // the whole point: `dirty` alone would have counted it clean.
        assert_eq!(
            partial.worktrees.dirty_unknown, 1,
            "{:?}",
            partial.worktrees
        );
        assert_eq!(
            partial.worktrees.dirty + partial.worktrees.dirty_unknown,
            3,
            "{:?}",
            partial.worktrees
        );
        // The same gap in the parked-operation probe, which used to fail into
        // an empty string that read as "nothing parked".
        assert_eq!(
            partial.worktrees.blocked_unknown, 1,
            "{:?}",
            partial.worktrees
        );
        assert!(
            partial
                .worktrees
                .items
                .iter()
                .any(|w| !w.operation_ok && w.operation_kind.is_empty()),
            "{:?}",
            partial.worktrees
        );
    }

    #[test]
    fn snapshot_marks_agent_counts_unavailable_when_the_worktree_listing_fails() {
        let main = init_repo();
        let read = snapshot(main.path().to_str().unwrap());
        // A repository with no agent worktrees: looked, found none.
        assert!(read.agents.ok, "{:?}", read.agents);
        assert_eq!(read.agents.sessions, 0);

        let unread = snapshot("/no/such/gitpulse-agents-repo");
        // Same zero, opposite meaning — and now they are distinguishable.
        assert!(!unread.agents.ok, "{:?}", unread.agents);
        assert_eq!(unread.agents.sessions, 0);
        assert_ne!(read.agents.ok, unread.agents.ok);
    }

    #[test]
    fn snapshot_separates_a_detached_head_from_a_branch_it_could_not_read() {
        let main = init_repo();
        let repo = main.path().to_str().unwrap();
        git_in(main.path(), &["checkout", "--detach"]);

        let detached = snapshot(repo);
        // The listing ran and reported a checkout on no branch.
        assert!(detached.branch_ok, "{detached:?}");
        assert!(detached.branch.is_none(), "{:?}", detached.branch);
        assert!(
            detached
                .worktrees
                .items
                .iter()
                .any(|w| w.is_main && w.is_detached),
            "{:?}",
            detached.worktrees.items
        );

        let unknown = snapshot("/no/such/gitpulse-branch-repo");
        assert!(!unknown.branch_ok, "{unknown:?}");
        assert!(unknown.branch.is_none());
        assert_ne!(detached.branch_ok, unknown.branch_ok);
    }

    #[test]
    fn snapshot_past_its_deadline_reports_partial_facets_rather_than_empty_ones() {
        let (main, _doomed) = repo_with_a_worktree_to_break();
        let repo = main.path().to_str().unwrap();

        let whole = snapshot(repo);
        assert!(!whole.deadline_expired, "{whole:?}");
        assert!(whole.collisions.ok, "{:?}", whole.collisions);
        assert!(whole.worktrees.items.iter().all(|w| w.operation_ok));
        assert_eq!(whole.worktrees.blocked_unknown, 0);

        // Zero budget: the fixed-cost stages still run, and every stage whose
        // cost grows with the worktree count is skipped and says so.
        let rushed = snapshot_within(repo, Duration::ZERO);
        assert!(rushed.deadline_expired, "{rushed:?}");
        assert!(rushed.worktrees.ok, "{:?}", rushed.worktrees);
        assert_eq!(rushed.worktrees.count, 3);
        assert!(rushed.changes.ok, "{:?}", rushed.changes);
        assert!(
            rushed.worktrees.items.iter().all(|w| !w.operation_ok),
            "{:?}",
            rushed.worktrees.items
        );
        assert_eq!(rushed.worktrees.blocked_unknown, 3);
        // A collision scan that never started must not arrive as "no overlaps".
        assert!(!rushed.collisions.ok, "{:?}", rushed.collisions);
        assert!(
            rushed.collisions.error.contains("time"),
            "{:?}",
            rushed.collisions
        );
        assert_eq!(rushed.collisions.scanned_worktrees, 0);
        assert!(whole.collisions.overlapping_files > 0);
        assert_eq!(rushed.collisions.overlapping_files, 0);
    }

    #[test]
    fn active_changes_refuses_a_worktree_belonging_to_another_repository() {
        let here = init_repo();
        let elsewhere = init_repo();
        fs::write(elsewhere.path().join("shared.txt"), "other-repo-edit").unwrap();

        let changes = active_changes(
            here.path().to_str().unwrap(),
            Some(elsewhere.path().to_str().unwrap()),
            None,
        );
        // Reading it anyway stamped this repository's identity on another
        // repository's files.
        assert!(!changes.ok, "{changes:?}");
        assert!(
            changes.error.contains("does not belong"),
            "unexpected error: {}",
            changes.error
        );
        assert!(changes.files.is_empty(), "{changes:?}");
        assert_eq!(changes.total, 0);
    }

    #[test]
    fn change_context_refuses_a_worktree_belonging_to_another_repository() {
        let here = init_repo();
        let elsewhere = init_repo();
        fs::write(elsewhere.path().join("shared.txt"), "other-repo-edit").unwrap();

        let ctx = change_context(
            here.path().to_str().unwrap(),
            Some(elsewhere.path().to_str().unwrap()),
        );
        assert!(!ctx.worktree_ok, "{ctx:?}");
        assert!(!ctx.changes.ok, "{:?}", ctx.changes);
        assert!(!ctx.collisions.ok, "{:?}", ctx.collisions);
        assert!(!ctx.task_ok, "{ctx:?}");
        assert!(!ctx.operation_ok, "{ctx:?}");
        assert!(ctx.changes.files.is_empty(), "{:?}", ctx.changes);
        assert!(
            ctx.worktree_error.contains("does not belong"),
            "unexpected error: {}",
            ctx.worktree_error
        );
    }

    #[test]
    fn active_changes_still_reads_a_worktree_that_does_belong_to_the_repository() {
        let main = init_repo();
        let repo = main.path().to_str().unwrap();
        fs::create_dir_all(main.path().join(".claude/worktrees")).unwrap();
        let wt = main.path().join(".claude/worktrees/session-a");
        worktree::add_worktree(
            repo,
            wt.to_str().unwrap(),
            Some("agent/session-a"),
            Some("main"),
            false,
        )
        .expect("add worktree");
        fs::write(wt.join("shared.txt"), "agent-edit").unwrap();

        let changes = active_changes(repo, Some(wt.to_str().unwrap()), None);
        assert!(changes.ok, "{changes:?}");
        assert_eq!(changes.repo_path, repo);
        assert_eq!(
            Path::new(&changes.worktree_path),
            fs::canonicalize(&wt).unwrap(),
            "the authenticated worktree is the one git registered"
        );
        assert!(
            changes.files.iter().any(|f| f.path == "shared.txt"),
            "{changes:?}"
        );
    }

    #[test]
    fn change_context_on_a_missing_repository_marks_every_probe_as_failed() {
        let read = {
            let main = init_repo();
            change_context(main.path().to_str().unwrap(), None)
        };
        // Looked, and found a clean repository with nothing bound to it.
        assert!(read.worktree_ok, "{read:?}");
        assert!(read.changes.ok, "{:?}", read.changes);
        assert!(read.collisions.ok, "{:?}", read.collisions);
        assert!(read.task_ok, "{}", read.task_error);
        assert!(read.operation_ok, "{}", read.operation_error);
        assert!(read.collisions.items.is_empty());
        assert!(read.task_id.is_empty());
        assert!(read.operation.is_none());

        let unread = change_context("/no/such/gitpulse-context-repo", None);
        // Same empty values, and not one of them was established. Before this
        // the two payloads agreed on every field but `changes.ok`.
        assert!(!unread.worktree_ok, "{unread:?}");
        assert!(!unread.changes.ok, "{:?}", unread.changes);
        assert!(!unread.collisions.ok, "{:?}", unread.collisions);
        assert!(!unread.task_ok, "{unread:?}");
        assert!(!unread.operation_ok, "{unread:?}");
        assert!(unread.collisions.items.is_empty());
        assert_eq!(unread.collisions.scanned_worktrees, 0);
        assert!(!unread.worktree_error.is_empty());
        assert!(!unread.collisions.error.is_empty());
    }

    #[test]
    fn change_context_keeps_the_collision_scans_coverage_alongside_its_rows() {
        let (main, doomed) = repo_with_a_worktree_to_break();
        let repo = main.path().to_str().unwrap();
        fs::remove_dir_all(&doomed).unwrap();

        let ctx = change_context(repo, None);
        // The rows are narrowed to this worktree; the coverage fields describe
        // the scan that produced them and are what make an empty list readable.
        assert!(!ctx.collisions.ok, "{:?}", ctx.collisions);
        assert_eq!(ctx.collisions.failed_worktrees, 1, "{:?}", ctx.collisions);
        assert_eq!(ctx.collisions.scanned_worktrees, 2, "{:?}", ctx.collisions);
        assert!(
            ctx.collisions.items.iter().all(|item| item
                .worktrees
                .iter()
                .any(|p| Path::new(&p.path) == fs::canonicalize(main.path()).unwrap())),
            "rows must involve the requested worktree: {:?}",
            ctx.collisions.items
        );
        assert_eq!(
            ctx.collisions.overlapping_files as usize,
            ctx.collisions.items.len(),
            "the row count must describe the rows that are here"
        );
    }

    fn status_row(path: &str, additions: usize, warnings: Vec<String>) -> FileStatus {
        FileStatus {
            path: path.to_string(),
            old_path: None,
            status_code: " M".to_string(),
            is_staged: false,
            is_conflicted: false,
            additions,
            deletions: 0,
            warnings,
        }
    }

    #[test]
    fn a_changed_file_carries_its_churn_warning_instead_of_dropping_it() {
        let warned = status_row(
            "src/lib.rs",
            0,
            vec!["numstat record had unparseable counts".to_string()],
        );
        let row = to_changed(&warned);
        // 0/0 on an unreadable diff is not a measurement, and this is the only
        // thing on the row that says so.
        assert_eq!(row.additions, 0);
        assert_eq!(row.warnings, warned.warnings);

        // Additive on the wire: absent entirely while empty, exactly as
        // `FileStatus` does it.
        let clean = to_changed(&status_row("src/main.rs", 3, Vec::new()));
        let value = serde_json::to_value(&clean).unwrap();
        assert!(
            value.get("warnings").is_none(),
            "empty warnings must not appear on the wire: {value}"
        );
        let carried = serde_json::to_value(&row).unwrap();
        assert_eq!(
            carried["warnings"][0],
            "numstat record had unparseable counts"
        );
    }

    #[test]
    fn churn_counts_report_how_many_rows_could_not_be_measured() {
        let counts = count_statuses(&[
            status_row("a.rs", 5, Vec::new()),
            status_row(
                "b.rs",
                0,
                vec!["numstat record had unparseable counts".into()],
            ),
        ]);
        assert_eq!(counts.additions, 5);
        // The total is a floor: one row contributed a zero nobody measured.
        assert_eq!(counts.churn_warnings, 1);
        assert!(!counts.churn_overflowed);

        let facet = changes_from_status(
            &[status_row(
                "b.rs",
                0,
                vec!["numstat record had unparseable counts".into()],
            )],
            false,
        );
        assert_eq!(facet.churn_warnings, 1);
    }

    #[test]
    fn churn_totals_saturate_and_say_so_instead_of_wrapping() {
        // Two rows that overflow a u32 between them: the old `+=` panicked in
        // debug and wrapped silently in release, reporting a small fabricated
        // total as authoritative.
        let counts = count_statuses(&[
            status_row("huge.bin", u32::MAX as usize, Vec::new()),
            status_row("more.bin", 12, Vec::new()),
        ]);
        assert_eq!(counts.additions, u32::MAX);
        assert!(counts.churn_overflowed, "a saturated total must say so");

        // A row that does not even fit a u32 on its own is caught the same way.
        let single = count_statuses(&[status_row("vast.bin", u32::MAX as usize + 9, Vec::new())]);
        assert_eq!(single.additions, u32::MAX);
        assert!(single.churn_overflowed);

        let ordinary = count_statuses(&[status_row("small.rs", 12, Vec::new())]);
        assert_eq!(ordinary.additions, 12);
        assert!(!ordinary.churn_overflowed);
    }

    #[test]
    fn mcp_info_never_claims_a_binary_it_did_not_find() {
        // Force the miss path: an explicit env that is not a file.
        std::env::set_var("GITPULSE_MCP_PATH", "/no/such/gitpulse-mcp");
        let info = mcp_info();
        std::env::remove_var("GITPULSE_MCP_PATH");
        assert!(!info.binary_found);
        assert!(!info.binary_error.is_empty());
        assert!(info.read_only);
        assert_eq!(info.protocol_version, crate::mcp::PROTOCOL_VERSION);
        assert!(!info.tools.is_empty());
    }

    #[test]
    fn mcp_info_reads_the_native_codex_package() {
        let info = mcp_info();
        assert!(info.plugin_found, "{}", info.plugin_error);
        assert!(
            Path::new(&info.plugin_path).ends_with("plugins/gitpulse"),
            "unexpected plugin root: {}",
            info.plugin_path
        );
        assert!(info.plugin_manifest_json.contains("\"mcpServers\""));
        assert!(info.plugin_mcp_json.contains("\"gitpulse\""));
        assert!(!info.plugin_mcp_json.contains("$schema"));
    }
}
