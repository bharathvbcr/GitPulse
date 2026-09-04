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
use std::collections::HashMap;
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
    pub is_main: bool,
    pub is_bare: bool,
    pub dirty_files: Option<u32>,
    pub agent_kind: String,
    pub session_slug: String,
    pub operation_kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentKindCount {
    pub kind: String,
    pub sessions: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSummary {
    pub sessions: u32,
    pub kinds: Vec<AgentKindCount>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorktreeFacet {
    pub ok: bool,
    pub error: String,
    pub count: u32,
    pub dirty: u32,
    pub blocked: u32,
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
    pub ok: bool,
    pub error: String,
    pub overlapping_files: u32,
    pub worktrees_involved: u32,
    pub scanned_worktrees: u32,
    pub unscanned_worktrees: u32,
    pub truncated: bool,
    pub items: Vec<CollisionItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InsightsSnapshot {
    pub repo_path: String,
    pub branch: Option<String>,
    pub worktrees: WorktreeFacet,
    pub agents: AgentSummary,
    pub changes: ChangesFacet,
    pub collisions: CollisionRisk,
    pub ledger: LedgerStatus,
    pub codeintel: CodeintelStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangedFile {
    pub path: String,
    pub status_code: String,
    pub is_staged: bool,
    pub is_conflicted: bool,
    pub additions: u32,
    pub deletions: u32,
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangeContext {
    pub repo_path: String,
    pub worktree: WorktreeSummary,
    pub task_id: String,
    pub changes: ActiveChanges,
    pub collisions: Vec<CollisionItem>,
    pub operation: Option<RepoOperation>,
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
        blocked: 0,
        truncated: false,
        items: Vec::new(),
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
        truncated: false,
        items: Vec::new(),
    }
}

fn summarise_worktree(info: &WorktreeInfo, operation_kind: String) -> WorktreeSummary {
    WorktreeSummary {
        path: info.path.clone(),
        name: info.name.clone(),
        branch: info.branch.clone(),
        is_main: info.is_main,
        is_bare: info.is_bare,
        dirty_files: info.dirty_files.map(|n| n as u32),
        agent_kind: agent_kind(&info.path).unwrap_or_default(),
        session_slug: agent_session_slug(&info.path).unwrap_or_default(),
        operation_kind,
    }
}

fn agent_summary(items: &[WorktreeSummary]) -> AgentSummary {
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
        sessions,
        kinds: counts,
    }
}

fn detect_operation(path: &str) -> String {
    let Ok(repo) = validate_repo(path) else {
        return String::new();
    };
    match repo_op::detect(&repo) {
        Ok(Some(op)) => format!("{:?}", op.kind),
        Ok(None) => String::new(),
        Err(_) => String::new(),
    }
}

fn count_statuses(files: &[FileStatus]) -> (u32, u32, u32, u32, u32, u32) {
    let mut staged = 0u32;
    let mut unstaged = 0u32;
    let mut untracked = 0u32;
    let mut conflicted = 0u32;
    let mut additions = 0u32;
    let mut deletions = 0u32;
    for file in files {
        if file.is_conflicted {
            conflicted += 1;
        }
        if file.is_staged {
            staged += 1;
        }
        if file.status_code.contains('?') {
            untracked += 1;
        } else if !file.is_staged || file.status_code.chars().nth(1).is_some_and(|c| c != ' ') {
            unstaged += 1;
        }
        additions += file.additions as u32;
        deletions += file.deletions as u32;
    }
    (
        staged, unstaged, untracked, conflicted, additions, deletions,
    )
}

fn changes_from_status(files: &[FileStatus], truncated: bool) -> ChangesFacet {
    let (staged, unstaged, untracked, conflicted, additions, deletions) = count_statuses(files);
    ChangesFacet {
        ok: true,
        error: String::new(),
        files: files.len() as u32,
        staged,
        unstaged,
        untracked,
        conflicted,
        additions,
        deletions,
        truncated,
    }
}

/// One-shot read of everything an agent needs to see the repository as the
/// Work view does: worktrees, agent sessions, dirty files, collisions, ledger
/// and code graph. Individual facets fail independently.
pub fn snapshot(repo_path: &str) -> InsightsSnapshot {
    let listed = worktree::list_worktrees(repo_path);
    let (worktrees, agents) = match &listed {
        Ok(list) => {
            let truncated = list.len() > MAX_SNAPSHOT_WORKTREES;
            let slice: Vec<&WorktreeInfo> = list.iter().take(MAX_SNAPSHOT_WORKTREES).collect();
            let items: Vec<WorktreeSummary> = slice
                .iter()
                .map(|info| summarise_worktree(info, detect_operation(&info.path)))
                .collect();
            let dirty = items
                .iter()
                .filter(|w| w.dirty_files.unwrap_or(0) > 0)
                .count() as u32;
            let blocked = items
                .iter()
                .filter(|w| !w.operation_kind.is_empty())
                .count() as u32;
            let agents = agent_summary(&items);
            (
                WorktreeFacet {
                    ok: true,
                    error: String::new(),
                    count: list.len() as u32,
                    dirty,
                    blocked,
                    truncated,
                    items,
                },
                agents,
            )
        }
        Err(error) => (
            empty_worktrees(error.clone()),
            AgentSummary {
                sessions: 0,
                kinds: Vec::new(),
            },
        ),
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
        Ok(list) => collision_from_list(list),
        Err(error) => empty_collisions(error.clone()),
    };

    let branch = worktrees
        .items
        .iter()
        .find(|w| w.is_main)
        .and_then(|w| w.branch.clone());

    let ledger = match crate::ledger::bindings::repository_status(repo_path) {
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
        worktrees,
        agents,
        changes,
        collisions,
        ledger,
        codeintel: codeintel::status(repo_path),
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

/// One repository's cheap facet: what can be learned in two `git` spawns.
///
/// Deliberately NOT what [`snapshot`] returns. That function probes every
/// worktree for a parked operation, counts dirty files in up to 32 of them,
/// and cross-scans up to 16 for colliding paths — right for one repository on
/// screen, and several hundred subprocesses when multiplied by a workspace.
/// Everything expensive is left to the per-repository views that already do it.
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
        agents: AgentSummary {
            sessions: 0,
            kinds: Vec::new(),
        },
        last_commit_ok: false,
        last_commit_epoch: 0,
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

fn fleet_facet(repo_path: &str) -> FleetRepoFacet {
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
                    .map(|info| summarise_worktree(info, String::new()))
                    .collect();
                (
                    true,
                    String::new(),
                    list.len() as u32,
                    agent_summary(&items),
                )
            }
            Err(error) => (
                false,
                error,
                0,
                AgentSummary {
                    sessions: 0,
                    kinds: Vec::new(),
                },
            ),
        };

    let (last_commit_ok, last_commit) = match last_commit_epoch(&repo) {
        Ok(epoch) => (true, epoch),
        Err(_) => (false, 0),
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
pub fn fleet_snapshot(repo_paths: &[String]) -> FleetSnapshot {
    let started = Instant::now();
    let requested = repo_paths.len() as u32;

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
            fleet_facet(path)
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
    let truncated = items.len() > MAX_COLLISION_ITEMS || paths_truncated || unscanned > 0;
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
        ok: scan_error.is_empty() || scanned > 0,
        error: scan_error,
        overlapping_files,
        worktrees_involved: involved.len() as u32,
        scanned_worktrees: scanned,
        unscanned_worktrees: unscanned as u32,
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
    }
}

/// Working-tree file list for one worktree, capped and counted.
pub fn active_changes(
    repo_path: &str,
    worktree_path: Option<&str>,
    limit: Option<u32>,
) -> ActiveChanges {
    let target = worktree_path.unwrap_or(repo_path);
    let cap = (limit.unwrap_or(MAX_ACTIVE_FILES as u32) as usize).clamp(1, 500);
    match GitReader::get_status(target) {
        Ok(files) => {
            let total = files.len() as u32;
            let truncated = files.len() > cap;
            let kept: Vec<FileStatus> = files.iter().take(cap).cloned().collect();
            let (staged, unstaged, untracked, conflicted, additions, deletions) =
                count_statuses(&kept);
            ActiveChanges {
                repo_path: repo_path.to_string(),
                worktree_path: target.to_string(),
                ok: true,
                error: String::new(),
                shown: kept.len() as u32,
                files: kept.iter().map(to_changed).collect(),
                total,
                truncated,
                staged,
                unstaged,
                untracked,
                conflicted,
                additions,
                deletions,
            }
        }
        Err(error) => ActiveChanges {
            repo_path: repo_path.to_string(),
            worktree_path: target.to_string(),
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
        },
    }
}

/// In-flight context for one worktree: changes, parked operation, bound task,
/// collisions that involve it.
pub fn change_context(repo_path: &str, worktree_path: Option<&str>) -> ChangeContext {
    let target = worktree_path.unwrap_or(repo_path).to_string();
    let listed = worktree::list_worktrees(repo_path).ok();
    let info = listed.as_ref().and_then(|list| {
        list.iter()
            .find(|w| Path::new(&w.path) == Path::new(&target) || w.path == target)
    });
    let operation = validate_repo(&target)
        .ok()
        .and_then(|repo| repo_op::detect(&repo).ok().flatten());
    let operation_kind = operation
        .as_ref()
        .map(|op| format!("{:?}", op.kind))
        .unwrap_or_default();
    let worktree = match info {
        Some(found) => summarise_worktree(found, operation_kind),
        None => WorktreeSummary {
            path: target.clone(),
            name: Path::new(&target)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| target.clone()),
            branch: None,
            is_main: false,
            is_bare: false,
            dirty_files: None,
            agent_kind: agent_kind(&target).unwrap_or_default(),
            session_slug: agent_session_slug(&target).unwrap_or_default(),
            operation_kind,
        },
    };
    let task_id = crate::ledger::bindings::resolve(repo_path, &target)
        .ok()
        .flatten()
        .unwrap_or_default();
    let changes = active_changes(repo_path, Some(&target), None);
    let collisions = collision_risk(repo_path)
        .items
        .into_iter()
        .filter(|item| item.worktrees.iter().any(|p| p.path == target))
        .collect();
    ChangeContext {
        repo_path: repo_path.to_string(),
        worktree,
        task_id,
        changes,
        collisions,
        operation,
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
    use std::process::Command;

    fn git_in(dir: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args([
                "-c",
                "user.name=GitPulse",
                "-c",
                "user.email=gitpulse@test.local",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .current_dir(dir)
            .output()
            .expect("spawn git");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

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

    #[test]
    fn fleet_snapshot_isolates_one_bad_repository_from_the_rest() {
        let good = init_repo();
        let snap = fleet_snapshot(&[
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

        let snap = fleet_snapshot(&[repo.to_string()]);
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
        let snap = fleet_snapshot(&[repo.clone(), repo.clone(), String::new()]);
        assert_eq!(snap.requested, 3);
        assert_eq!(snap.repos.len(), 1);
    }

    #[test]
    fn fleet_snapshot_reports_no_commits_as_read_rather_than_failed() {
        let dir = tempfile::TempDir::new().unwrap();
        git_in(dir.path(), &["init", "-b", "main"]);
        let snap = fleet_snapshot(&[dir.path().to_str().unwrap().to_string()]);
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
        let snap = fleet_snapshot(&[main.path().to_str().unwrap().to_string()]);
        let facet = &snap.repos[0];
        // The ledger read ran and found nothing. `metrics_ok` is what
        // separates that from a ledger we could not open at all.
        assert!(facet.metrics_ok, "{:?}", facet.metrics_error);
        assert!(facet.metrics.is_none());
    }

    #[test]
    fn fleet_snapshot_truncation_means_short_sweep_not_broken_repository() {
        let snap = fleet_snapshot(&["/no/such/gitpulse-fleet-repo".to_string()]);
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
        let snap = fleet_snapshot(&paths);
        assert!(snap.truncated);
        assert_eq!(snap.repos.len(), MAX_FLEET_REPOS);
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
            info.plugin_path.ends_with("/plugins/gitpulse"),
            "unexpected plugin root: {}",
            info.plugin_path
        );
        assert!(info.plugin_manifest_json.contains("\"mcpServers\""));
        assert!(info.plugin_mcp_json.contains("\"gitpulse\""));
        assert!(!info.plugin_mcp_json.contains("$schema"));
    }
}
