//! Multi-repo workspace registry for open GitPulse tabs.
//!
//! Wraps [`devmap_query::workspace`] so open repository tabs are recorded in
//! that repository's `.devmap/workspace.json` (or `.devcouncil/…` when that is
//! the resolved state directory) under the same advisory lock upstream uses.
//! Cross-repo symbol search and import-link candidates then call
//! [`devmap_query::workspace_search`] / [`devmap_query::link_candidates`].

use crate::codeintel::DEFAULT_CODEINTEL_BUDGET;
use crate::engine::git_cli::validate_repo;
use devmap_query::workspace::{
    name_for, workspace_path, FederatedSearch, LinkCandidate, RepoUnavailable, Workspace,
    WorkspaceRepo, WORKSPACE_VERSION,
};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// One registered repository as seen by the UI.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceRepoEntry {
    pub name: String,
    pub root: String,
    pub db: String,
    /// Absolute path to the store the registry records.
    pub db_path: String,
}

/// Snapshot of the registry file rooted at `registry_root`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceSnapshot {
    pub version: u32,
    pub registry_root: String,
    pub registry_path: String,
    pub repos: Vec<WorkspaceRepoEntry>,
}

/// Outcome of registering one repository.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceRegisterResult {
    pub name: String,
    pub root: String,
    pub replaced: bool,
    pub registry_path: String,
}

/// Outcome of removing a repository by name.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceUnregisterResult {
    pub name: String,
    pub removed: bool,
    pub registry_path: String,
}

/// Federated hit labelled with the repository it came from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceFederatedHit {
    pub repo: String,
    pub symbol_name: String,
    pub file_path: String,
    pub kind: String,
    pub span_start_line: u32,
    pub span_end_line: u32,
    pub source_span: String,
    pub source_unavailable_reason: Option<String>,
    pub score: f32,
}

/// Cross-repo search answer. Carries unavailable repos rather than dropping them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceSearchResult {
    pub items: Vec<WorkspaceFederatedHit>,
    pub repos_queried: usize,
    pub unavailable: Vec<RepoUnavailable>,
    pub total: u32,
    pub shown: u32,
    pub hidden: u32,
    pub truncated: bool,
    /// True when `semantic` was requested — TF-IDF name ranking, not AI search.
    pub semantic: bool,
}

/// Cross-repo import candidates (declared module links, not name-matched calls).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceLinksResult {
    pub links: Vec<LinkCandidate>,
    pub count: usize,
    pub repos_considered: usize,
}

fn resolve_registry_root(registry_root: &str) -> Result<PathBuf, String> {
    validate_repo(registry_root)
}

fn resolve_member_root(repo_path: &str) -> Result<PathBuf, String> {
    validate_repo(repo_path)
}

fn to_entry(repo: &WorkspaceRepo) -> WorkspaceRepoEntry {
    WorkspaceRepoEntry {
        name: repo.name.clone(),
        root: repo.root.to_string_lossy().into_owned(),
        db: repo.db.clone(),
        db_path: repo.db_path().to_string_lossy().into_owned(),
    }
}

fn snapshot_of(root: &Path, workspace: &Workspace) -> WorkspaceSnapshot {
    WorkspaceSnapshot {
        version: workspace.version,
        registry_root: root.to_string_lossy().into_owned(),
        registry_path: workspace_path(root).to_string_lossy().into_owned(),
        repos: workspace.repos.iter().map(to_entry).collect(),
    }
}

fn map_search(result: FederatedSearch, semantic: bool) -> WorkspaceSearchResult {
    WorkspaceSearchResult {
        items: result
            .items
            .into_iter()
            .map(|entry| WorkspaceFederatedHit {
                repo: entry.repo,
                symbol_name: entry.hit.symbol_name,
                file_path: entry.hit.file_path,
                kind: entry.hit.kind,
                span_start_line: entry.hit.span.0,
                span_end_line: entry.hit.span.1,
                source_span: entry.hit.source_span,
                source_unavailable_reason: entry.hit.source_unavailable_reason,
                score: entry.hit.score,
            })
            .collect(),
        repos_queried: result.repos_queried,
        unavailable: result.unavailable,
        total: result.total,
        shown: result.shown,
        hidden: result.hidden,
        truncated: result.truncated,
        semantic,
    }
}

