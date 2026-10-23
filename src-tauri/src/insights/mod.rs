//! Read-only repository insights for agents and the Work view.
//!
//! Assembles worktrees, agent sessions, working-tree changes, and overlapping
//! dirty files into one snapshot. Every facet reports whether it could run:
//! a check that did not run must never look like one that ran and found
//! nothing. Nothing here mutates a repository.

mod collision_memo;
mod entity_collision;
#[cfg(test)]
mod head_divergence_tests;
#[cfg(test)]
mod mcp_info_adversarial;
pub(crate) mod live_sessions;
#[cfg(test)]
mod merge_adv_tests;
pub mod merge_conflicts;
#[cfg(test)]
mod merge_conflicts_bench_tests;

pub use crate::engine::worktree::ScanDeadline;
pub use collision_memo::{
    collision_for_path_memoized, MemoizedCollision, TTL as COLLISION_MEMO_TTL,
};
pub use entity_collision::{
    classify_incoming, classify_overlapping_path, parse_old_side_ranges, EntityCollisionKind,
    EntityCollisionVerdict, IncomingEdit, LineRange, OldSideHunk,
};

use crate::codeintel::{self, CodeintelStatus};
use crate::engine::git_cli::{current_admission, git_text, with_admission};
use crate::engine::git_reader::{FileStatus, GitReader};
use crate::engine::repo_op::{self, RepoOperation};
use crate::engine::validate_repo;
use crate::engine::worktree::{self, agent_layout, changed_paths_within, StatusRead, WorktreeInfo};
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
    /// The commit this checkout is on (a full object name), detached or not.
    /// `None` when git named none (a bare entry, a checkout the listing did
    /// not describe).
    #[serde(default)]
    pub head: Option<String>,
    /// Commits on `head` that the repository's default base lacks: the lane's
    /// committed-but-unmerged work, which `dirty_files` stops showing the
    /// moment a lane commits. `None` is "not measured" (see
    /// `divergence_note`), never zero.
    #[serde(default)]
    pub ahead: Option<u32>,
    /// Commits on the default base that `head` lacks.
    #[serde(default)]
    pub behind: Option<u32>,
    /// Commits since `head` left the default base: the same count as `ahead`,
    /// under the name a reader asking "how much has this lane done" looks for.
    #[serde(default)]
    pub commits_since_base: Option<u32>,
    /// Why `ahead`, `behind` and `commits_since_base` are absent for a
    /// checkout that has a `head` (`unmeasured: ...`), or why they are not
    /// exact (`shallow: counts are bounded by the clone depth`); `None` when
    /// they were measured against whole history.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub divergence_note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentKindCount {
    pub kind: String,
    pub sessions: u32,
}

/// One running agent session, as its agent registered it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveSession {
    pub kind: String,
    pub pid: u32,
    /// How the session was started (`cli`, `claude-desktop`, …), verbatim
    /// from the agent; empty when it did not say.
    pub entrypoint: String,
    /// The agent's own last-reported state (`busy`, `idle`, …), verbatim.
    pub status: String,
    pub cwd: String,
}

/// The live sessions whose working directory is inside one worktree.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveWorktree {
    pub path: String,
    pub sessions: Vec<LiveSession>,
}

/// What one agent kind's detector established for this repository.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveKindStatus {
    pub kind: String,
    /// Whether this kind's sessions could be observed at all. `sessions: 0`
    /// with `ok: false` is "unknown", never "none running".
    pub ok: bool,
    pub error: String,
    /// Live sessions of this kind attributed to a worktree of this repository.
    pub sessions: u32,
    /// Registry entries that may be live sessions here but could not be
    /// verified (unreadable, or the process could not be judged). Non-zero
    /// makes `sessions` a floor.
    pub unverified: u32,
    /// True when the registry held more entries than one probe reads.
    pub truncated: bool,
}

/// Running agent sessions, attributed to worktree paths.
///
/// This is the half [`AgentSummary::sessions`] cannot see: that count is of
/// worktrees laid out for an agent, so several sessions in one checkout —
/// the main one included — read as one, or as none.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveSessionFacet {
    /// True only when every kind was observed, completely and verifiably.
    pub ok: bool,
    /// Live sessions attributed to this repository, over the kinds that could
    /// be observed. A floor whenever `ok` is false.
    pub sessions: u32,
    pub kinds: Vec<LiveKindStatus>,
    /// Worktrees with at least one live session.
    pub worktrees: Vec<LiveWorktree>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSummary {
    /// Whether the worktree listing these counts are derived from ran at all.
    /// Zero sessions with `ok: false` is "we could not look", which must never
    /// render as "no agent sessions running".
    pub ok: bool,
    /// Worktrees laid out for an agent (`.<agent>/worktrees/<slug>`), counted
    /// by layout. Not a count of running sessions — see `live` for those.
    pub sessions: u32,
    pub kinds: Vec<AgentKindCount>,
    /// Running sessions per worktree path, from each agent's own registry.
    pub live: LiveSessionFacet,
    /// True when these numbers came from a capped sample of the repository's
    /// worktrees rather than all of them.
    ///
    /// `sessions` is then a **floor**, and a kind whose only worktrees fall
    /// past the cap is missing from `kinds` entirely — not undercounted,
    /// absent. Without this field a repository past
    /// [`MAX_SNAPSHOT_WORKTREES`] reported its capped count as an exact one,
    /// which is the same "a bounded sample rendered as complete coverage"
    /// mistake the rest of this module exists to avoid.
    pub truncated: bool,
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
    /// Symbol classification for this path. Populated only for the row the
    /// caller asked about, and only when two or more worktrees hold it, after
    /// the porcelain scan — never inside that scan.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity: Option<EntityCollisionVerdict>,
}

