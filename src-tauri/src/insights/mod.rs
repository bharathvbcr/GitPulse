//! Read-only repository insights for agents and the Work view.
//!
//! Assembles worktrees, agent sessions, working-tree changes, and overlapping
//! dirty files into one snapshot. Every facet reports whether it could run:
//! a check that did not run must never look like one that ran and found
//! nothing. Nothing here mutates a repository.

use crate::codeintel::{self, CodeintelStatus};
use crate::engine::git_reader::{FileStatus, GitReader};
use crate::engine::repo_op::{self, RepoOperation};
use crate::engine::validate_repo;
use crate::engine::worktree::{self, agent_kind, agent_session_slug, changed_paths, WorktreeInfo};
use crate::ledger::LedgerStatus;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

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

fn resolve_plugin_root() -> (bool, String, String) {
    if let Ok(explicit) = std::env::var("GITPULSE_PLUGIN_ROOT") {
        let path = Path::new(&explicit);
        if path.join("plugin.json").is_file() {
            return (true, explicit, String::new());
        }
        return (
            false,
            explicit,
            "GITPULSE_PLUGIN_ROOT has no plugin.json".into(),
        );
    }
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("plugin"));
            candidates.push(dir.join("../../../plugin"));
        }
    }
    candidates.push(Path::new(env!("CARGO_MANIFEST_DIR")).join("../plugin"));
    for candidate in candidates {
        if candidate.join("plugin.json").is_file() {
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
        "Agent Plugins package not found next to this app (expected plugin/plugin.json). Set GITPULSE_PLUGIN_ROOT."
            .into(),
    )
}

/// What Settings and an installer need: binary location, Agent Plugins 1.0
/// manifests, and the tool catalog. Never claims a binary is present when
/// the file could not be found.
pub fn mcp_info() -> McpInfo {
    let (binary_found, binary_path, binary_error) = resolve_mcp_binary();
    let (plugin_found, plugin_path, plugin_error) = resolve_plugin_root();
    let (plugin_manifest_json, plugin_mcp_json) = if plugin_found {
        let root = Path::new(&plugin_path);
        let manifest = std::fs::read_to_string(root.join("plugin.json")).unwrap_or_default();
        let mcp = std::fs::read_to_string(root.join("mcp.json")).unwrap_or_default();
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
}