/// Unique labels for a set of roots, widening path suffixes on collision.
///
/// Mirrors the frontend `disambiguateLabels` rule so open tabs and the registry
/// agree on names. Labels are lowercased to match [`name_for`].
fn unique_labels(roots: &[PathBuf]) -> Vec<(String, PathBuf)> {
    let mut by_leaf: HashMap<String, Vec<PathBuf>> = HashMap::new();
    for root in roots {
        let leaf = name_for(root);
        by_leaf.entry(leaf).or_default().push(root.clone());
    }

    let mut out: Vec<(String, PathBuf)> = Vec::with_capacity(roots.len());
    for (leaf, group) in by_leaf {
        if group.len() == 1 {
            out.push((leaf, group.into_iter().next().expect("len 1")));
            continue;
        }
        let mut pending = group;
        let mut claimed = HashSet::new();
        let mut depth = 2usize;
        while !pending.is_empty() {
            let mut buckets: HashMap<String, Vec<PathBuf>> = HashMap::new();
            for root in pending.drain(..) {
                let label = suffix_label(&root, depth);
                buckets.entry(label).or_default().push(root);
            }
            let mut next = Vec::new();
            for (label, members) in buckets {
                if members.len() == 1 && !claimed.contains(&label) {
                    claimed.insert(label.clone());
                    out.push((label, members.into_iter().next().expect("len 1")));
                } else {
                    next.extend(members);
                }
            }
            pending = next;
            depth = depth.saturating_add(1);
            if depth > 64 {
                // Pathological: fall back to full path strings.
                for root in pending {
                    out.push((root.to_string_lossy().to_lowercase(), root));
                }
                break;
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn suffix_label(root: &Path, depth: usize) -> String {
    let parts: Vec<_> = root
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => Some(s.to_string_lossy().to_lowercase()),
            _ => None,
        })
        .collect();
    if parts.is_empty() {
        return name_for(root);
    }
    let take = depth.min(parts.len());
    parts[parts.len() - take..].join("/")
}

/// Read the registry rooted at `registry_root` (empty when the file is absent).
pub fn list(registry_root: &str) -> Result<WorkspaceSnapshot, String> {
    let root = resolve_registry_root(registry_root)?;
    let workspace = Workspace::load(&root).map_err(|e| e.to_string())?;
    Ok(snapshot_of(&root, &workspace))
}

/// Register one repository under the registry rooted at `registry_root`.
pub fn register(
    registry_root: &str,
    repo_path: &str,
    name: Option<&str>,
) -> Result<WorkspaceRegisterResult, String> {
    let root = resolve_registry_root(registry_root)?;
    let member = resolve_member_root(repo_path)?;
    let label = name
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| name_for(&member));

    let (add_result, written) = Workspace::update(&root, |workspace| {
        workspace.add(label.clone(), member.clone())
    })
    .map_err(|e| e.to_string())?;
    let replaced = add_result.map_err(|e| e.to_string())?;

    Ok(WorkspaceRegisterResult {
        name: label,
        root: member.to_string_lossy().into_owned(),
        replaced,
        registry_path: written.to_string_lossy().into_owned(),
    })
}

/// Remove a repository by registry name.
pub fn unregister(registry_root: &str, name: &str) -> Result<WorkspaceUnregisterResult, String> {
    let label = name.trim();
    if label.is_empty() {
        return Err("name must not be blank".into());
    }
    let root = resolve_registry_root(registry_root)?;
    let (removed, written) =
        Workspace::update(&root, |workspace| workspace.remove(label)).map_err(|e| e.to_string())?;
    Ok(WorkspaceUnregisterResult {
        name: label.to_string(),
        removed,
        registry_path: written.to_string_lossy().into_owned(),
    })
}

/// Replace the registry contents so they match the open-tab set exactly.
///
/// Uses the same file lock as upstream `Workspace::update`. Names are
/// disambiguated across colliding basenames.
pub fn sync_open_tabs(
    registry_root: &str,
    repo_paths: &[String],
) -> Result<WorkspaceSnapshot, String> {
    let root = resolve_registry_root(registry_root)?;
    let mut members = Vec::with_capacity(repo_paths.len());
    let mut seen = HashSet::new();
    for path in repo_paths {
        let member = resolve_member_root(path)?;
        let key = member.to_string_lossy().to_lowercase();
        if seen.insert(key) {
            members.push(member);
        }
    }
    // Always include the registry host itself so a search from an open tab
    // never omits the repository holding the file.
    let host_key = root.to_string_lossy().to_lowercase();
    if seen.insert(host_key) {
        members.push(root.clone());
    }

    let labelled = unique_labels(&members);
    let (sync_result, written) = Workspace::update(&root, |workspace| -> Result<(), String> {
        // Build the replacement set first so a failed add cannot leave a
        // half-cleared registry on disk — `update` always saves after mutate.
        let mut next = Workspace {
            version: WORKSPACE_VERSION,
            repos: Vec::new(),
        };
        for (name, member) in &labelled {
            next.add(name.clone(), member.clone())
                .map_err(|e| e.to_string())?;
        }
        *workspace = next;
        Ok(())
    })
    .map_err(|e| e.to_string())?;
    sync_result?;

    let workspace = Workspace::load(&root).map_err(|e| e.to_string())?;
    let mut snap = snapshot_of(&root, &workspace);
    snap.registry_path = written.to_string_lossy().into_owned();
    Ok(snap)
}

/// Cross-repo symbol search over the registered roots.
///
/// When `semantic` is true, each repository ranks by TF-IDF name similarity
/// (`search_semantic`) — not an AI / embedding search.
pub fn search(
    registry_root: &str,
    query: &str,
    token_budget: Option<u32>,
    semantic: bool,
) -> Result<WorkspaceSearchResult, String> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return Err("query must not be blank".into());
    }
    let root = resolve_registry_root(registry_root)?;
    let workspace = Workspace::load(&root).map_err(|e| e.to_string())?;
    let budget = token_budget.unwrap_or(DEFAULT_CODEINTEL_BUDGET);
    let result = devmap_query::workspace_search(&workspace, trimmed, budget, semantic)
        .map_err(|e| e.to_string())?;
    Ok(map_search(result, semantic))
}