/// One worktree with two or more live agent sessions in it, and the files
/// dirty there. Those sessions share one index and one working tree, so every
/// one of these files is open to both of them; which session dirtied which
/// is not something git records.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SharedWorktree {
    pub path: String,
    pub branch: Option<String>,
    pub sessions: Vec<LiveSession>,
    /// Dirty paths, repo-relative. Empty with `scanned: false` is "not
    /// looked at", not "nothing dirty".
    pub files: Vec<String>,
    /// Whether this worktree's dirty files were read.
    pub scanned: bool,
    /// True when `files` stopped at the per-worktree path cap.
    pub truncated: bool,
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
    /// Dirty files in worktrees that two or more live sessions share — the
    /// collision `overlapping_files` cannot see, because it compares
    /// worktrees and these sessions are in one.
    pub shared_worktree_files: u32,
    pub shared_worktrees: Vec<SharedWorktree>,
    /// Whether every agent kind's live sessions could be observed. False
    /// means a worktree shared with an unobservable session is missing from
    /// `shared_worktrees`, so its emptiness is not a clean result.
    pub sessions_ok: bool,
    /// Why `sessions_ok` is false: each kind that could not be observed.
    pub sessions_error: String,
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
    /// How current the ledger data this context read is (the bound task comes
    /// from it): `ledger_freshness.as_of` is the `ts_utc` of the newest row
    /// that could be seen, `stale` whether anything shows a recorder ran
    /// recently. Unavailable (stale, with the reason) when the ledger could not
    /// be read or was never consulted.
    #[serde(default)]
    pub ledger_freshness: crate::ledger::Freshness,
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
    /// The bare `gitpulse-mcp` the plugin manifest names resolves on `PATH`.
    pub binary_on_path: bool,
    /// `gitpulse-hook`, resolved the same way as the server above.
    pub hook_found: bool,
    pub hook_path: String,
    pub hook_error: String,
    pub hook_on_path: bool,
    pub plugin_found: bool,
    pub plugin_path: String,
    pub plugin_error: String,
    pub plugin_manifest_json: String,
    /// The portable manifest, naming `gitpulse-mcp` bare.
    pub plugin_mcp_json: String,
    /// The MCP manifest to install: `command` is the resolved absolute path.
    /// Empty when the server was not found.
    pub mcp_config_json: String,
    /// The hooks manifest to install: absolute path, deciding hooks refusing
    /// when the binary is missing. Empty when the hook binary was not found.
    pub hooks_config_json: String,
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
fn unknown_agents(error: &str) -> AgentSummary {
    AgentSummary {
        ok: false,
        sessions: 0,
        kinds: Vec::new(),
        live: live_sessions::unattributed(error),
        // Not "we saw everything there was": `ok: false` already says the
        // listing never ran, and claiming a complete sample on top of that
        // would be a second false statement rather than a safer default.
        truncated: false,
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
    let error = error.into();
    CollisionRisk {
        ok: false,
        error: error.clone(),
        overlapping_files: 0,
        worktrees_involved: 0,
        scanned_worktrees: 0,
        unscanned_worktrees: 0,
        failed_worktrees: 0,
        truncated: false,
        items: Vec::new(),
        shared_worktree_files: 0,
        shared_worktrees: Vec::new(),
        sessions_ok: false,
        sessions_error: error,
    }
}

/// Wire fields a facet carries when it failed because the spawn gate deferred
/// its Git read: `deferred: true`, and how long to wait before asking again.
pub const DEFERRED_FIELD: &str = "deferred";
pub const RETRY_AFTER_FIELD: &str = "retry_after_ms";

/// The soonest a deferred read is worth repeating: one refill interval of the
/// spawn rate budget (the time it takes to mint one token), rounded up. The
/// budget refills at a fixed rate, so this is derived from it rather than
/// guessed; it is a floor, not a promise that the next attempt is admitted.
pub fn deferral_retry_after_ms() -> u64 {
    let (_burst, per_sec) = crate::engine::git_cli::production_spawn_rate();
    1000u64.div_ceil(u64::from(per_sec.max(1)))
}

/// One `; `-separated piece of an error: the gate's refusal, or one that
/// [`sessions_error`] prefixed with the agent kind it concerns (`claude: `).
/// Either way the refusal itself must start the piece, so a real error that
/// merely quotes the phrase is not one.
fn is_deferral_segment(segment: &str) -> bool {
    let refusal = match segment.split_once(": ") {
        Some((kind, rest))
            if !kind.is_empty()
                && kind
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_')) =>
        {
            rest
        }
        _ => segment,
    };
    crate::engine::git_cli::is_deferred_under_load(refusal)
        || crate::engine::git_cli::is_deferred_under_load(segment)
}

/// True when `value` is a string the spawn gate's deferral wording (or the
/// ledger's typed `deferred_worktree` code) produced. One predicate for the
/// whole seam: [`crate::engine::git_cli::is_deferred_under_load`].
fn is_deferral_text(value: &serde_json::Value) -> bool {
    value.as_str().is_some_and(|text| {
        text == crate::ledger::bindings::DEFERRED_WORKTREE
            || !text.is_empty() && text.split("; ").all(is_deferral_segment)
    })
}

/// Adds `deferred: true` and `retry_after_ms` to every failed facet of a
/// serialized insights payload whose failure was a deferral, and returns
/// whether any was found.
///
/// A deferral is the gate declining to start Git under load: nothing about the
/// repository was examined, and asking again may answer. The payload already
/// says `ok: false` with the gate's sentence; this adds the typed form so a
/// consumer branches on a field instead of a substring. Additive only: a facet
/// that ran, or that failed for any other reason, is left exactly as it was,
/// and a facet that failed for a *real* reason is never marked.
///
/// An object is marked only when its own failure is a deferral (an `error`
/// field the gate wrote, or the `deferred_worktree` code) and it does not say
/// `ok: true`. A parent that states a different cause is never marked on a
/// descendant's account; one that states none (`agents`, whose cause lives in
/// `live`) is. The return value is true when anything beneath, or here, was
/// marked.
pub fn mark_deferrals(payload: &mut serde_json::Value) -> bool {
    use serde_json::Value;
    match payload {
        Value::Object(map) => {
            let mut found = false;
            for child in map.values_mut() {
                found |= mark_deferrals(child);
            }
            // A facet is deferred only by its OWN failure: one that says it
            // succeeded is never marked, and a real failure is not made
            // retryable because something beneath it deferred.
            let own = map.get("ok") != Some(&Value::Bool(true))
                && (map
                    .iter()
                    .any(|(key, value)| key.ends_with("error") && is_deferral_text(value))
                    || map.get("error_code").is_some_and(is_deferral_text));
            // A failed facet that states no cause of its own (`agents` keeps
            // the cause only in `live`) speaks through the descendant that
            // does. One that states a different cause is never marked.
            let states_cause = map.iter().any(|(key, value)| {
                key.ends_with("error") && value.as_str().is_some_and(|text| !text.is_empty())
            });
            let inherits = found && map.get("ok") == Some(&Value::Bool(false)) && !states_cause;
            if own || inherits {
                map.insert(DEFERRED_FIELD.to_string(), Value::Bool(true));
                map.insert(
                    RETRY_AFTER_FIELD.to_string(),
                    Value::from(deferral_retry_after_ms()),
                );
                found = true;
            }
            found
        }
        Value::Array(items) => items
            .iter_mut()
            .fold(false, |found, item| mark_deferrals(item) | found),
        _ => false,
    }
}

/// Every kind that could not be fully observed, and why, as one sentence.
fn sessions_error(live: &LiveSessionFacet) -> String {
    live.kinds
        .iter()
        .filter_map(|k| {
            if !k.ok {
                Some(format!("{}: {}", k.kind, k.error))
            } else if k.truncated {
                Some(format!("{}: registry truncated", k.kind))
            } else if k.unverified > 0 {
                Some(format!(
                    "{}: {} session(s) unverified",
                    k.kind, k.unverified
                ))
            } else {
                None
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
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
/// The agent kind and session slug for a worktree path, both empty when it is
/// not an agent worktree.
///
/// The one place a worktree row or a collision party gets its agent labels.
/// One scan for both: asking twice would re-walk the path for every worktree
/// of every repository in a fleet sweep, and — worse — would let the kind and
/// the slug come from two different matches.
fn agent_labels(path: &str) -> (String, String) {
    agent_layout(path)
        .map(|layout| (layout.kind, layout.slug))
        .unwrap_or_default()
}

/// The one builder of [`WorktreeSummary`]. A checkout the listing did not
/// describe comes through here too, as [`unlisted_info`].
fn summarise_worktree(
    info: &WorktreeInfo,
    operation_kind: String,
    operation_ok: bool,
) -> WorktreeSummary {
    let (agent_kind, session_slug) = agent_labels(&info.path);
    WorktreeSummary {
        path: info.path.clone(),
        name: info.name.clone(),
        branch: info.branch.clone(),
        is_detached: info.is_detached,
        is_main: info.is_main,
        is_bare: info.is_bare,
        dirty_files: info.dirty_files.map(|n| n as u32),
        agent_kind,
        session_slug,
        operation_kind,
        operation_ok,
        head: Some(info.head.clone()).filter(|head| !head.is_empty()),
        ahead: None,
        behind: None,
        commits_since_base: None,
        divergence_note: None,
    }
}

/// Fills each row's `ahead`, `behind` and `commits_since_base` from its HEAD
/// commit, detached or not, against the repository's default base. The one
/// place those are set: [`summarise_worktree`] leaves them unmeasured so the
/// fleet and context rows, which never pay for the read, cannot read as zero.
fn fill_head_divergence(
    repo_path: &str,
    items: &mut [WorktreeSummary],
    clock: &worktree::ScanDeadline,
) {
    let heads: Vec<String> = items
        .iter()
        .filter(|item| !item.is_bare)
        .filter_map(|item| item.head.clone())
        .collect();
    if heads.is_empty() {
        return;
    }
    let asked: Vec<&str> = heads.iter().map(String::as_str).collect();
    let measured = match validate_repo(repo_path) {
        Ok(repo) => worktree::measure_head_divergence(&repo, &asked, clock),
        Err(error) => worktree::HeadDivergences {
            unread: Some(error),
            ..Default::default()
        },
    };
    for item in items.iter_mut().filter(|item| !item.is_bare) {
        let Some(head) = item.head.as_deref() else {
            continue;
        };
        match measured.by_head.get(head) {
            Some(found) => {
                item.ahead = Some(found.ahead);
                item.behind = Some(found.behind);
                item.commits_since_base = Some(found.ahead);
                if measured.shallow {
                    item.divergence_note =
                        Some("shallow: counts are bounded by the clone depth".to_string());
                }
            }
            None => {
                item.divergence_note = Some(
                    measured
                        .unread
                        .clone()
                        .unwrap_or_else(|| "unmeasured: this HEAD was not measured".to_string()),
                );
            }
        }
    }
}

/// Rolls per-worktree agent labels into counts.
///
/// `ok` and `truncated` both travel in from the caller because they are facts
/// about the listing these items came from, which this function never sees.
/// An empty slice from a failed listing and an empty slice from a repository
/// with no agents are the same input and must not be the same answer; and a
/// slice that is all of a repository's worktrees is indistinguishable here
/// from the first 64 of 300, which is why the caller has to say which it is.
fn agent_summary(
    ok: bool,
    truncated: bool,
    items: &[WorktreeSummary],
    live: LiveSessionFacet,
) -> AgentSummary {
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
        live,
        truncated,
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
    // The kind alone: this row reports which operation is parked, and the full
    // report's branch, incoming commit and conflict list cost a child each.
    match repo_op::detect_kind(&repo) {
        Ok(Some(kind)) => (format!("{kind:?}"), true),
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
/// The expensive half of this call is one `git status` per worktree, up to
/// [`MAX_SNAPSHOT_WORKTREES`] of them, which feeds both the worktree rows and
/// the cross-worktree collision scan. Git's own per-spawn timeout is 90 s with
/// no ceiling above it, so a pathological tree could hold the single-threaded
/// MCP server — which answers nothing else meanwhile — for minutes. One
/// [`worktree::ScanDeadline`] is handed to that read: no wave of statuses
/// starts after it, one already running is given only the time left, and the
/// worktrees it did not reach are reported unknown in the rows and unscanned in
/// the collision facet (`ok: false`, with the reason), exactly as
/// [`FLEET_DEADLINE`] does for a workspace sweep.
///
/// The fixed-cost stages — one worktree listing, one `git status` of the main
/// checkout, two local store reads — always run. Cutting them would leave a
/// snapshot with nothing in it, and their cost does not grow with the number of
/// worktrees. The parked-operation probe reads control files and starts no
/// process, so it is bounded by the worktree count; the clock still ends it
/// early.
pub(crate) const SNAPSHOT_DEADLINE: Duration = Duration::from_secs(10);

/// One-shot read of everything an agent needs to see the repository as the
/// Work view does: worktrees, agent sessions, dirty files, collisions, ledger
/// and code graph. Individual facets fail independently.
pub fn snapshot(repo_path: &str) -> InsightsSnapshot {
    snapshot_within(repo_path, SNAPSHOT_DEADLINE, MAX_SNAPSHOT_WORKTREES)
}

/// [`snapshot`] with the deadline and the worktree cap as arguments, so both
/// partial-snapshot paths are reachable in a test without a pathological
/// repository. Sixty-five real worktrees to prove one boolean would be a slow,
/// load-sensitive test of git rather than of this function.
fn snapshot_within(repo_path: &str, deadline: Duration, max_worktrees: usize) -> InsightsSnapshot {
    snapshot_observed(
        repo_path,
        deadline,
        max_worktrees,
        &live_sessions::observe(),
    )
}

/// [`snapshot_within`] over a live-session observation the caller made, so a
/// test can place sessions without running agents.
fn snapshot_observed(
    repo_path: &str,
    deadline: Duration,
    max_worktrees: usize,
    observation: &live_sessions::Observation,
) -> InsightsSnapshot {
    let started = Instant::now();
    // One clock for the whole pass, handed to every stage that can run long.
    let clock = worktree::ScanDeadline::after(started, deadline);
    // The listing alone: one process. The per-worktree status read is made
    // below, once, for the worktree rows and the collision scan together.
    let mut listed = worktree::list_worktrees_lite(repo_path);
    let mut deadline_expired = false;
    // Attributed over the whole listing, not the capped summaries: a session
    // in the 65th worktree is still a session in this repository.
    let live = match &listed {
        Ok(list) => live_in(observation, list),
        Err(error) => live_sessions::unattributed(error),
    };

    let changes = match GitReader::get_status(repo_path) {
        // The facet carries counts, not rows, and the whole status is already
        // in memory: count every file. Counting only the first
        // `MAX_ACTIVE_FILES` made every total a floor on a busy tree.
        Ok(files) => changes_from_status(&files, false),
        Err(error) => empty_changes(error),
    };

    // The only `git status` of each worktree this snapshot makes, and the last
    // git read it makes: the rows' dirty counts and the collision scan's paths
    // come out of it together, so the two facets describe the same moment, and
    // an edit made before a worktree is read is in both. The read is bounded by
    // the same clock as everything else; a worktree it did not reach stays
    // unknown in both facets rather than reading as clean in either.
    if let Ok(list) = &mut listed {
        if worktree::read_dirty(list, &clock) > 0 {
            deadline_expired = true;
        }
    }

    let (worktrees, agents) = match &listed {
        Ok(list) => {
            let truncated = list.len() > max_worktrees;
            let items: Vec<WorktreeSummary> = list
                .iter()
                .take(max_worktrees)
                .map(|info| {
                    // `detect_operation` reads control files and starts no
                    // process, so this stage's cost is bounded by the number of
                    // worktrees; the clock still ends it early on a host too
                    // slow to read even those.
                    if clock.expired() {
                        deadline_expired = true;
                        return summarise_worktree(info, String::new(), false);
                    }
                    let (kind, probed) = detect_operation(&info.path);
                    summarise_worktree(info, kind, probed)
                })
                .collect();
            let mut facet = worktree_facet(list.len() as u32, truncated, items);
            fill_head_divergence(repo_path, &mut facet.items, &clock);
            // The same `truncated` the facet carries: these counts are rolled
            // from `facet.items`, which is the capped list, so a repository
            // past the cap must not report its floor as an exact count.
            let agents = agent_summary(true, truncated, &facet.items, live.clone());
            (facet, agents)
        }
        Err(error) => (empty_worktrees(error.clone()), unknown_agents(error)),
    };

    let collisions = match &listed {
        Ok(list) => {
            if clock.expired() {
                deadline_expired = true;
            }
            // Past the clock the scan spawns nothing: it uses the statuses
            // already read and counts every worktree it has none for as
            // unscanned, with `ok: false` and the reason — never
            // `overlapping_files: 0` for a scan that did not look.
            collision_from_list(list, &live, Some(&clock))
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
            recording_state: crate::ledger::STATE_UNKNOWN.to_string(),
            path: String::new(),
            dropped: 0,
            dropped_scope: crate::ledger::DROPPED_SCOPE.to_string(),
            catch_up_failures: 0,
            history_only_bindings: 0,
            error: error.to_string(),
            error_code: error.code.to_string(),
            freshness: crate::ledger::Freshness::unavailable(error.to_string()),
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
/// Open tabs are no longer capped near this (the workspace holds up to 1000,
/// with live watches pooled separately), so a large workspace can ask for more.
/// This bounds one sweep's git work and IPC payload, not what is open: anything
/// past it is dropped and reported through `truncated` rather than silently
/// ignored, and the 10-second deadline below bounds it again.
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
    ///
    /// What true means depends on the git. From 2.37 (`--since-as-filter`)
    /// only in-window commits count toward the cap, so true means the window
    /// itself hit it. Below 2.37 the walk has no date bound, history older
    /// than the window spends the cap, and true means "the count bound was
    /// hit; the window may be cut". It can be true for a long history whose
    /// whole window was seen, and is never false for a window that was cut.
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
        // than the window. A row older than the window is dropped from every
        // count rather than folded into the oldest bucket, so `commits` and
        // `daily` always describe the same population. A commit stamped in the
        // future by a skewed clock is the opposite case: it is counted, in the
        // newest bucket (`age <= 0` below).
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
    fleet_commit_stats_walking(
        repo,
        anchor_epoch,
        window_days,
        crate::engine::git_cli::git_supports(crate::engine::git_cli::SINCE_AS_FILTER),
    )
}

/// The `git log` arguments of the commit-rhythm walk. One over the cap, so
/// "we stopped early" is observable rather than indistinguishable from a
/// repository that happens to have exactly the cap.
///
/// With `since_as_filter` (git 2.37) only commits inside the window count
/// toward the cap, so `truncated` means the window itself hit the cap. Plain
/// `--since` is not used: it stops walking at the first old commit, so one
/// commit with a skewed clock at the tip hid everything behind it. Below 2.37
/// the walk is bounded by count alone and [`bucket_commit_log`] applies the
/// window to the committer dates git printed; history older than the window
/// spends the cap, so `truncated` there means "the window may be cut".
fn fleet_log_args(since_as_filter: bool, since_epoch: i64) -> Vec<String> {
    let mut args: Vec<String> = ["log", "--no-show-signature"].map(String::from).to_vec();
    if since_as_filter {
        args.push(format!("--since-as-filter={since_epoch}"));
    }
    args.extend(
        [
            "-n",
            &(MAX_FLEET_COMMITS + 1).to_string(),
            "--format=%ct%x1f%ae",
            "--",
        ]
        .map(String::from),
    );
    args
}

fn fleet_commit_stats_walking(
    repo: &Path,
    anchor_epoch: i64,
    window_days: u32,
    since_as_filter: bool,
) -> Result<FleetCommitStats, String> {
    let since_epoch = anchor_epoch.saturating_sub(i64::from(window_days) * BUCKET_SECONDS);
    let args = fleet_log_args(since_as_filter, since_epoch);
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    let text = match git_text(repo, &argv) {
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
    let agents = unknown_agents(&error);
    FleetRepoFacet {
        repo_path: repo_path.to_string(),
        ok: false,
        error,
        worktrees_ok: false,
        worktrees_error: String::new(),
        worktrees: 0,
        agents,
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
fn fleet_facet(
    repo_path: &str,
    anchor_epoch: i64,
    window_days: u32,
    observation: &live_sessions::Observation,
) -> FleetRepoFacet {
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
                    // `list_worktrees_lite` returns every worktree — the cost
                    // this facet avoids is the per-worktree `git status`, not
                    // the listing — so this sample is complete.
                    agent_summary(true, false, &items, live_in(observation, &list)),
                )
            }
            Err(error) => {
                let agents = unknown_agents(&error);
                (false, error, 0, agents)
            }
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

    // One look at the running agents for the whole sweep; each repository
    // only attributes it to its own worktrees.
    let observation = live_sessions::observe();
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
            fleet_facet(path, anchor_epoch, window_days, &observation)
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

/// Live sessions attributed to the worktrees of one listing.
fn live_in(observation: &live_sessions::Observation, list: &[WorktreeInfo]) -> LiveSessionFacet {
    let paths: Vec<String> = list.iter().map(|w| w.path.clone()).collect();
    live_sessions::attribute(observation, &paths)
}

/// Worktrees two or more live sessions are working in.
fn shared_paths(live: &LiveSessionFacet) -> HashSet<&str> {
    live.worktrees
        .iter()
        .filter(|w| w.sessions.len() > 1)
        .map(|w| w.path.as_str())
        .collect()
}

/// Overlapping dirty files across `list`'s worktrees, and the dirty files of
/// any worktree two live sessions share.
///
/// A worktree whose status the listing already read ([`StatusRead`]) costs
/// nothing here: its paths are used as read, once, rather than read again. One
/// that was never read is read now, on rayon's pool but in the caller's
/// admission class, for at most [`MAX_COLLISION_SCANS`] of them. With a
/// `deadline`, none is read once it has passed: those worktrees are counted as
/// unscanned and the facet says `ok: false` and why.
fn collision_from_list(
    list: &[WorktreeInfo],
    live: &LiveSessionFacet,
    deadline: Option<&worktree::ScanDeadline>,
) -> CollisionRisk {
    collision_from_list_reading(
        list,
        live,
        deadline,
        &changed_paths_within,
        CollisionScope::default(),
    )
}

/// What a collision read is asked about, beyond "every file two worktrees
/// share". Both narrow the answer *before* the row cap, so the one row a caller
/// wants cannot be cut for being the thirty-third.
#[derive(Debug, Clone, Copy, Default)]
struct CollisionScope<'a> {
    /// A path about to be edited. Its row is kept when ONE worktree holds it
    /// dirty: the editor is not dirty there yet, and "no overlap" would be the
    /// wrong answer to "is anyone else on this file".
    focus: Option<&'a str>,
    /// Keep only rows this worktree is a party to.
    involving: Option<&'a str>,
}

/// One worktree's changed paths, and whether the list was cut at its cap: the
/// answer of [`changed_paths_within`], or of a memo standing in for it.
type PathsRead = Result<(Vec<String>, bool), String>;

/// [`collision_from_list`] reading each unread worktree through `read`.
///
/// The only other caller is the memoized scan the app answers hooks with
/// ([`collision_memo`]), which is why the reader is a parameter: the cap, the
/// shared-worktree ordering and the unscanned accounting stay in one place.
fn collision_from_list_reading(
    list: &[WorktreeInfo],
    live: &LiveSessionFacet,
    deadline: Option<&worktree::ScanDeadline>,
    read: &(dyn Fn(&str, Option<&worktree::ScanDeadline>) -> PathsRead + Sync),
    scope: CollisionScope<'_>,
) -> CollisionRisk {
    let shared = shared_paths(live);
    // A worktree that two sessions share is scanned ahead of the cap: it is
    // the one place a collision is already known to be possible.
    let (first, rest): (Vec<&WorktreeInfo>, Vec<&WorktreeInfo>) = list
        .iter()
        .filter(|w| !w.is_bare)
        .partition(|w| shared.contains(w.path.as_str()));

    type Read = PathsRead;
    let mut scans: Vec<(&WorktreeInfo, Option<Read>)> = Vec::new();
    let mut pending: Vec<usize> = Vec::new();
    let mut fresh_reads_left = MAX_COLLISION_SCANS;
    let mut over_the_cap = 0usize;
    let mut cut_off = 0usize;
    for wt in first.into_iter().chain(rest) {
        match &wt.status_read {
            StatusRead::Read { paths, truncated } => {
                scans.push((wt, Some(Ok((paths.clone(), *truncated)))));
            }
            // Already tried inside this pass; trying again would repeat the
            // refusal and spend the time twice.
            StatusRead::Failed(reason) => scans.push((wt, Some(Err(reason.clone())))),
            StatusRead::NotRead if deadline.is_some_and(worktree::ScanDeadline::expired) => {
                cut_off += 1;
            }
            StatusRead::NotRead if fresh_reads_left > 0 => {
                fresh_reads_left -= 1;
                pending.push(scans.len());
                scans.push((wt, None));
            }
            StatusRead::NotRead => over_the_cap += 1,
        }
    }
    let unscanned = over_the_cap + cut_off;

    // The pool's threads start in the default admission class, so the class
    // the caller asked under travels in explicitly.
    let class = current_admission();
    let to_read: Vec<&WorktreeInfo> = pending.iter().map(|&at| scans[at].0).collect();
    let fresh: Vec<Read> = to_read
        .into_par_iter()
        .map(|wt| with_admission(class, || read(&wt.path, deadline)))
        .collect();
    for (at, read) in pending.into_iter().zip(fresh) {
        scans[at].1 = Some(read);
    }

    let mut by_path: HashMap<String, Vec<CollisionParty>> = HashMap::new();
    let mut scan_error = String::new();
    let mut scanned = 0u32;
    let mut failed = 0u32;
    let mut paths_truncated = false;
    let mut shared_scans: HashMap<&str, (Vec<String>, bool)> = HashMap::new();
    for (wt, result) in scans {
        match result.unwrap_or_else(|| Err("the worktree was not read".to_string())) {
            Ok((paths, truncated)) => {
                scanned += 1;
                paths_truncated |= truncated;
                if shared.contains(wt.path.as_str()) {
                    shared_scans.insert(wt.path.as_str(), (paths.clone(), truncated));
                }
                let party = CollisionParty {
                    path: wt.path.clone(),
                    branch: wt.branch.clone(),
                    agent_kind: agent_labels(&wt.path).0,
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

    if cut_off > 0 {
        let reason =
            format!("{cut_off} worktree(s) were not read before the snapshot ran out of time");
        scan_error = if scan_error.is_empty() {
            reason
        } else {
            format!("{scan_error}; {reason}")
        };
    }

    let mut items: Vec<CollisionItem> = by_path
        .into_iter()
        .filter(|(path, parties)| parties.len() > 1 || scope.focus == Some(path.as_str()))
        .filter(|(_, parties)| {
            scope
                .involving
                .is_none_or(|target| parties.iter().any(|party| same_path(&party.path, target)))
        })
        .map(|(path, worktrees)| CollisionItem {
            path,
            worktrees,
            entity: None,
        })
        .collect();
    items.sort_by(|a, b| a.path.cmp(&b.path));
    // The focus row is not an overlap unless a second worktree holds it.
    let overlapping_files = items.iter().filter(|item| item.worktrees.len() > 1).count() as u32;
    let truncated =
        items.len() > MAX_COLLISION_ITEMS || paths_truncated || unscanned > 0 || failed > 0;
    if items.len() > MAX_COLLISION_ITEMS {
        // The row the caller asked about outranks the alphabet: moved to the
        // front, it survives the cut however many paths sort before it.
        if let Some(at) = items
            .iter()
            .position(|item| scope.focus == Some(item.path.as_str()))
            .filter(|&at| at >= MAX_COLLISION_ITEMS)
        {
            let focus = items.remove(at);
            items.insert(0, focus);
        }
        items.truncate(MAX_COLLISION_ITEMS);
    }
    let mut involved = std::collections::BTreeSet::new();
    for item in &items {
        for party in &item.worktrees {
            involved.insert(party.path.clone());
        }
    }

    let shared_worktrees: Vec<SharedWorktree> = live
        .worktrees
        .iter()
        .filter(|w| w.sessions.len() > 1)
        .map(|w| {
            // Absent from the scans: past the cap, or its `git status` failed.
            // Either way its files were not read, which `scanned` says.
            let scan = shared_scans.remove(w.path.as_str());
            let scanned = scan.is_some();
            let (files, truncated) = scan.unwrap_or_default();
            SharedWorktree {
                path: w.path.clone(),
                branch: list
                    .iter()
                    .find(|info| info.path == w.path)
                    .and_then(|info| info.branch.clone()),
                sessions: w.sessions.clone(),
                files,
                scanned,
                truncated,
            }
        })
        .collect();
    let shared_worktree_files = shared_worktrees.iter().map(|w| w.files.len() as u32).sum();

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
        //
        // The deadline does: a worktree skipped because the clock ran out is
        // one the caller asked about and did not get an answer for, not one
        // it declined to ask about.
        ok: failed == 0 && cut_off == 0,
        error: scan_error,
        overlapping_files,
        worktrees_involved: involved.len() as u32,
        scanned_worktrees: scanned,
        unscanned_worktrees: unscanned as u32,
        failed_worktrees: failed,
        truncated,
        items,
        shared_worktree_files,
        shared_worktrees,
        sessions_ok: live.ok,
        sessions_error: sessions_error(live),
    }
}

/// Who else holds `path` dirty, ahead of an edit of it.
///
/// The one owner of the pre-edit question, answered by what it asks rather than
/// by the repo-wide overlap list: the worktree listing is read once, then each
/// worktree answers one pathspec-limited `status -u` for `path` alone, in the
/// caller's admission class and under `deadline`. Any worktree holding it is a
/// row, including a lone one, because the editor is not dirty there yet. A
/// worktree that could not be read, or was not read before the deadline, makes
/// the answer partial (`ok: false`, `truncated`, and the counts): an empty
/// `items` is evidence of absence only when those say the scan was complete.
/// The symbol verdict is attached when two or more worktrees hold the file; a
/// caller that classifies the edit itself asks [`scan_holders_of_path`].
pub fn collision_for_path(
    repo_path: &str,
    path: &str,
    deadline: Option<&worktree::ScanDeadline>,
) -> CollisionRisk {
    entity_collision::enrich_item(scan_holders_of_path(repo_path, path, deadline), Some(path))
}

/// What one other worktree holds dirty, for [`dirty_elsewhere`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirtyElsewhereWorktree {
    pub path: String,
    pub branch: Option<String>,
    /// Dirty paths, repo-relative, cut at the per-worktree path cap.
    pub files: Vec<String>,
    /// Whether this worktree's status was read. `files` empty with `false` is
    /// "not looked at", never "nothing dirty".
    pub scanned: bool,
    /// Why it was not read; empty when it was.
    pub error: String,
    /// `files` stopped at the path cap.
    pub truncated: bool,
}

/// Every other worktree's dirty files, for a caller that has not chosen a file
/// yet. See [`dirty_elsewhere`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirtyElsewhere {
    /// Every other worktree was read in full.
    pub ok: bool,
    pub error: String,
    pub worktrees: Vec<DirtyElsewhereWorktree>,
    /// Worktrees past the scan cap, never read.
    pub unscanned_worktrees: u32,
    /// What this answer can establish, and what it cannot.
    pub establishes: String,
}

const DIRTY_ELSEWHERE_ESTABLISHES: &str = "Lists the uncommitted files of every other worktree, so you can see whether a file you mean to edit is held. It cannot know which file you will edit: a clean worktree has no overlap with anyone yet. Before editing a file, ask gitpulse_collision_risk with its path; a worktree with scanned false was not read and says nothing.";

/// The dirty files of every worktree of `repo_path` except `repo_path`'s own.
///
/// The pre-edit view for a worktree that is itself clean, where the overlap
/// list is empty by construction. Capped like the collision scan
/// ([`MAX_COLLISION_SCANS`] worktrees, [`worktree::MAX_CHANGED_PATHS`] paths
/// each) and read in the caller's admission class; a worktree that could not be
/// read is listed with its error, and makes `ok` false.
pub fn dirty_elsewhere(repo_path: &str) -> DirtyElsewhere {
    let failed = |error: String| DirtyElsewhere {
        ok: false,
        error,
        worktrees: Vec::new(),
        unscanned_worktrees: 0,
        establishes: DIRTY_ELSEWHERE_ESTABLISHES.to_string(),
    };
    let list = match worktree::list_worktrees_lite(repo_path) {
        Ok(list) => list,
        Err(error) => return failed(error),
    };
    let others: Vec<&WorktreeInfo> = list
        .iter()
        .filter(|w| !w.is_bare && !same_path(&w.path, repo_path))
        .collect();
    let unscanned = others.len().saturating_sub(MAX_COLLISION_SCANS);
    let class = current_admission();
    let worktrees: Vec<DirtyElsewhereWorktree> = others
        .into_iter()
        .take(MAX_COLLISION_SCANS)
        .collect::<Vec<_>>()
        .into_par_iter()
        .map(|wt| {
            let read = with_admission(class, || changed_paths_within(&wt.path, None));
            let (files, truncated, error) = match read {
                Ok((files, truncated)) => (files, truncated, String::new()),
                Err(error) => (Vec::new(), false, error),
            };
            DirtyElsewhereWorktree {
                path: wt.path.clone(),
                branch: wt.branch.clone(),
                scanned: error.is_empty(),
                files,
                error,
                truncated,
            }
        })
        .collect();
    let unread = worktrees.iter().filter(|w| !w.scanned).count();
    let error = worktrees
        .iter()
        .find(|w| !w.scanned)
        .map(|w| w.error.clone())
        .unwrap_or_default();
    DirtyElsewhere {
        ok: unread == 0 && unscanned == 0,
        error,
        worktrees,
        unscanned_worktrees: unscanned as u32,
        establishes: DIRTY_ELSEWHERE_ESTABLISHES.to_string(),
    }
}

/// [`collision_for_path`] as the `gitpulse_collision_risk {path}` answer: the
/// one row for `path` (a lone holder included) and the scan's coverage counts,
/// and nothing else. The shared-worktree list is a repo-wide fact the
/// path-limited read did not gather, so it is left empty rather than reported
/// from a scan that only looked at one file.
pub fn collision_row_for_path(repo_path: &str, path: &str) -> CollisionRisk {
    let mut risk = collision_for_path(repo_path, path, None);
    risk.shared_worktrees.clear();
    risk.shared_worktree_files = 0;
    risk
}

/// [`collision_for_path`] without the symbol verdict: who holds `path`, and how
/// much of the family was read to say so.
///
/// The bound is [`MAX_COLLISION_SCANS`] status reads, one per worktree, the
/// same cap as the repo-wide scan: a family past it has its tail counted in
/// `unscanned_worktrees` and the answer is partial, never clean.
pub fn scan_holders_of_path(
    repo_path: &str,
    path: &str,
    deadline: Option<&worktree::ScanDeadline>,
) -> CollisionRisk {
    scan_holders_observed(repo_path, path, &live_sessions::observe(), deadline)
}

fn scan_holders_observed(
    repo_path: &str,
    path: &str,
    observation: &live_sessions::Observation,
    deadline: Option<&worktree::ScanDeadline>,
) -> CollisionRisk {
    match worktree::list_worktrees_lite(repo_path) {
        Ok(list) => {
            let read = |worktree: &str, deadline: Option<&worktree::ScanDeadline>| -> PathsRead {
                worktree::changed_paths_only(worktree, path, deadline)
            };
            collision_from_list_reading(
                &list,
                &live_in(observation, &list),
                deadline,
                &read,
                CollisionScope {
                    focus: Some(path),
                    involving: None,
                },
            )
        }
        Err(error) => empty_collisions(error),
    }
}

/// Overlapping dirty files across worktrees of `repo_path`, and dirty files
/// in any worktree two live agent sessions share.
///
/// The repo-wide view: porcelain paths only, no symbol verdict. A caller that
/// is about to edit one file asks [`collision_for_path`] instead.
pub fn collision_risk(repo_path: &str) -> CollisionRisk {
    collision_risk_observed(repo_path, &live_sessions::observe())
}

/// [`collision_risk`] over a live-session observation the caller made.
fn collision_risk_observed(
    repo_path: &str,
    observation: &live_sessions::Observation,
) -> CollisionRisk {
    match worktree::list_worktrees_lite(repo_path) {
        Ok(list) => collision_from_list(&list, &live_in(observation, &list), None),
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
pub(crate) fn same_path(left: impl AsRef<Path>, right: impl AsRef<Path>) -> bool {
    let (left, right) = (left.as_ref(), right.as_ref());
    if left == right {
        return true;
    }
    match (std::fs::canonicalize(left), std::fs::canonicalize(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
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
            // Totals describe the whole worktree, like `total`; only the row
            // list is capped. Counting the rows kept understated them.
            let counts = count_statuses(&files);
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
/// anywhere. Built by [`summarise_worktree`], so this row cannot drift from a
/// listed one.
fn unlisted_worktree(target: &str, operation_kind: String, operation_ok: bool) -> WorktreeSummary {
    summarise_worktree(&unlisted_info(target), operation_kind, operation_ok)
}

/// A listing entry for a checkout git did not describe: its path and
/// directory name, and nothing git would have supplied.
fn unlisted_info(target: &str) -> WorktreeInfo {
    WorktreeInfo {
        path: target.to_string(),
        name: Path::new(target)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| target.to_string()),
        head: String::new(),
        branch: None,
        is_bare: false,
        is_detached: false,
        is_main: false,
        is_locked: false,
        is_prunable: false,
        dirty_files: None,
        diff_stat: None,
        main_divergence: None,
        active_routes: Vec::new(),
        scan_note: None,
        status_read: worktree::StatusRead::NotRead,
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
        ledger_freshness: crate::ledger::Freshness::unavailable(
            "the repository was not resolved, so the ledger was not read",
        ),
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
                .any(|party| same_path(&party.path, target))
        })
        .collect();
    let mut involved = std::collections::BTreeSet::new();
    for item in &items {
        for party in &item.worktrees {
            involved.insert(party.path.clone());
        }
    }
    let shared_worktrees: Vec<SharedWorktree> = risk
        .shared_worktrees
        .into_iter()
        .filter(|w| same_path(&w.path, target))
        .collect();
    CollisionRisk {
        overlapping_files: items.len() as u32,
        worktrees_involved: involved.len() as u32,
        items,
        shared_worktree_files: shared_worktrees.iter().map(|w| w.files.len() as u32).sum(),
        shared_worktrees,
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

    // Only the target's dirty count is read, so only the target is scanned:
    // a full listing scanned up to 32 worktrees to describe one.
    //
    // The listing is read once and also answers the collision question: the
    // rows are cut to this worktree's before the row cap, so one of its files
    // cannot be dropped for sorting after thirty-two others'.
    let listed = worktree::list_worktrees_lite(repo_path);
    let collisions = match &listed {
        Ok(list) => collisions_involving(
            collision_from_list_reading(
                list,
                &live_in(&live_sessions::observe(), list),
                None,
                &changed_paths_within,
                CollisionScope {
                    focus: None,
                    involving: Some(&target),
                },
            ),
            &target,
        ),
        Err(error) => empty_collisions(error.clone()),
    };
    let (worktree, worktree_ok, worktree_error) = match listed {
        Ok(list) => match list.into_iter().find(|w| same_path(&w.path, &target)) {
            Some(mut found) => {
                worktree::scan_dirty(&mut found);
                (
                    summarise_worktree(&found, operation_kind, operation_ok),
                    true,
                    String::new(),
                )
            }
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
        collisions,
        operation,
        operation_ok,
        operation_error,
        ledger_freshness: ledger_freshness(repo_path),
    }
}

/// The freshness of the ledger a read of `repo_path` consults: its anchor's,
/// or unavailable with the reason when the anchor cannot be named.
pub(crate) fn ledger_freshness(repo_path: &str) -> crate::ledger::Freshness {
    match crate::ledger::bindings::repository_address_readonly(repo_path) {
        Ok(address) => crate::ledger::freshness_readonly(&address.anchor),
        Err(error) => crate::ledger::Freshness::unavailable(error.to_string()),
    }
}

/// Where one of the app's bundled executables is, and whether a bare name
/// would reach it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct BinaryStatus {
    found: bool,
    path: String,
    error: String,
    /// The bare command name resolves on `PATH`. The plugin's own manifests
    /// name it bare, so with an empty `PATH` they cannot start it even when
    /// `found`.
    on_path: bool,
}

/// `gitpulse-mcp`, or the target-triple build Tauri names it (`gitpulse-mcp-<triple>`).
fn looks_like_binary(name: &str, stem: &str) -> bool {
    let name = name.strip_suffix(".exe").unwrap_or(name);
    name == stem
        || name
            .strip_prefix(stem)
            .is_some_and(|rest| rest.starts_with('-'))
}

/// Whether `stem` is an executable file in one of the directories of `path_var`.
/// `None` and an empty value find nothing.
fn on_path_in(stem: &str, path_var: Option<std::ffi::OsString>) -> bool {
    let Some(path_var) = path_var else {
        return false;
    };
    std::env::split_paths(&path_var).any(|dir| {
        !dir.as_os_str().is_empty()
            && [stem.to_string(), format!("{stem}.exe")]
                .iter()
                .any(|name| dir.join(name).is_file())
    })
}

/// Resolves one bundled executable: an explicit `env_var` override, else this
/// process itself or a file beside it (the app bundle's `Contents/MacOS`).
fn resolve_bundled_binary(env_var: &str, stem: &str) -> BinaryStatus {
    let on_path = on_path_in(stem, std::env::var_os("PATH"));
    let missing = |error: String| BinaryStatus {
        error,
        on_path,
        ..BinaryStatus::default()
    };
    let found = |path: String| BinaryStatus {
        found: true,
        path,
        error: String::new(),
        on_path,
    };
    if let Ok(explicit) = std::env::var(env_var) {
        if Path::new(&explicit).is_file() {
            return found(explicit);
        }
        return BinaryStatus {
            path: explicit,
            ..missing(format!("{env_var} does not point at a file"))
        };
    }
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => return missing(format!("could not resolve this process path: {e}")),
    };
    if looks_like_binary(
        exe.file_name().and_then(|n| n.to_str()).unwrap_or_default(),
        stem,
    ) && exe.is_file()
    {
        return found(exe.to_string_lossy().into_owned());
    }
    let Some(dir) = exe.parent() else {
        return missing(format!("{stem} is not next to this process"));
    };
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if path.is_file() && looks_like_binary(name, stem) {
                return found(path.to_string_lossy().into_owned());
            }
        }
    }
    missing(format!(
        "{stem} is not next to this app. Build it with `cargo build --bin {stem} --manifest-path src-tauri/Cargo.toml`, or set {env_var}."
    ))
}

/// The path of a helper executable that ships beside the app (the bundle's
/// `Contents/MacOS`), or why it could not be found. `env_var` overrides, as it
/// does for the copy-paste setup above.
pub(crate) fn bundled_binary(env_var: &str, stem: &str) -> Result<std::path::PathBuf, String> {
    let status = resolve_bundled_binary(env_var, stem);
    if status.found {
        Ok(status.path.into())
    } else {
        Err(status.error)
    }
}

/// Single-quotes `text` for a POSIX shell.
fn sh_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// The plugin's MCP manifest with `gitpulse` started by `binary`'s absolute
/// path. A bare `gitpulse-mcp` only works when something put it on `PATH`, and
/// a released app does not.
fn mcp_config_with(template: &str, binary: &str) -> Result<String, String> {
    let mut value: serde_json::Value =
        serde_json::from_str(template).map_err(|e| format!("the MCP manifest is not JSON: {e}"))?;
    let server = value
        .pointer_mut("/mcpServers/gitpulse")
        .and_then(|s| s.as_object_mut())
        .ok_or("the MCP manifest has no gitpulse server")?;
    server.insert(
        "command".into(),
        serde_json::Value::String(binary.to_string()),
    );
    serde_json::to_string_pretty(&value).map_err(|e| e.to_string())
}

/// The plugin's hooks manifest with every `gitpulse-hook` spelled as the
/// absolute path of `binary`.
///
/// The hooks that decide something (`collision-guard`, `command-gate`) also
/// refuse when the binary is missing: a host treats a hook that cannot start as
/// a non-blocking error and runs the call anyway, so the bare command left the
/// gate silently off. Exit status 2 is the host's "block", with the reason on
/// stderr. `session-brief` cannot block anything, so it tells the session
/// instead. Reports (`notify`) stay as they are: losing one loses a
/// notification, never a decision. On Windows there is no `sh` to wrap with, so
/// every command is the quoted path alone.
fn hooks_config_with(template: &str, binary: &str) -> Result<String, String> {
    let mut value: serde_json::Value = serde_json::from_str(template)
        .map_err(|e| format!("the hooks manifest is not JSON: {e}"))?;
    let events = value
        .get_mut("hooks")
        .and_then(|h| h.as_object_mut())
        .ok_or("the hooks manifest has no hooks")?;
    let quoted = sh_quote(binary);
    for groups in events.values_mut() {
        for group in groups.as_array_mut().into_iter().flatten() {
            for handler in group
                .get_mut("hooks")
                .and_then(|h| h.as_array_mut())
                .into_iter()
                .flatten()
            {
                let Some(command) = handler.get("command").and_then(|c| c.as_str()) else {
                    continue;
                };
                let Some(args) = command.strip_prefix("gitpulse-hook ") else {
                    return Err(format!("unexpected hook command: {command}"));
                };
                let wrapped = if cfg!(windows) {
                    format!("\"{binary}\" {args}")
                } else {
                    match args.split_whitespace().next() {
                        Some("collision-guard" | "command-gate") => format!(
                            "[ -x {quoted} ] || {{ echo {} >&2; exit 2; }}; exec {quoted} {args}",
                            sh_quote(&format!(
                                "GitPulse blocked this call: its safety hook {binary} cannot run. Reinstall GitPulse or set the hook path in Settings."
                            ))
                        ),
                        Some("session-brief") => format!(
                            "[ -x {quoted} ] || {{ printf '%s' {}; exit 0; }}; exec {quoted} {args}",
                            sh_quote(&serde_json::json!({
                                "hookSpecificOutput": {
                                    "hookEventName": "SessionStart",
                                    "additionalContext": format!(
                                        "WARNING: GitPulse's safety hooks cannot run ({binary} is missing), so the collision guard and command gate are OFF in this session."
                                    )
                                }
                            })
                            .to_string())
                        ),
                        _ => format!("{quoted} {args}"),
                    }
                };
                handler["command"] = serde_json::Value::String(wrapped);
            }
        }
    }
    serde_json::to_string_pretty(&value).map_err(|e| e.to_string())
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
    let mcp = resolve_bundled_binary("GITPULSE_MCP_PATH", "gitpulse-mcp");
    let hook = resolve_bundled_binary("GITPULSE_HOOK_PATH", "gitpulse-hook");
    let (plugin_found, plugin_path, mut plugin_error) = resolve_plugin_root();
    let (plugin_manifest_json, plugin_mcp_json, mcp_config_json, hooks_config_json) =
        if plugin_found {
            let root = Path::new(&plugin_path);
            // The first thing wrong with the package is the reason Settings
            // shows; an empty config with no reason is what this replaces.
            let mut first_error: Option<String> = None;
            let mut read = |relative: &str| {
                let path = root.join(relative);
                std::fs::read_to_string(&path).unwrap_or_else(|e| {
                    first_error.get_or_insert(format!("could not read {}: {e}", path.display()));
                    String::new()
                })
            };
            let manifest = read(".codex-plugin/plugin.json");
            let mcp_template = read(".mcp.json");
            let hooks_template = read("hooks/hooks.json");
            // What Settings copies is the command that will run, so it is built
            // from the path just resolved. Nothing is offered for a binary that
            // was not found: a copy naming a bare command is the bug. The
            // template is still checked against the bare name so a malformed
            // one is reported either way.
            let mut check = |built: Result<String, String>, offered: bool| match built {
                Ok(config) if offered => config,
                Ok(_) => String::new(),
                Err(e) => {
                    first_error.get_or_insert(e);
                    String::new()
                }
            };
            let mcp_config = check(
                mcp_config_with(
                    &mcp_template,
                    if mcp.found { &mcp.path } else { "gitpulse-mcp" },
                ),
                mcp.found,
            );
            let hooks_config = check(
                hooks_config_with(
                    &hooks_template,
                    if hook.found {
                        &hook.path
                    } else {
                        "gitpulse-hook"
                    },
                ),
                hook.found,
            );
            if let Some(error) = first_error {
                plugin_error = error;
            }
            (manifest, mcp_template, mcp_config, hooks_config)
        } else {
            (String::new(), String::new(), String::new(), String::new())
        };
    McpInfo {
        protocol_version: crate::mcp::PROTOCOL_VERSION.to_string(),
        server_name: crate::mcp::SERVER_NAME.to_string(),
        server_version: crate::mcp::server_version().to_string(),
        read_only: true,
        binary_found: mcp.found,
        binary_path: mcp.path,
        binary_error: mcp.error,
        binary_on_path: mcp.on_path,
        hook_found: hook.found,
        hook_path: hook.path,
        hook_error: hook.error,
        hook_on_path: hook.on_path,
        plugin_found,
        plugin_path,
        plugin_error,
        plugin_manifest_json,
        plugin_mcp_json,
        mcp_config_json,
        hooks_config_json,
        tools: crate::mcp::tool_catalog(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::procguard::LockedSpawn;

    /// Measurement, not a gate: the spawns one snapshot of `GP_MEASURE_REPO`
    /// makes, by argv, across every worktree directory.
    #[test]
    #[ignore = "measures a real repository named by GP_MEASURE_REPO"]
    fn measure_snapshot_spawns_on_a_real_repository() {
        let repo = std::env::var("GP_MEASURE_REPO").expect("GP_MEASURE_REPO");
        crate::test_support::trust_repo(std::path::Path::new(&repo));
        let dirs: Vec<std::path::PathBuf> = worktree::list_worktrees_lite(&repo)
            .expect("list")
            .iter()
            .map(|w| std::path::PathBuf::from(&w.path))
            .collect();
        for dir in &dirs {
            crate::test_support::trust_repo(dir);
        }
        let count = |dirs: &[std::path::PathBuf]| -> usize {
            dirs.iter()
                .map(|d| crate::engine::git_cli::spawn_log::spawns_in(d).len())
                .sum()
        };
        for round in 0..3 {
            let before = count(&dirs);
            let started = Instant::now();
            let snap = snapshot(&repo);
            println!(
                "round {round}: {} worktrees, {} spawns, {:?}, ok(worktrees={}, changes={}, collisions={})",
                dirs.len(),
                count(&dirs) - before,
                started.elapsed(),
                snap.worktrees.ok,
                snap.changes.ok,
                snap.collisions.ok
            );
        }
        let mut by_argv: std::collections::BTreeMap<String, usize> = Default::default();
        for d in &dirs {
            for argv in crate::engine::git_cli::spawn_log::spawns_in(d) {
                *by_argv
                    .entry(argv.join(" ").chars().take(70).collect())
                    .or_default() += 1;
            }
        }
        for (argv, n) in by_argv {
            println!("{n:5} {argv}");
        }
    }
    use serde_json::json;
    use std::fs;

    use crate::test_support::git_in;

    #[test]
    fn same_path_takes_strings_and_paths_and_sees_through_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        fs::create_dir(&real).unwrap();
        assert!(same_path(real.to_str().unwrap(), &real));
        assert!(!same_path(&real, dir.path()));
        // A removed directory cannot be canonicalised and compares literally.
        let gone = dir.path().join("gone");
        assert!(same_path(&gone, &gone));
        assert!(!same_path(&gone, &real));
        #[cfg(unix)]
        {
            let link = dir.path().join("link");
            std::os::unix::fs::symlink(&real, &link).unwrap();
            assert!(same_path(&link, &real));
        }
    }

    fn init_repo() -> tempfile::TempDir {
        let dir = tempfile::TempDir::new().unwrap();
        git_in(dir.path(), &["init", "-b", "main"]);
        fs::write(dir.path().join("shared.txt"), "seed").unwrap();
        git_in(dir.path(), &["add", "."]);
        git_in(dir.path(), &["commit", "-m", "init"]);
        dir
    }

    /// A listing entry carrying exactly what an unlisted checkout is known
    /// to have: a path, its directory name, and nothing git would supply.
    fn bare_listing(path: &str, name: &str) -> WorktreeInfo {
        WorktreeInfo {
            path: path.into(),
            name: name.into(),
            head: String::new(),
            branch: None,
            is_bare: false,
            is_detached: false,
            is_main: false,
            is_locked: false,
            is_prunable: false,
            dirty_files: None,
            diff_stat: None,
            main_divergence: None,
            active_routes: Vec::new(),
            scan_note: None,
            status_read: worktree::StatusRead::NotRead,
        }
    }

    /// The snapshot row and the change-context row for an unlisted checkout
    /// come from one builder. Pinned to the bytes the two separate builders
    /// produced before they converged, so the convergence changed no output.
    #[test]
    fn listed_and_unlisted_worktree_rows_are_built_identically() {
        let path = "/repo/.gitpulse/worktrees/fix-1a2b3c4d";
        let pinned = r#"{"path":"/repo/.gitpulse/worktrees/fix-1a2b3c4d","name":"fix-1a2b3c4d","branch":null,"is_detached":false,"is_main":false,"is_bare":false,"dirty_files":null,"agent_kind":"gitpulse","session_slug":"fix-1a2b3c4d","operation_kind":"rebase","operation_ok":true,"head":null,"ahead":null,"behind":null,"commits_since_base":null}"#;
        let unlisted = unlisted_worktree(path, "rebase".into(), true);
        assert_eq!(serde_json::to_string(&unlisted).unwrap(), pinned);
        let listed = summarise_worktree(&bare_listing(path, "fix-1a2b3c4d"), "rebase".into(), true);
        assert_eq!(serde_json::to_string(&listed).unwrap(), pinned);
        // A path with no file name falls back to the path itself.
        // A collision party carries the same kind the row for that path does.
        assert_eq!(agent_labels(path).0, listed.agent_kind);
        assert_eq!(agent_labels("/repo"), (String::new(), String::new()));
        let root = unlisted_worktree("/", String::new(), false);
        assert_eq!(
            serde_json::to_string(&root).unwrap(),
            r#"{"path":"/","name":"/","branch":null,"is_detached":false,"is_main":false,"is_bare":false,"dirty_files":null,"agent_kind":"","session_slug":"","operation_kind":"","operation_ok":false,"head":null,"ahead":null,"behind":null,"commits_since_base":null}"#
        );
    }

    /// A running process registered as one Claude Code session in `cwd`.
    /// Killed when dropped, so a failing assertion leaves nothing behind.
    #[cfg(unix)]
    struct SimulatedSession(std::process::Child);

    #[cfg(unix)]
    impl Drop for SimulatedSession {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[cfg(unix)]
    fn simulate_session(registry: &Path, cwd: &Path) -> SimulatedSession {
        let child = std::process::Command::new("sleep")
            .arg("60")
            .spawn_locked()
            .expect("spawn a stand-in session process");
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        // The fields Claude Code writes, as observed in its registry.
        let entry = serde_json::json!({
            "pid": child.id(),
            "sessionId": format!("simulated-{}", child.id()),
            "cwd": cwd,
            "startedAt": now_ms,
            "kind": "interactive",
            "entrypoint": "cli",
            "status": "busy",
        });
        fs::write(
            registry.join(format!("{}.json", child.id())),
            entry.to_string(),
        )
        .unwrap();
        SimulatedSession(child)
    }

    /// Two live sessions in one worktree, the case `overlapping_files`
    /// cannot see: there is only one worktree, so nothing overlaps across
    /// worktrees, and both sessions are editing the same files.
    #[cfg(unix)]
    #[test]
    fn collision_risk_flags_a_file_dirty_while_two_sessions_share_one_worktree() {
        let main = init_repo();
        let repo = main.path().to_str().unwrap();
        fs::write(main.path().join("shared.txt"), "edited").unwrap();
        let registry = tempfile::TempDir::new().unwrap();
        let _first = simulate_session(registry.path(), main.path());
        // The second session works from a subdirectory of the same checkout.
        fs::create_dir_all(main.path().join("src")).unwrap();
        let _second = simulate_session(registry.path(), &main.path().join("src"));
        let observation = live_sessions::observe_with(
            Some(registry.path()),
            &crate::workbench::process_birth::running_since,
        );
        let claude = observation.iter().find(|k| k.kind == "claude").unwrap();
        assert_eq!(claude.live.len(), 2, "both stand-ins are alive: {claude:?}");

        let risk = collision_risk_observed(repo, &observation);
        assert!(risk.ok, "{risk:?}");
        assert_eq!(risk.overlapping_files, 0, "one worktree overlaps nothing");
        assert_eq!(risk.shared_worktree_files, 1, "{risk:?}");
        let shared = &risk.shared_worktrees[0];
        assert!(same_path(&shared.path, repo), "{shared:?}");
        assert_eq!(shared.sessions.len(), 2, "{shared:?}");
        assert!(shared.scanned);
        assert_eq!(shared.files, ["shared.txt"]);
        // Codex cannot be observed, so the same-worktree half is never
        // reported as a clean, complete check.
        assert!(!risk.sessions_ok);
        assert!(
            risk.sessions_error.contains("codex"),
            "{}",
            risk.sessions_error
        );

        let snap = snapshot_observed(
            repo,
            SNAPSHOT_DEADLINE,
            MAX_SNAPSHOT_WORKTREES,
            &observation,
        );
        assert_eq!(snap.agents.live.sessions, 2, "{:?}", snap.agents.live);
        assert_eq!(snap.agents.live.worktrees.len(), 1);
        assert!(same_path(&snap.agents.live.worktrees[0].path, repo));
        assert_eq!(snap.collisions.shared_worktree_files, 1);
        // The layout count still sees no agent worktree, which is exactly why
        // it cannot stand for the number of running sessions.
        assert_eq!(snap.agents.sessions, 0);
    }

    #[cfg(unix)]
    #[test]
    fn one_session_per_worktree_is_not_a_shared_worktree() {
        let main = init_repo();
        let repo = main.path().to_str().unwrap();
        fs::create_dir_all(main.path().join(".claude/worktrees")).unwrap();
        let wt = main.path().join(".claude/worktrees/lane");
        worktree::add_worktree(
            repo,
            wt.to_str().unwrap(),
            Some("lane"),
            Some("main"),
            false,
        )
        .expect("add worktree");
        crate::test_support::trust_repo(&wt);
        fs::write(main.path().join("shared.txt"), "edited").unwrap();
        let registry = tempfile::TempDir::new().unwrap();
        let _main_session = simulate_session(registry.path(), main.path());
        let _lane_session = simulate_session(registry.path(), &wt);
        let observation = live_sessions::observe_with(
            Some(registry.path()),
            &crate::workbench::process_birth::running_since,
        );

        let risk = collision_risk_observed(repo, &observation);
        assert!(risk.ok, "{risk:?}");
        assert!(risk.shared_worktrees.is_empty(), "{risk:?}");
        assert_eq!(risk.shared_worktree_files, 0);

        let snap = snapshot_observed(
            repo,
            SNAPSHOT_DEADLINE,
            MAX_SNAPSHOT_WORKTREES,
            &observation,
        );
        assert_eq!(snap.agents.live.sessions, 2);
        assert_eq!(
            snap.agents.live.worktrees.len(),
            2,
            "{:?}",
            snap.agents.live
        );
    }

    #[test]
    fn a_failed_listing_reports_every_session_kind_as_unknown() {
        let snap = snapshot_observed(
            "/no/such/gitpulse-insights-repo",
            SNAPSHOT_DEADLINE,
            MAX_SNAPSHOT_WORKTREES,
            &Vec::new(),
        );
        assert!(!snap.agents.live.ok);
        assert!(!snap.agents.live.kinds.is_empty());
        assert!(snap
            .agents
            .live
            .kinds
            .iter()
            .all(|k| !k.ok && !k.error.is_empty()));
        assert!(!snap.collisions.sessions_ok);
    }

    /// Walks a serialized payload and collects every object that says it
    /// failed (`ok: false`, or `recording: false`), keyed by its path.
    fn failed_facets<'a>(
        path: String,
        value: &'a serde_json::Value,
        out: &mut Vec<(String, &'a serde_json::Value)>,
    ) {
        match value {
            serde_json::Value::Object(map) => {
                if map.get("ok") == Some(&json!(false))
                    || map.get("recording") == Some(&json!(false))
                {
                    out.push((path.clone(), value));
                }
                for (key, child) in map {
                    failed_facets(format!("{path}.{key}"), child, out);
                }
            }
            serde_json::Value::Array(items) => {
                for (index, child) in items.iter().enumerate() {
                    failed_facets(format!("{path}[{index}]"), child, out);
                }
            }
            _ => {}
        }
    }

    /// Under a spawn gate that sheds the read, every failed facet says it was a
    /// deferral, with when to ask again, and none calls it an invalid worktree.
    #[test]
    fn a_snapshot_under_a_forced_deferral_marks_every_failed_facet_deferred() {
        let main = init_repo();
        let repo = main.path().to_str().unwrap();
        let mut wire = crate::engine::git_cli::with_forced_deferral(|| json!(snapshot(repo)));
        assert!(mark_deferrals(&mut wire), "{wire}");

        let mut failed = Vec::new();
        failed_facets("snapshot".to_string(), &wire, &mut failed);
        let named: Vec<&str> = failed.iter().map(|(path, _)| path.as_str()).collect();
        for facet in [
            "snapshot.worktrees",
            "snapshot.agents",
            "snapshot.changes",
            "snapshot.collisions",
            "snapshot.ledger",
        ] {
            assert!(named.contains(&facet), "{facet} did not fail: {named:?}");
        }
        for (path, facet) in &failed {
            assert_eq!(facet["deferred"], json!(true), "{path}: {facet}");
            assert_eq!(
                facet["retry_after_ms"],
                json!(deferral_retry_after_ms()),
                "{path}: {facet}"
            );
        }
        assert!(
            !wire.to_string().contains("invalid_worktree"),
            "a deferral was called invalid: {wire}"
        );
        assert_eq!(wire["ledger"]["error_code"], json!("deferred_worktree"));
    }

    /// A facet that ran keeps its exact shape, and a facet that failed for a
    /// real reason is never dressed up as a deferral, even beside one that was.
    #[test]
    fn deferral_marks_only_the_failures_that_were_deferrals() {
        let deferral = "git status deferred under load after 2.013s: the git spawn rate limit \
                        admitted nothing sooner";
        let mut payload = json!({
            "worktrees": { "ok": true, "error": "", "count": 3 },
            "changes": { "ok": false, "error": deferral },
            "collisions": { "ok": false, "error": "fatal: not a git repository",
                            "sessions_ok": false, "sessions_error": "" },
            "ledger": { "recording": false, "error": "[invalid_worktree] fatal: not a git repository",
                        "error_code": "invalid_worktree" }
        });
        let ran = payload["worktrees"].clone();
        assert!(mark_deferrals(&mut payload));
        assert_eq!(payload["worktrees"], ran, "a facet that ran is untouched");
        assert_eq!(payload["changes"]["deferred"], json!(true));
        assert_eq!(
            payload["changes"]["retry_after_ms"],
            json!(deferral_retry_after_ms())
        );
        for real in ["collisions", "ledger"] {
            assert!(
                payload[real].get("deferred").is_none(),
                "{real}: {}",
                payload[real]
            );
            assert!(payload[real].get("retry_after_ms").is_none(), "{real}");
        }
        assert_eq!(payload["ledger"]["error_code"], json!("invalid_worktree"));

        let mut clean = json!({ "worktrees": { "ok": true, "error": "" } });
        assert!(!mark_deferrals(&mut clean));
        assert_eq!(clean, json!({ "worktrees": { "ok": true, "error": "" } }));
    }

    /// The hint is the budget's own refill interval, not a magic number: 8
    /// tokens a second is one every 125 ms.
    #[test]
    fn the_retry_hint_is_one_refill_interval_of_the_spawn_budget() {
        let (_, per_sec) = crate::engine::git_cli::production_spawn_rate();
        assert_eq!(
            deferral_retry_after_ms(),
            1000u64.div_ceil(u64::from(per_sec))
        );
        assert!(deferral_retry_after_ms() > 0);
    }

    #[test]
    fn snapshot_on_missing_repo_fails_facets_loudly() {
        let snap = snapshot("/no/such/gitpulse-insights-repo");
        assert!(!snap.worktrees.ok, "{snap:?}");
        assert!(!snap.worktrees.error.is_empty());
        assert!(!snap.changes.ok);
        assert!(!snap.collisions.ok);
        assert_eq!(snap.collisions.overlapping_files, 0);
        assert!(
            snap.collisions.items.is_empty(),
            "a collision scan that failed must not invent overlaps: {:?}",
            snap.collisions.items
        );
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
        crate::test_support::trust_repo(&wt);

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
        // The repo-wide view classifies nothing: a verdict costs git diffs per
        // party, and it is the edited file's, which only a path-scoped read
        // knows. Asked about the file, the same overlap carries one (typically
        // file_level here: no DevMap store in the fixture).
        assert!(
            risk.items.iter().all(|item| item.entity.is_none()),
            "{risk:?}"
        );
        let scoped = collision_for_path(repo, "shared.txt", None);
        let first = scoped.items.first().expect("the row for shared.txt");
        assert_eq!(first.path, "shared.txt");
        let entity = first
            .entity
            .as_ref()
            .expect("the asked-about row is classified");
        assert_eq!(entity.path, first.path);
        assert!(
            matches!(
                entity.kind,
                EntityCollisionKind::FileLevel
                    | EntityCollisionKind::DisjointSymbols
                    | EntityCollisionKind::SharedSymbol
            ),
            "{entity:?}"
        );
    }

    /// S3-09, at the seam the app's memory reads through: full path lists (not
    /// path-limited reads) cut at the row cap, so the focus row has to be kept
    /// ahead of the alphabet or the fortieth file reads as "no collision".
    #[test]
    fn a_full_list_read_keeps_the_focus_row_past_the_row_cap() {
        let list = vec![
            bare_listing("/gp-test/a", "a"),
            bare_listing("/gp-test/b", "b"),
        ];
        let read = |_: &str, _: Option<&worktree::ScanDeadline>| -> PathsRead {
            Ok(((0..40).map(|n| format!("f{n:02}.txt")).collect(), false))
        };
        let live = live_in(&Vec::new(), &list);
        let focused = collision_from_list_reading(
            &list,
            &live,
            None,
            &read,
            CollisionScope {
                focus: Some("f39.txt"),
                involving: None,
            },
        );
        assert_eq!(focused.items.len(), MAX_COLLISION_ITEMS);
        assert_eq!(focused.overlapping_files, 40);
        assert!(focused.truncated);
        assert!(
            focused.items.iter().any(|item| item.path == "f39.txt"),
            "{focused:?}"
        );
        let unfocused =
            collision_from_list_reading(&list, &live, None, &read, CollisionScope::default());
        assert!(!unfocused.items.iter().any(|item| item.path == "f39.txt"));
    }

    /// `repo` and one linked worktree of it, both trusted, on a repository
    /// whose `shared.txt` is committed.
    fn repo_with_one_worktree(slug: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let main = init_repo();
        let repo = main.path().to_str().unwrap();
        fs::create_dir_all(main.path().join(".claude/worktrees")).unwrap();
        let wt = main.path().join(".claude/worktrees").join(slug);
        worktree::add_worktree(
            repo,
            wt.to_str().unwrap(),
            Some(&format!("agent/{slug}")),
            Some("main"),
            false,
        )
        .expect("add worktree");
        crate::test_support::trust_repo(&wt);
        (main, wt)
    }

    /// S3-01. The editor's own worktree is clean, so the repo-wide overlap list
    /// (two dirty parties or none) is empty by construction while another
    /// worktree holds the file. The path-scoped read says who.
    #[test]
    fn a_file_one_other_worktree_holds_dirty_is_a_row_when_asked_about_that_file() {
        let (main, wt) = repo_with_one_worktree("holder");
        let repo = main.path().to_str().unwrap();
        fs::write(wt.join("shared.txt"), "theirs").unwrap();

        let wide = collision_risk(repo);
        assert!(wide.items.is_empty(), "no overlap: {wide:?}");

        let risk = collision_for_path(repo, "shared.txt", None);
        assert!(risk.ok, "{risk:?}");
        let row = risk.items.first().expect("a row for the held file");
        assert_eq!(row.path, "shared.txt");
        assert_eq!(row.worktrees.len(), 1, "{row:?}");
        assert!(same_path(&row.worktrees[0].path, &wt), "{row:?}");
        assert_eq!(risk.overlapping_files, 0, "one holder is not an overlap");
        assert!(row.entity.is_none(), "one holder has nothing to compare");

        let untouched = collision_for_path(repo, "not-held.txt", None);
        assert!(untouched.ok && untouched.items.is_empty(), "{untouched:?}");
    }

    /// S3-01/S3-08. A new file in a new directory, untracked in the holder and
    /// absent from the asker: the path is the file, not `new/`.
    #[test]
    fn an_untracked_file_in_a_new_directory_is_found_by_its_own_path() {
        let (main, wt) = repo_with_one_worktree("newdir");
        let repo = main.path().to_str().unwrap();
        fs::create_dir_all(wt.join("new/dir")).unwrap();
        fs::write(wt.join("new/dir/x.rs"), "fn x() {}").unwrap();

        let risk = collision_for_path(repo, "new/dir/x.rs", None);
        assert!(risk.ok, "{risk:?}");
        assert_eq!(risk.items.len(), 1, "{risk:?}");
        assert_eq!(risk.items[0].path, "new/dir/x.rs");
        assert!(same_path(&risk.items[0].worktrees[0].path, &wt));

        // Two worktrees each holding it: one row for the file, never `new/`.
        fs::create_dir_all(main.path().join("new/dir")).unwrap();
        fs::write(main.path().join("new/dir/x.rs"), "fn y() {}").unwrap();
        let wide = collision_risk(repo);
        let paths: Vec<&str> = wide.items.iter().map(|i| i.path.as_str()).collect();
        assert_eq!(paths, ["new/dir/x.rs"], "{wide:?}");
    }

    /// S3-08. A user's `status.showUntrackedFiles=no` must not hide a new file
    /// from the check.
    #[test]
    fn a_users_untracked_files_setting_does_not_hide_a_held_new_file() {
        let (main, wt) = repo_with_one_worktree("quiet");
        let repo = main.path().to_str().unwrap();
        git_in(main.path(), &["config", "status.showUntrackedFiles", "no"]);
        fs::write(wt.join("fresh.rs"), "fn f() {}").unwrap();
        let risk = collision_for_path(repo, "fresh.rs", None);
        assert_eq!(risk.items.len(), 1, "{risk:?}");
        let (paths, _) = changed_paths_within(wt.to_str().unwrap(), None).unwrap();
        assert_eq!(paths, ["fresh.rs"]);
    }

    /// S3-01. A worktree that was not read before the deadline, or could not be
    /// read at all, makes the answer partial: never an empty, clean-looking one.
    #[test]
    fn a_worktree_that_was_not_read_makes_the_path_answer_partial() {
        let (main, wt) = repo_with_one_worktree("late");
        let repo = main.path().to_str().unwrap();
        fs::write(wt.join("shared.txt"), "theirs").unwrap();

        let expired = worktree::ScanDeadline::after(Instant::now(), Duration::ZERO);
        let risk = collision_for_path(repo, "shared.txt", Some(&expired));
        assert!(!risk.ok, "{risk:?}");
        assert!(risk.truncated && risk.unscanned_worktrees > 0, "{risk:?}");
        assert!(risk.error.contains("ran out of time"), "{}", risk.error);
        assert!(risk.items.is_empty(), "nothing was read: {risk:?}");

        // One that cannot be read (its directory is gone) is counted failed.
        fs::remove_dir_all(&wt).unwrap();
        let gone = collision_for_path(repo, "shared.txt", None);
        assert!(!gone.ok && gone.failed_worktrees == 1, "{gone:?}");
        assert!(gone.truncated, "{gone:?}");
    }

    /// S3-09. Forty files dirty in both worktrees: the row cap is 32, and the
    /// fortieth must still be found when it is the one being edited.
    #[test]
    fn the_file_being_edited_is_found_past_the_row_cap() {
        let (main, wt) = repo_with_one_worktree("forty");
        let repo = main.path().to_str().unwrap();
        for n in 0..40 {
            fs::write(main.path().join(format!("f{n:02}.txt")), "m").unwrap();
            fs::write(wt.join(format!("f{n:02}.txt")), "w").unwrap();
        }
        let wide = collision_risk(repo);
        assert_eq!(wide.items.len(), MAX_COLLISION_ITEMS, "{wide:?}");
        assert!(wide.truncated && wide.overlapping_files == 40, "{wide:?}");
        assert!(!wide.items.iter().any(|i| i.path == "f39.txt"));

        let risk = collision_for_path(repo, "f39.txt", None);
        assert!(risk.ok, "{risk:?}");
        assert_eq!(risk.items.len(), 1, "{risk:?}");
        assert_eq!(risk.items[0].path, "f39.txt");
        assert_eq!(risk.items[0].worktrees.len(), 2);
    }

    /// The same cap, in a worktree's context: its own rows are chosen before
    /// the cut, so thirty-two overlaps elsewhere cannot hide its one.
    #[test]
    fn a_worktrees_context_keeps_its_own_collision_when_others_fill_the_cap() {
        let (main, wt) = repo_with_one_worktree("busy");
        let repo = main.path().to_str().unwrap();
        let third = main.path().join(".claude/worktrees/third");
        worktree::add_worktree(
            repo,
            third.to_str().unwrap(),
            Some("agent/third"),
            Some("main"),
            false,
        )
        .expect("add third");
        crate::test_support::trust_repo(&third);
        for n in 0..40 {
            fs::write(wt.join(format!("aa{n:02}.txt")), "w").unwrap();
            fs::write(third.join(format!("aa{n:02}.txt")), "t").unwrap();
        }
        fs::write(main.path().join("zz.txt"), "m").unwrap();
        fs::write(wt.join("zz.txt"), "w").unwrap();

        let context = change_context(repo, None);
        let paths: Vec<&str> = context
            .collisions
            .items
            .iter()
            .map(|i| i.path.as_str())
            .collect();
        assert_eq!(paths, ["zz.txt"], "{:?}", context.collisions);
    }

    #[test]
    fn dirty_elsewhere_lists_the_other_worktrees_files_and_says_what_it_cannot_establish() {
        let (main, wt) = repo_with_one_worktree("elsewhere");
        let repo = main.path().to_str().unwrap();
        fs::write(wt.join("theirs.txt"), "t").unwrap();
        fs::write(main.path().join("mine.txt"), "m").unwrap();
        let view = dirty_elsewhere(repo);
        assert!(view.ok, "{view:?}");
        assert_eq!(view.worktrees.len(), 1, "{view:?}");
        assert!(same_path(&view.worktrees[0].path, &wt));
        assert_eq!(view.worktrees[0].files, ["theirs.txt"]);
        assert!(view.establishes.contains("cannot know which file"));
        fs::remove_dir_all(&wt).unwrap();
        let broken = dirty_elsewhere(repo);
        assert!(!broken.ok && !broken.worktrees[0].scanned, "{broken:?}");
    }

    #[test]
    fn porcelain_collision_scan_does_not_embed_entity_verdicts() {
        // `collision_from_list` is the porcelain path scan. Enrichment is a
        // separate pass — putting git-diff or index opens inside the scan
        // would fight its bound.
        let main = init_repo();
        let repo = main.path().to_str().unwrap();
        fs::create_dir_all(main.path().join(".claude/worktrees")).unwrap();
        let wt = main.path().join(".claude/worktrees/session-b");
        worktree::add_worktree(
            repo,
            wt.to_str().unwrap(),
            Some("agent/session-b"),
            Some("main"),
            false,
        )
        .expect("add worktree");
        crate::test_support::trust_repo(&wt);
        fs::write(main.path().join("shared.txt"), "main-edit").unwrap();
        fs::write(wt.join("shared.txt"), "agent-edit").unwrap();

        let list = worktree::list_worktrees(repo).expect("list");
        let scanned = collision_from_list(&list, &live_in(&Vec::new(), &list), None);
        assert!(
            scanned.items.iter().all(|item| item.entity.is_none()),
            "porcelain scan must leave entity unset: {scanned:?}"
        );
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
        crate::test_support::trust_repo(&wt);

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

    /// 250 untracked files, a cap of 200 on rows: the totals cover all 250.
    /// They were rolled from the first 200 and read as exact.
    #[test]
    fn change_totals_cover_every_file_not_the_capped_list() {
        let main = init_repo();
        let repo = main.path().to_str().unwrap();
        for i in 0..250 {
            fs::write(main.path().join(format!("f{i}.txt")), "x\n").unwrap();
        }
        let snap = snapshot(repo);
        assert!(snap.changes.ok, "{:?}", snap.changes);
        assert_eq!(snap.changes.files, 250);
        assert_eq!(snap.changes.untracked, 250, "{:?}", snap.changes);
        assert!(!snap.changes.truncated, "{:?}", snap.changes);

        let listed = active_changes(repo, None, Some(10));
        assert_eq!(listed.files.len(), 10);
        assert_eq!(listed.total, 250);
        assert!(listed.truncated, "the row list is capped");
        assert_eq!(
            listed.untracked, 250,
            "totals are not the capped rows: {listed:?}"
        );
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
            .output_locked()
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

    /// Below git 2.37 the walk must not carry `--since-as-filter`, which that
    /// git rejects; at or above it the flag is the date bound.
    #[test]
    fn the_commit_walk_drops_since_as_filter_below_the_git_that_has_it() {
        let with = fleet_log_args(true, 1_700_000_000);
        assert!(
            with.contains(&"--since-as-filter=1700000000".to_string()),
            "{with:?}"
        );
        let without = fleet_log_args(false, 1_700_000_000);
        assert!(
            !without.iter().any(|arg| arg.contains("since")),
            "{without:?}"
        );
        let limit = (MAX_FLEET_COMMITS + 1).to_string();
        for args in [&with, &without] {
            assert!(args.windows(2).any(|w| w[0] == "-n" && w[1] == limit));
            assert_eq!(args.last().map(String::as_str), Some("--"));
        }
    }

    /// Both walks, run against real git, describe the same window; only the
    /// older git's `truncated` is allowed to differ (and does not on a small
    /// repository).
    #[test]
    fn the_count_bounded_commit_walk_matches_the_filtered_one_on_a_small_repository() {
        let dir = tempfile::TempDir::new().unwrap();
        git_in(dir.path(), &["init", "-b", "main"]);
        let anchor = 1_800_000_000i64;
        for (i, days_ago) in [0i64, 1, 8].into_iter().enumerate() {
            commit_at(
                dir.path(),
                &format!("c{i}"),
                anchor - days_ago * 86_400,
                "one@example.com",
            );
        }
        commit_at(
            dir.path(),
            "ancient",
            anchor - 200 * 86_400,
            "old@example.com",
        );
        let walk = |filtered| {
            fleet_commit_stats_walking(dir.path(), anchor, FLEET_COMMIT_WINDOW_DAYS, filtered)
                .expect("probe runs")
        };
        let (filtered, counted) = (walk(true), walk(false));
        assert_eq!(counted.commits, 3, "the ancient commit is windowed out");
        assert_eq!(counted.daily, filtered.daily);
        assert_eq!(counted.authors, filtered.authors);
        assert_eq!(counted.last_commit_epoch, filtered.last_commit_epoch);
        assert!(!counted.truncated);
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

    /// More history than the cap, all of it older than the window except three
    /// commits: the window was fully covered, so nothing is a floor.
    #[test]
    fn a_long_history_older_than_the_window_is_not_truncated() {
        use std::io::Write;
        let dir = tempfile::TempDir::new().unwrap();
        git_in(dir.path(), &["init", "-b", "main"]);
        let anchor = 1_800_000_000i64;
        let old_total = MAX_FLEET_COMMITS + 50;
        let mut stream = String::new();
        for i in 0..old_total + 3 {
            let stamp = if i < old_total {
                anchor - 400 * 86_400 + i as i64
            } else {
                anchor - 100 * (1 + (i - old_total) as i64)
            };
            stream.push_str(&format!(
                "commit refs/heads/main\nmark :{}\ncommitter A <a@example.com> {stamp} +0000\ndata 2\nc\n",
                i + 1
            ));
            if i > 0 {
                stream.push_str(&format!("from :{i}\n"));
            }
            stream.push('\n');
        }
        let file = dir.path().join("stream");
        fs::File::create(&file)
            .unwrap()
            .write_all(stream.as_bytes())
            .unwrap();
        let status = std::process::Command::new("git")
            .args(["fast-import", "--quiet"])
            .current_dir(dir.path())
            .stdin(fs::File::open(&file).unwrap())
            .status_locked()
            .expect("spawn git fast-import");
        assert!(status.success());

        let stats =
            fleet_commit_stats(dir.path(), anchor, FLEET_COMMIT_WINDOW_DAYS).expect("probe runs");
        assert_eq!(stats.commits, 3);
        assert!(
            !stats.truncated,
            "the window held 3 commits; history past the window must not spend the cap"
        );
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
            &Vec::new(),
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
        crate::test_support::trust_repo(&wt);

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
            crate::test_support::trust_repo(&wt);
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
        // Neither of those came from a capped sample, so neither may claim to.
        assert!(!read.agents.truncated, "{:?}", read.agents);
        assert!(!unread.agents.truncated, "{:?}", unread.agents);
    }

    #[test]
    fn a_capped_snapshot_reports_its_agent_count_as_a_floor() {
        // The agent counts are rolled from the *capped* worktree list. Without
        // the flag, a repository with three hundred worktrees reported the
        // first sixty-four of them as an exact total — a bounded sample
        // rendered as complete coverage, and a kind whose worktrees all fall
        // past the cap disappeared rather than being undercounted.
        let main = init_repo();
        let repo = main.path().to_str().unwrap();
        for (dir, kind) in [(".claude", "claude"), (".cursor", "cursor")] {
            fs::create_dir_all(main.path().join(dir).join("worktrees")).unwrap();
            let wt = main.path().join(dir).join("worktrees").join("s1");
            worktree::add_worktree(
                repo,
                wt.to_str().unwrap(),
                Some(&format!("{kind}/s1")),
                Some("main"),
                false,
            )
            .expect("add worktree");
            crate::test_support::trust_repo(&wt);
        }

        // Uncapped: both kinds visible, the count exact.
        let whole = snapshot_within(repo, SNAPSHOT_DEADLINE, MAX_SNAPSHOT_WORKTREES);
        assert!(!whole.worktrees.truncated, "{:?}", whole.worktrees);
        assert!(!whole.agents.truncated, "{:?}", whole.agents);
        assert_eq!(whole.agents.sessions, 2, "{:?}", whole.agents);
        assert_eq!(whole.agents.kinds.len(), 2, "{:?}", whole.agents);

        // Capped to the main checkout plus one: the second agent worktree is
        // past the cap, so the count is short AND its kind is absent. The
        // flag is the only thing standing between that and a confident lie.
        let capped = snapshot_within(repo, SNAPSHOT_DEADLINE, 2);
        assert!(capped.worktrees.truncated, "{:?}", capped.worktrees);
        assert!(capped.agents.truncated, "{:?}", capped.agents);
        assert!(capped.agents.ok, "{:?}", capped.agents);
        assert!(
            capped.agents.sessions < whole.agents.sessions,
            "the cap must actually bite: {:?}",
            capped.agents
        );
        assert!(
            capped.agents.kinds.len() < whole.agents.kinds.len(),
            "a kind past the cap is absent, not undercounted: {:?}",
            capped.agents
        );
    }

    #[test]
    fn agent_summary_counts_every_agent_checkout_slug_or_none() {
        // The same worktrees as the counts fixture in
        // src/lib/agents/plane.test.ts, which asserts that Fleet's facet, the
        // Work view and the Agents plane all report 3 for them. This is the
        // facet's side of that agreement, executed rather than transcribed.
        let info = |path: &str, is_main: bool| WorktreeInfo {
            path: path.to_string(),
            name: String::new(),
            head: String::new(),
            branch: None,
            is_bare: false,
            is_detached: false,
            is_main,
            is_locked: false,
            is_prunable: false,
            dirty_files: Some(0),
            diff_stat: None,
            main_divergence: None,
            active_routes: Vec::new(),
            scan_note: None,
            status_read: worktree::StatusRead::NotRead,
        };
        let items: Vec<WorktreeSummary> = [
            info("/repo", true),
            info("/repo/.claude/worktrees/alpha", false),
            info("/repo/.codex/worktrees/b", false),
            info("/repo/.claude/worktrees", false),
            info("/repo/feature", false),
        ]
        .iter()
        .map(|item| summarise_worktree(item, String::new(), true))
        .collect();
        assert_eq!(items[3].agent_kind, "claude");
        assert_eq!(
            items[3].session_slug, "",
            "the container has no slug and still counts"
        );
        let summary = agent_summary(
            true,
            false,
            &items,
            live_sessions::attribute(&Vec::new(), &[]),
        );
        assert_eq!(summary.sessions, 3, "{summary:?}");
    }

    #[test]
    fn agent_summary_never_infers_completeness_from_its_own_input() {
        // The rollup cannot see whether the slice it was handed is all of a
        // repository or the front of it, which is exactly why both facts are
        // arguments. An empty slice has three distinct meanings and this is
        // the function that must keep them apart.
        let none = agent_summary(true, false, &[], live_sessions::attribute(&Vec::new(), &[]));
        assert!(none.ok && !none.truncated && none.sessions == 0);

        let unreadable = agent_summary(
            false,
            false,
            &[],
            live_sessions::attribute(&Vec::new(), &[]),
        );
        assert!(!unreadable.ok && unreadable.sessions == 0);

        let capped = agent_summary(true, true, &[], live_sessions::attribute(&Vec::new(), &[]));
        assert!(capped.ok && capped.truncated && capped.sessions == 0);

        // Same zero, three different facts — and no two of them compare equal.
        assert_ne!(
            (none.ok, none.truncated),
            (unreadable.ok, unreadable.truncated)
        );
        assert_ne!((none.ok, none.truncated), (capped.ok, capped.truncated));
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
        let rushed = snapshot_within(repo, Duration::ZERO, MAX_SNAPSHOT_WORKTREES);
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
        crate::test_support::trust_repo(&wt);
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
            staged_additions: Some(0),
            staged_deletions: Some(0),
            unstaged_additions: Some(additions),
            unstaged_deletions: Some(0),
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
        // `GITPULSE_MCP_PATH` is process-global and `resolve_mcp_binary` reads
        // it, so this took no serial at all: a concurrent test calling
        // `mcp_info` saw the fake path, and this one raced whatever the
        // developer's environment held. The serial is the sidecar's, which is
        // the one every binary-path override in this crate takes.
        let serial = crate::harness::sidecar::test_serial();
        // Force the miss path: an explicit env that is not a file.
        let _env = crate::test_support::env::bind_env(&serial)
            .set("GITPULSE_MCP_PATH", "/no/such/gitpulse-mcp");
        let info = mcp_info();
        assert!(!info.binary_found);
        assert!(!info.binary_error.is_empty());
        assert!(info.read_only);
        assert_eq!(info.protocol_version, crate::mcp::PROTOCOL_VERSION);
        assert!(!info.tools.is_empty());
    }

    fn plugin_hooks_template() -> String {
        fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../plugins/gitpulse/hooks/hooks.json"),
        )
        .expect("the plugin ships a hooks manifest")
    }

    /// The commands of the generated hooks manifest, keyed by the subcommand
    /// they run.
    fn generated_commands(binary: &str) -> Vec<(String, String)> {
        let json = hooks_config_with(&plugin_hooks_template(), binary).expect("rewritten");
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        let mut found = Vec::new();
        for groups in value["hooks"].as_object().unwrap().values() {
            for group in groups.as_array().unwrap() {
                for handler in group["hooks"].as_array().unwrap() {
                    let command = handler["command"].as_str().unwrap().to_string();
                    let sub = command.rsplit(' ').next().unwrap_or_default().to_string();
                    found.push((sub, command));
                }
            }
        }
        found
    }

    /// A released app has nothing on `PATH`, so the manifest it hands out must
    /// name the binary by path. The shipped one names it bare.
    #[test]
    fn the_generated_hooks_and_mcp_manifests_name_the_binary_by_absolute_path() {
        let commands =
            generated_commands("/Applications/GitPulse.app/Contents/MacOS/gitpulse-hook");
        assert!(!commands.is_empty());
        for (_, command) in &commands {
            assert!(
                command.contains("/Applications/GitPulse.app/Contents/MacOS/gitpulse-hook"),
                "{command}"
            );
            assert!(!command.starts_with("gitpulse-hook "), "{command}");
        }
        let mcp = mcp_config_with(
            r#"{"mcpServers":{"gitpulse":{"type":"stdio","command":"gitpulse-mcp"}}}"#,
            "/Applications/GitPulse.app/Contents/MacOS/gitpulse-mcp",
        )
        .unwrap();
        assert!(
            mcp.contains("\"command\": \"/Applications/GitPulse.app/Contents/MacOS/gitpulse-mcp\""),
            "{mcp}"
        );
        assert!(mcp_config_with("{}", "/x").is_err());
        assert!(hooks_config_with(
            r#"{"hooks":{"Stop":[{"hooks":[{"command":"other x"}]}]}}"#,
            "/x"
        )
        .is_err());
    }

    /// A hook that cannot launch must not read as a hook that allowed the
    /// call. Run for real: with the binary missing, the deciding hooks exit 2
    /// (the host's "block") with the reason on stderr, session start says the
    /// hooks are off, and a present binary is simply run.
    #[cfg(unix)]
    #[test]
    fn a_missing_hook_binary_refuses_deciding_hooks_and_warns_at_session_start() {
        use std::process::Command;
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("gitpulse hook's dir/gitpulse-hook");
        let run = |command: &str| {
            Command::new("sh")
                .arg("-c")
                .arg(command)
                .output_locked()
                .expect("sh runs")
        };
        let commands = generated_commands(missing.to_str().unwrap());
        for (sub, command) in &commands {
            let out = run(command);
            match sub.as_str() {
                "collision-guard" | "command-gate" => {
                    assert_eq!(out.status.code(), Some(2), "{sub}: {command}");
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    assert!(stderr.contains("blocked this call"), "{sub}: {stderr}");
                }
                "session-brief" => {
                    assert_eq!(out.status.code(), Some(0), "{sub}");
                    let stdout = String::from_utf8_lossy(&out.stdout);
                    let value: serde_json::Value =
                        serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("{e}: {stdout}"));
                    assert!(
                        value["hookSpecificOutput"]["additionalContext"]
                            .as_str()
                            .unwrap()
                            .contains("OFF"),
                        "{stdout}"
                    );
                }
                _ => {}
            }
        }
        assert!(commands.iter().any(|(sub, _)| sub == "command-gate"));
        assert!(commands.iter().any(|(sub, _)| sub == "session-brief"));

        // Present: the wrapper hands control to the binary with its arguments.
        std::fs::create_dir_all(missing.parent().unwrap()).unwrap();
        std::fs::write(&missing, "#!/bin/sh\necho \"ran $*\"\nexit 0\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&missing, std::fs::Permissions::from_mode(0o755)).unwrap();
        for (sub, command) in generated_commands(missing.to_str().unwrap()) {
            let out = run(&command);
            assert_eq!(out.status.code(), Some(0), "{sub}");
            assert!(
                String::from_utf8_lossy(&out.stdout).starts_with("ran "),
                "{sub}"
            );
        }
    }

    /// The bare command a plugin manifest names reaches nothing with an empty
    /// `PATH`; Settings says so instead of reporting the binary as fine.
    #[test]
    fn a_bare_command_is_not_reachable_on_an_empty_path() {
        let dir = tempfile::tempdir().unwrap();
        let name = if cfg!(windows) {
            "gitpulse-hook.exe"
        } else {
            "gitpulse-hook"
        };
        fs::write(dir.path().join(name), "").unwrap();
        assert!(on_path_in(
            "gitpulse-hook",
            Some(dir.path().as_os_str().to_owned())
        ));
        assert!(!on_path_in(
            "gitpulse-hook",
            Some(std::ffi::OsString::new())
        ));
        assert!(!on_path_in("gitpulse-hook", None));
        assert!(!on_path_in(
            "gitpulse-mcp",
            Some(dir.path().as_os_str().to_owned())
        ));
        assert!(looks_like_binary(
            "gitpulse-hook-aarch64-apple-darwin",
            "gitpulse-hook"
        ));
        assert!(looks_like_binary("gitpulse-hook.exe", "gitpulse-hook"));
        assert!(!looks_like_binary("gitpulse-hookah", "gitpulse-hook"));
    }

    /// With the hook binary unresolved nothing is offered to copy: a manifest
    /// naming a binary that is not there is the failure being fixed.
    #[test]
    fn mcp_info_offers_no_hooks_manifest_for_a_hook_binary_it_did_not_find() {
        let serial = crate::harness::sidecar::test_serial();
        let _env = crate::test_support::env::bind_env(&serial)
            .set("GITPULSE_HOOK_PATH", "/no/such/gitpulse-hook");
        let info = mcp_info();
        assert!(!info.hook_found);
        assert!(
            info.hook_error.contains("GITPULSE_HOOK_PATH"),
            "{}",
            info.hook_error
        );
        assert!(info.hooks_config_json.is_empty());
    }

    fn plugin_package(mcp: &str, hooks: &str) -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join(".codex-plugin")).unwrap();
        fs::create_dir_all(root.path().join("hooks")).unwrap();
        fs::write(root.path().join(".codex-plugin/plugin.json"), "{}").unwrap();
        fs::write(root.path().join(".mcp.json"), mcp).unwrap();
        fs::write(root.path().join("hooks/hooks.json"), hooks).unwrap();
        root
    }

    /// A plugin template that cannot be turned into a config used to read as
    /// `plugin_error: ""` and an empty config: Settings showed a blank "copy
    /// this config" with no reason.
    #[test]
    fn mcp_info_reports_a_malformed_plugin_template() {
        let exe = std::env::current_exe().unwrap();
        let serial = crate::harness::sidecar::test_serial();

        let package = plugin_package("{}", r#"{"hooks":{}}"#);
        let _env = crate::test_support::env::bind_env(&serial)
            .set("GITPULSE_PLUGIN_ROOT", package.path().to_str().unwrap())
            .set("GITPULSE_MCP_PATH", exe.to_str().unwrap())
            .set("GITPULSE_HOOK_PATH", "/no/such/gitpulse-hook");
        let info = mcp_info();
        assert!(info.plugin_found);
        assert!(info.binary_found);
        assert!(info.mcp_config_json.is_empty());
        assert!(
            info.plugin_error.contains("no gitpulse server"),
            "{:?}",
            info.plugin_error
        );
        drop(_env);

        // Not JSON at all, and the binary was not found: still reported.
        let package = plugin_package("not json", r#"{"hooks":{}}"#);
        let _env = crate::test_support::env::bind_env(&serial)
            .set("GITPULSE_PLUGIN_ROOT", package.path().to_str().unwrap())
            .set("GITPULSE_MCP_PATH", "/no/such/gitpulse-mcp")
            .set("GITPULSE_HOOK_PATH", "/no/such/gitpulse-hook");
        let info = mcp_info();
        assert!(!info.binary_found);
        assert!(
            info.plugin_error.contains("is not JSON"),
            "{:?}",
            info.plugin_error
        );
        drop(_env);

        // A missing hooks manifest is a read error, not an empty file.
        let package = plugin_package(r#"{"mcpServers":{"gitpulse":{}}}"#, "{}");
        fs::remove_file(package.path().join("hooks/hooks.json")).unwrap();
        let _env = crate::test_support::env::bind_env(&serial)
            .set("GITPULSE_PLUGIN_ROOT", package.path().to_str().unwrap())
            .set("GITPULSE_MCP_PATH", exe.to_str().unwrap())
            .set("GITPULSE_HOOK_PATH", "/no/such/gitpulse-hook");
        let info = mcp_info();
        assert!(
            info.plugin_error.contains("could not read"),
            "{:?}",
            info.plugin_error
        );
        assert!(
            !info.mcp_config_json.is_empty(),
            "a good template still yields a config"
        );
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

    /// The collision check, the change context and the snapshot read paths,
    /// branches and dirty counts. None of them may pay for the diff stats and
    /// divergence a full worktree listing measures: the collision check alone
    /// runs on every Work refresh.
    #[test]
    fn insight_reads_never_run_the_full_worktree_measurements() {
        let main = init_repo();
        let root = main.path().canonicalize().unwrap();
        let repo = root.to_str().unwrap();
        fs::create_dir_all(root.join(".claude/worktrees")).unwrap();
        let mut cwds = vec![root.clone()];
        for name in ["session-a", "session-b"] {
            let wt = root.join(".claude/worktrees").join(name);
            worktree::add_worktree(
                repo,
                wt.to_str().unwrap(),
                Some(&format!("agent/{name}")),
                Some("main"),
                false,
            )
            .expect("add worktree");
            crate::test_support::trust_repo(&wt);
            cwds.push(wt.canonicalize().unwrap());
        }
        fs::write(root.join("shared.txt"), "main-edit").unwrap();
        fs::write(cwds[1].join("shared.txt"), "agent-edit").unwrap();
        let measured = |cwds: &[std::path::PathBuf]| {
            cwds.iter()
                .flat_map(|cwd| crate::engine::git_cli::spawn_log::spawns_in(cwd))
                .filter(|argv| {
                    argv.iter()
                        .any(|arg| arg == "--shortstat" || arg == "rev-list" || arg == "--verify")
                })
                .count()
        };

        let risk = collision_risk(repo);
        assert!(risk.ok, "{risk:?}");
        assert_eq!(risk.overlapping_files, 1, "{risk:?}");
        assert_eq!(measured(&cwds), 0, "collision_risk");

        let context = change_context(repo, cwds[1].to_str());
        assert!(context.worktree_ok, "{context:?}");
        assert_eq!(context.worktree.dirty_files, Some(1));
        assert_eq!(measured(&cwds), 0, "change_context");

        // The snapshot's one per-HEAD read (ahead/behind) is cached by the
        // immutable (head, base) pair: the first snapshot may spawn it for a
        // HEAD it has not seen, and a second, with nothing moved, spawns no
        // rev-list, no --shortstat and no --verify at all.
        let first = snapshot(repo);
        assert!(first
            .worktrees
            .items
            .iter()
            .all(|w| w.dirty_files.is_some()));
        let after_first = measured(&cwds);
        let second = snapshot(repo);
        assert!(second
            .worktrees
            .items
            .iter()
            .all(|w| w.dirty_files.is_some()));
        assert_eq!(measured(&cwds), after_first, "a steady-state snapshot");
    }

    /// A repository with `linked` extra worktrees, all trusted. Returns the
    /// main checkout, the directory holding the linked ones, and their
    /// canonical paths.
    fn repo_with_linked_worktrees(
        linked: usize,
    ) -> (
        tempfile::TempDir,
        tempfile::TempDir,
        Vec<std::path::PathBuf>,
    ) {
        let main = init_repo();
        let parent = tempfile::TempDir::new().unwrap();
        let paths = (0..linked)
            .map(|i| {
                let path = parent.path().join(format!("wt-{i:02}"));
                git_in(
                    main.path(),
                    &[
                        "worktree",
                        "add",
                        "-q",
                        "-b",
                        &format!("agent/{i:02}"),
                        path.to_str().unwrap(),
                    ],
                );
                crate::test_support::trust_repo(&path);
                path.canonicalize().unwrap()
            })
            .collect();
        (main, parent, paths)
    }

    /// Children that ran `git status` in `dir`, whichever way the path was
    /// spelled to the spawn.
    fn status_spawns_in(
        dir: &std::path::Path,
    ) -> Vec<(Vec<String>, crate::engine::git_cli::Admission)> {
        crate::engine::git_cli::spawn_log::classed_spawns_in(dir)
            .into_iter()
            .filter(|(argv, _)| argv.iter().any(|arg| arg == "status"))
            .collect()
    }

    /// One `git status` per worktree, shared by the worktree rows and the
    /// collision scan. The collision scan used to re-run the status the
    /// listing had already read, in every worktree it covered.
    #[test]
    fn a_snapshot_reads_each_worktrees_status_once() {
        let (main, _parent, linked) = repo_with_linked_worktrees(17);
        let root = main.path().canonicalize().unwrap();
        // Two worktrees dirty the same file, so the collision scan has a real
        // overlap to find from the statuses it is given.
        fs::write(linked[0].join("shared.txt"), "first").unwrap();
        fs::write(linked[1].join("shared.txt"), "second").unwrap();

        let snap = snapshot(root.to_str().unwrap());
        assert_eq!(snap.worktrees.count, 18, "{:?}", snap.worktrees);
        assert!(snap.collisions.ok, "{:?}", snap.collisions);
        assert_eq!(
            snap.collisions.overlapping_files, 1,
            "{:?}",
            snap.collisions
        );

        for dir in &linked {
            assert_eq!(
                status_spawns_in(dir).len(),
                1,
                "{} was read more than once",
                dir.display()
            );
        }
        // The main checkout is read by the listing and once more by
        // `get_status`, which needs per-file untracked rows (`-u`) and churn
        // the listing's count does not: the O(1) beside the N statuses.
        assert!(
            status_spawns_in(&root).len() <= 2,
            "{:?}",
            status_spawns_in(&root)
        );
    }

    /// The collision scan runs on rayon's pool, whose threads start in the
    /// default class. Under a caller that asked for background work, its
    /// statuses competed as refresh traffic.
    #[test]
    fn the_collision_scan_keeps_the_callers_admission_class() {
        use crate::engine::git_cli::{with_admission, Admission};
        let (main, _parent, linked) = repo_with_linked_worktrees(10);
        let root = main.path().canonicalize().unwrap();
        let risk = with_admission(Admission::Background, || {
            collision_risk(root.to_str().unwrap())
        });
        assert!(risk.ok, "{risk:?}");
        assert_eq!(risk.scanned_worktrees, 11, "{risk:?}");

        let mut checked = 0;
        let mut escaped = Vec::new();
        for dir in linked.iter().chain(std::iter::once(&root)) {
            for (argv, class) in status_spawns_in(dir) {
                checked += 1;
                if class != Admission::Background {
                    escaped.push((dir.clone(), argv, class));
                }
            }
        }
        assert_eq!(checked, 11, "every worktree was read once");
        assert!(
            escaped.is_empty(),
            "{} of {checked} status reads left the caller's class: {escaped:?}",
            escaped.len()
        );
    }

    /// The snapshot reports which operation is parked and nothing else about
    /// it, so reading the parked state is a look at control files. The full
    /// report also resolves the branch, describes the incoming commit and
    /// lists conflicts: three more children per parked worktree, run one
    /// worktree after another inside the deadline-bounded stage.
    #[test]
    fn a_parked_operation_is_named_without_starting_a_process() {
        let (main, _parent, linked) = repo_with_linked_worktrees(2);
        let root = main.path().canonicalize().unwrap();
        let parked = &linked[0];
        let head = git_text(parked, &["rev-parse", "HEAD"]).unwrap();
        let git_dir = fs::read_to_string(parked.join(".git")).unwrap();
        let git_dir = std::path::PathBuf::from(git_dir.trim().trim_start_matches("gitdir: "));
        fs::write(git_dir.join("MERGE_HEAD"), head).unwrap();
        let before = crate::engine::git_cli::spawn_log::spawns_in(parked).len();

        let snap = snapshot(root.to_str().unwrap());
        let row = snap
            .worktrees
            .items
            .iter()
            .find(|w| same_path(&w.path, parked.to_str().unwrap()))
            .expect("the parked worktree is listed");
        assert_eq!(row.operation_kind, "Merge", "{row:?}");
        assert!(row.operation_ok, "{row:?}");
        assert_eq!(snap.worktrees.blocked, 1, "{:?}", snap.worktrees);

        let beyond_status: Vec<_> = crate::engine::git_cli::spawn_log::spawns_in(parked)
            .into_iter()
            .skip(before)
            .filter(|argv| !argv.iter().any(|arg| arg == "status"))
            .collect();
        assert!(
            beyond_status.is_empty(),
            "probing the parked operation started: {beyond_status:?}"
        );
    }

    /// The deadline bounds the work, not only the gaps between stages: the
    /// worktree status reads ran to completion whatever it said, and the
    /// snapshot returned when they finished.
    #[test]
    fn a_snapshot_returns_within_its_deadline_when_every_status_is_slow() {
        let (main, _parent, linked) = repo_with_linked_worktrees(17);
        let root = main.path().canonicalize().unwrap();
        let mut everywhere = linked.clone();
        everywhere.push(root.clone());
        let deadline = Duration::from_secs(1);

        let started = Instant::now();
        let snap = crate::engine::worktree::status_probes::with_probe(
            &everywhere,
            |_| std::thread::sleep(Duration::from_millis(500)),
            || snapshot_within(root.to_str().unwrap(), deadline, MAX_SNAPSHOT_WORKTREES),
        );
        let took = started.elapsed();

        assert!(
            took < deadline + Duration::from_secs(1),
            "a 1 s deadline returned after {took:?} (the snapshot reports {} ms)",
            snap.duration_ms
        );
        assert!(snap.deadline_expired, "{snap:?}");
        // The reads that did not happen are unknown, never clean.
        assert_eq!(snap.worktrees.count, 18, "{:?}", snap.worktrees);
        assert!(snap.worktrees.dirty_unknown > 0, "{:?}", snap.worktrees);
        assert!(snap.worktrees.scanned < 18, "{:?}", snap.worktrees);
        assert!(!snap.collisions.ok, "{:?}", snap.collisions);
        assert!(snap.collisions.truncated, "{:?}", snap.collisions);
        assert!(
            snap.collisions.scanned_worktrees
                + snap.collisions.failed_worktrees
                + snap.collisions.unscanned_worktrees
                >= 18,
            "{:?}",
            snap.collisions
        );
    }

    fn row<'a>(snap: &'a InsightsSnapshot, dir: &std::path::Path) -> &'a WorktreeSummary {
        snap.worktrees
            .items
            .iter()
            .find(|w| same_path(&w.path, dir.to_str().unwrap()))
            .expect("the worktree is listed")
    }

    /// Committed-but-unmerged work is invisible to a `git status` scan. A
    /// detached worktree has no branch to name, so its divergence is measured
    /// from its HEAD commit.
    #[test]
    fn a_detached_worktrees_committed_work_is_ahead_and_a_stale_one_behind() {
        use crate::test_support::{git_in, write};
        let (main, _parent, linked) = crate::test_support::repo_with_worktrees(2);
        let root = main.path().canonicalize().unwrap();
        // Lane 0 detaches and commits twice; lane 1 stays on its branch.
        git_in(&linked[0], &["switch", "-q", "--detach"]);
        for n in 0..2 {
            write(&linked[0], &format!("lane{n}.txt"), "work\n");
            git_in(&linked[0], &["add", "-A"]);
            git_in(&linked[0], &["commit", "-q", "-m", &format!("lane {n}")]);
        }
        // The base moves on by one commit.
        write(&root, "base.txt", "base\n");
        git_in(&root, &["add", "-A"]);
        git_in(&root, &["commit", "-q", "-m", "base moves"]);

        let snap = snapshot(root.to_str().unwrap());

        let lane = row(&snap, &linked[0]);
        assert!(lane.is_detached, "{lane:?}");
        assert!(lane.branch.is_none());
        assert_eq!(lane.ahead, Some(2), "{lane:?}");
        assert_eq!(lane.behind, Some(1), "{lane:?}");
        assert_eq!(lane.commits_since_base, Some(2));
        assert_eq!(lane.divergence_note, None);
        let head = git_text(&linked[0], &["rev-parse", "HEAD"]).unwrap();
        assert_eq!(lane.head.as_deref(), Some(head.trim()));
        // The clean-status lane that never committed is behind, not ahead.
        let idle = row(&snap, &linked[1]);
        assert_eq!((idle.ahead, idle.behind), (Some(0), Some(1)), "{idle:?}");
        let primary = row(&snap, &root);
        assert_eq!((primary.ahead, primary.behind), (Some(0), Some(0)));
    }

    /// A row whose divergence was not measured says why; it never reads as 0.
    #[test]
    fn unmeasured_divergence_is_absent_with_a_reason_never_zero() {
        let (main, _parent, linked) = crate::test_support::repo_with_worktrees(1);
        let root = main.path().canonicalize().unwrap();
        let mut rows: Vec<WorktreeSummary> = worktree::list_worktrees_lite(root.to_str().unwrap())
            .unwrap()
            .iter()
            .map(|info| summarise_worktree(info, String::new(), false))
            .collect();
        assert_eq!(rows.len(), 1 + linked.len());
        // A deadline that has already passed measures nothing.
        let passed = worktree::ScanDeadline::after(Instant::now(), Duration::ZERO);
        fill_head_divergence(root.to_str().unwrap(), &mut rows, &passed);
        for row in &rows {
            assert!(row.head.is_some(), "{row:?}");
            assert_eq!(
                (row.ahead, row.behind, row.commits_since_base),
                (None, None, None)
            );
            let note = row.divergence_note.as_deref().unwrap_or_default();
            assert!(note.contains("deadline"), "{row:?}");
        }
        // The fleet and context rows never pay for the read: unmeasured too.
        let unlisted = summarise_worktree(&unlisted_info("/nowhere"), String::new(), false);
        assert_eq!(
            (unlisted.ahead, unlisted.behind, unlisted.head),
            (None, None, None)
        );
    }

    /// An edit made while the snapshot is running is reported when it lands
    /// before that worktree is read: the read is the last git stage, after the
    /// listing and the main checkout's status, and the rows and the collision
    /// scan both come out of it.
    #[test]
    fn an_edit_made_mid_snapshot_before_its_worktrees_read_is_reported() {
        let (main, _parent, linked) = repo_with_linked_worktrees(9);
        let root = main.path().canonicalize().unwrap();
        let first = linked.first().unwrap().clone();
        let last = linked.last().unwrap().clone();
        fs::write(first.join("shared.txt"), "first").unwrap();

        // The main checkout is in the first wave and the last worktree in the
        // third: the edit lands after the snapshot began and before the last
        // worktree's turn. Relies on a fan-out of 4: with 10 worktrees, main is
        // read in wave 1 and the last in wave 3, so a change to
        // DIRTY_SCAN_FAN_OUT that put them in one wave would make this vacuous.
        let edited = last.clone();
        let snap = crate::engine::worktree::status_probes::with_probe(
            std::slice::from_ref(&root),
            move |_| fs::write(edited.join("shared.txt"), "mid-snapshot").unwrap(),
            || snapshot(root.to_str().unwrap()),
        );

        assert!(snap.collisions.ok, "{:?}", snap.collisions);
        assert_eq!(
            snap.collisions.overlapping_files, 1,
            "{:?}",
            snap.collisions
        );
        assert_eq!(snap.collisions.items[0].path, "shared.txt");
        assert_eq!(
            row(&snap, &last).dirty_files,
            Some(1),
            "{:?}",
            snap.worktrees
        );
    }

    /// One read per worktree means the rows and the collision scan can only
    /// disagree about a worktree if they read it at different moments. An edit
    /// that lands after a worktree was read is in neither facet of that
    /// snapshot, and in both of the next one: the snapshot is a point-in-time
    /// read of each worktree, bounded by its own `duration_ms`, and says the
    /// same thing in both places. Before, the listing said "clean" and the
    /// collision scan, reading again later, said "dirty".
    #[test]
    fn the_rows_and_the_collision_scan_describe_one_read_of_each_worktree() {
        let (main, _parent, linked) = repo_with_linked_worktrees(9);
        let root = main.path().canonicalize().unwrap();
        let last = linked.last().unwrap().clone();
        fs::write(linked[0].join("late.txt"), "first").unwrap();

        // The last worktree is read in the third wave; the main checkout, read
        // in the first, is edited just before it. Relies on the same fan-out of 4
        // as the test above.
        let edited = root.clone();
        let snap = crate::engine::worktree::status_probes::with_probe(
            std::slice::from_ref(&last),
            move |_| fs::write(edited.join("late.txt"), "after main was read").unwrap(),
            || snapshot(root.to_str().unwrap()),
        );
        assert_eq!(
            row(&snap, &root).dirty_files,
            Some(0),
            "{:?}",
            snap.worktrees
        );
        assert_eq!(
            snap.collisions.overlapping_files, 0,
            "the collision scan saw an edit the row did not: {:?}",
            snap.collisions
        );

        let next = snapshot(root.to_str().unwrap());
        assert_eq!(
            row(&next, &root).dirty_files,
            Some(1),
            "{:?}",
            next.worktrees
        );
        assert_eq!(
            next.collisions.overlapping_files, 1,
            "{:?}",
            next.collisions
        );
    }
}

#[cfg(test)]
#[path = "adv_l1_tests.rs"]
mod adv_l1_tests;