/// Declared cross-repo import links among registered repositories.
pub fn link_candidates(registry_root: &str) -> Result<WorkspaceLinksResult, String> {
    let root = resolve_registry_root(registry_root)?;
    let workspace = Workspace::load(&root).map_err(|e| e.to_string())?;
    let links = devmap_query::link_candidates(&workspace).map_err(|e| e.to_string())?;
    let count = links.len();
    Ok(WorkspaceLinksResult {
        links,
        count,
        repos_considered: workspace.repos.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;
    use std::sync::{Mutex, OnceLock};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn git_env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn scratch(tag: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("gitpulse-ws-{tag}-{}-{nanos}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("mkdir scratch");
        dir
    }

    fn init_git_repo(dir: &Path) {
        let _guard = git_env_lock().lock().expect("git env lock");
        let status = Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(dir)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .status()
            .expect("git init");
        assert!(status.success(), "git init failed in {}", dir.display());
    }

    #[test]
    fn missing_registry_lists_empty() {
        let dir = scratch("empty");
        init_git_repo(&dir);
        let snap = list(dir.to_str().unwrap()).expect("list");
        assert!(snap.repos.is_empty());
        assert_eq!(snap.version, WORKSPACE_VERSION);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn register_list_unregister_round_trip() {
        let host = scratch("host");
        let member = scratch("member");
        init_git_repo(&host);
        init_git_repo(&member);

        let added = register(
            host.to_str().unwrap(),
            member.to_str().unwrap(),
            Some("sibling"),
        )
        .expect("register");
        assert!(!added.replaced);
        assert_eq!(added.name, "sibling");

        let snap = list(host.to_str().unwrap()).expect("list");
        assert_eq!(snap.repos.len(), 1);
        assert_eq!(snap.repos[0].name, "sibling");
        assert!(Path::new(&snap.registry_path).is_file());

        let removed = unregister(host.to_str().unwrap(), "sibling").expect("unregister");
        assert!(removed.removed);
        assert!(list(host.to_str().unwrap()).unwrap().repos.is_empty());

        let _ = fs::remove_dir_all(&host);
        let _ = fs::remove_dir_all(&member);
    }

    #[test]
    fn sync_open_tabs_replaces_and_disambiguates() {
        let host = scratch("sync-host");
        let a = scratch("code").join("twin");
        let b = scratch("oss").join("twin");
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();
        init_git_repo(&host);
        init_git_repo(&a);
        init_git_repo(&b);

        let snap = sync_open_tabs(
            host.to_str().unwrap(),
            &[
                a.to_string_lossy().into_owned(),
                b.to_string_lossy().into_owned(),
            ],
        )
        .expect("sync");

        // Host is always registered alongside the open set.
        assert_eq!(snap.repos.len(), 3);
        let names: HashSet<_> = snap.repos.iter().map(|r| r.name.as_str()).collect();
        assert!(names.contains("code/twin") || names.iter().any(|n| n.contains("twin")));
        assert_eq!(names.len(), 3, "names must stay unique: {names:?}");

        // Closing everything but the host clears the extras.
        let again = sync_open_tabs(host.to_str().unwrap(), &[]).expect("resync");
        assert_eq!(again.repos.len(), 1);
        assert_eq!(
            Path::new(&again.repos[0].root).canonicalize().unwrap(),
            host.canonicalize().unwrap()
        );

        let _ = fs::remove_dir_all(&host);
        let _ = fs::remove_dir_all(a.parent().unwrap());
        let _ = fs::remove_dir_all(b.parent().unwrap());
    }

    #[test]
    fn search_blank_query_is_refused() {
        let host = scratch("search-blank");
        init_git_repo(&host);
        let err = search(host.to_str().unwrap(), "   ", None, false).unwrap_err();
        assert!(err.contains("blank"), "{err}");
        let _ = fs::remove_dir_all(&host);
    }

    #[test]
    fn search_empty_workspace_returns_zero_without_inventing_hits() {
        let host = scratch("search-empty");
        init_git_repo(&host);
        let _ = sync_open_tabs(host.to_str().unwrap(), &[]).expect("sync host");
        let found = search(host.to_str().unwrap(), "Anything", Some(500), false).expect("search");
        assert!(found.items.is_empty());
        assert_eq!(found.total, 0);
        // Host has no store yet — counted as unavailable, not as a silent zero.
        assert!(
            !found.unavailable.is_empty() || found.repos_queried == 0,
            "empty index must not look like a complete search that found nothing: {found:?}"
        );
        let _ = fs::remove_dir_all(&host);
    }

    #[test]
    fn unique_labels_widen_on_collision() {
        let labels = unique_labels(&[
            PathBuf::from("/Users/acme/code/gitpulse"),
            PathBuf::from("/Users/acme/oss/gitpulse"),
            PathBuf::from("/tmp/unique"),
        ]);
        let map: HashMap<_, _> = labels.into_iter().map(|(n, p)| (p, n)).collect();
        assert_eq!(
            map.get(Path::new("/Users/acme/code/gitpulse"))
                .map(String::as_str),
            Some("code/gitpulse")
        );
        assert_eq!(
            map.get(Path::new("/Users/acme/oss/gitpulse"))
                .map(String::as_str),
            Some("oss/gitpulse")
        );
        assert_eq!(
            map.get(Path::new("/tmp/unique")).map(String::as_str),
            Some("unique")
        );
    }

    #[test]
    fn register_refuses_non_git_directory() {
        let host = scratch("nogit-host");
        let other = scratch("nogit-member");
        init_git_repo(&host);
        // `other` is a plain directory, not a git repo.
        let err = register(host.to_str().unwrap(), other.to_str().unwrap(), None).unwrap_err();
        assert!(
            err.contains("Not a Git repository") || err.contains("Git"),
            "{err}"
        );
        let _ = fs::remove_dir_all(&host);
        let _ = fs::remove_dir_all(&other);
    }
}
