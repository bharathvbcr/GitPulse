//! Cross-repo org graph federation (`devmap federate status|impact`).
//!
//! Reads a declared multi-repo workspace manifest (`repos: [{name, root, db?}]`),
//! opens each sibling repository's SQLite store strictly read-only, refuses
//! symlinks / missing / damaged / schema-mismatched stores with explicit
//! per-repo `degraded_reason` rows (never pretending 0 callers!), and finds
//! cross-repo imports, calls, references, and namesake candidates for an
//! exported target symbol within a bounded completeness envelope.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use anyhow::{ensure, Context, Result};
use devmap_extract::model::Extraction;
use devmap_store::{Store, CURRENT_SCHEMA_VERSION};
use serde::{Deserialize, Serialize};

use crate::workspace::WORKSPACE_VERSION;

/// Default maximum hits returned by [`federate_impact`].
pub const DEFAULT_FEDERATE_LIMIT: usize = 200;
/// Hard ceiling on hits returned by one [`federate_impact`] query.
pub const MAX_FEDERATE_LIMIT: usize = 1000;

/// One repository entry in a workspace manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FederatedRepoSpec {
    pub name: String,
    pub root: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub db: Option<String>,
}

/// Multi-repo workspace manifest read by `devmap federate`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FederatedWorkspaceManifest {
    #[serde(default = "default_manifest_version")]
    pub version: u32,
    #[serde(default)]
    pub repos: Vec<FederatedRepoSpec>,
}

fn default_manifest_version() -> u32 {
    WORKSPACE_VERSION
}

impl FederatedWorkspaceManifest {
    /// Load and validate a workspace manifest from `path`.
    ///
    /// Refuses symlinks on the manifest file itself, malformed JSON, unknown
    /// future versions, and duplicate or invalid repo names.
    pub fn load_from_path(path: &Path) -> Result<Self> {
        let meta = std::fs::symlink_metadata(path)
            .with_context(|| format!("cannot inspect workspace manifest {}", path.display()))?;
        ensure!(
            !meta.file_type().is_symlink(),
            "refusing symlinked workspace manifest {}",
            path.display()
        );
        ensure!(
            meta.is_file(),
            "workspace manifest {} is not a regular file",
            path.display()
        );
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read workspace manifest {}", path.display()))?;
        let manifest: Self = serde_json::from_str(&text)
            .with_context(|| format!("{} is not a valid workspace manifest", path.display()))?;
        ensure!(
            manifest.version <= WORKSPACE_VERSION,
            "{} declares workspace version {}, which this build does not understand (max {WORKSPACE_VERSION})",
            path.display(),
            manifest.version
        );
        let mut names = BTreeSet::new();
        for repo in &manifest.repos {
            let trimmed = repo.name.trim();
            ensure!(!trimmed.is_empty(), "workspace repo name must not be empty");
            ensure!(
                !trimmed.chars().any(char::is_control),
                "workspace repo name {:?} contains control characters",
                repo.name
            );
            ensure!(
                names.insert(trimmed.to_string()),
                "duplicate repository name {trimmed:?} in workspace manifest"
            );
        }
        Ok(manifest)
    }
}

/// Explicit degradation record for a sibling repo that could not be queried.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FederatedDegradedRepo {
    pub repo: String,
    pub root: String,
    pub db_path: String,
    pub degraded_reason: String,
}

/// Per-repo health record emitted by `devmap federate status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FederatedRepoStatus {
    pub repo: String,
    pub root: String,
    pub db_path: String,
    pub healthy: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema_version: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbols: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edges: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub degraded_reason: Option<String>,
}

/// Complete envelope returned by `devmap federate status`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FederatedStatusReport {
    pub workspace_path: String,
    pub repos_total: usize,
    pub repos_queried: usize,
    pub repos_degraded: Vec<FederatedDegradedRepo>,
    pub walk_incomplete: bool,
    pub repos: Vec<FederatedRepoStatus>,
}

/// A declaration of the target exported symbol inside a workspace repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FederatedDefinition {
    pub repo: String,
    pub file_path: String,
    pub symbol_name: String,
    pub qualified_name: String,
    pub kind: String,
    pub is_exported: bool,
}

/// Classification of a cross-repo impact hit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FederatedHitKind {
    /// Sibling repo both imports a defining repo's module/symbol and calls it.
    ImportAndCall,
    /// Sibling repo both imports a defining repo's module/symbol and references it.
    ImportAndReference,
    /// Sibling repo explicitly imports a defining repo's module or symbol.
    CrossRepoImport,
    /// Sibling repo calls a matching symbol name without a proven module import.
    NamesakeCall,
    /// Sibling repo references a matching symbol name without a proven module import.
    NamesakeReference,
}

/// One cross-repo reference/import/namesake hit in a sibling repository.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FederatedImpactHit {
    pub repo: String,
    pub file_path: String,
    pub kind: FederatedHitKind,
    pub confidence: f32,
    pub matched_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enclosing_symbol: Option<String>,
    pub byte_start: usize,
    pub byte_end: usize,
    pub evidence: String,
}

/// Bounded completeness envelope returned by `devmap federate impact`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FederatedImpactReport {
    pub target: String,
    pub workspace_path: String,
    pub defining_repos: Vec<FederatedDefinition>,
    pub repos_queried: usize,
    pub repos_degraded: Vec<FederatedDegradedRepo>,
    pub walk_incomplete: bool,
    pub walk_incomplete_reasons: Vec<String>,
    pub truncated: bool,
    pub total: usize,
    pub shown: usize,
    pub hits: Vec<FederatedImpactHit>,
}

/// Policy options controlling workspace root confinement (C11).
#[derive(Debug, Clone, Copy, Default)]
pub struct FederateOptions {
    /// When `false` (default), every repository `root` in `workspace.json` MUST
    /// reside strictly beneath the workspace manifest's parent directory so an
    /// untrusted or committed `workspace.json` cannot point at arbitrary paths
    /// such as `/` or sibling user directories. Pass `true` (`--allow-external-roots`)
    /// to opt into absolute roots outside the workspace directory.
    pub allow_external_roots: bool,
}

/// Resolve a repo root against the workspace manifest's parent directory (never CWD)
/// and validate that the store path stays strictly inside the repo root with no
/// intermediate or leaf symlinks (C11).
fn resolve_repo_root(workspace_dir: &Path, raw_root: &Path) -> PathBuf {
    if raw_root.is_absolute() {
        raw_root.to_path_buf()
    } else {
        workspace_dir.join(raw_root)
    }
}

fn lexically_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push(comp.as_os_str());
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// A validated store location: the repository root, the store database path,
/// and the database file's `(dev, ino)` identity when it could be read.
type ValidatedStorePath = (PathBuf, PathBuf, Option<(u64, u64)>);

/// One repository loaded for federation: its spec, its latest extractions,
/// and the `(module, path)` pairs collected from its root.
type LoadedRepo = (FederatedRepoSpec, Vec<Extraction>, Vec<(String, String)>);

fn resolve_and_validate_store_path(
    workspace_dir: &Path,
    repo: &FederatedRepoSpec,
    opts: FederateOptions,
) -> std::result::Result<ValidatedStorePath, String> {
    let resolved_root = lexically_normalize(&resolve_repo_root(workspace_dir, &repo.root));
    if !opts.allow_external_roots {
        let base = workspace_dir
            .canonicalize()
            .unwrap_or_else(|_| lexically_normalize(workspace_dir));
        let norm_base = lexically_normalize(workspace_dir);
        let inside_lexical = resolved_root.starts_with(&norm_base);
        let inside_canon = resolved_root
            .canonicalize()
            .ok()
            .is_some_and(|c| c.starts_with(&base));
        if !inside_lexical && !inside_canon {
            return Err(format!(
                "refused: repository root {} lies outside workspace directory {} (pass --allow-external-roots to allow)",
                resolved_root.display(),
                workspace_dir.display()
            ));
        }
    }
    let root_meta = std::fs::symlink_metadata(&resolved_root).map_err(|e| {
        format!(
            "repository root {} is unreadable: {e}",
            resolved_root.display()
        )
    })?;
    if root_meta.file_type().is_symlink() {
        return Err(format!(
            "refused: repository root {} is a symlink",
            resolved_root.display()
        ));
    }
    if !root_meta.is_dir() {
        return Err(format!(
            "refused: repository root {} is not a directory",
            resolved_root.display()
        ));
    }

    let rel_db: PathBuf = match repo.db.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(spec) => {
            let candidate = Path::new(spec);
            if candidate.is_absolute() {
                // C11: keep explicit `db` strictly inside `resolved_root`.
                match candidate.strip_prefix(&resolved_root) {
                    Ok(rel) => rel.to_path_buf(),
                    Err(_) => {
                        return Err(format!(
                            "refused: store path {} escapes repository root {}",
                            candidate.display(),
                            resolved_root.display()
                        ));
                    }
                }
            } else {
                candidate.to_path_buf()
            }
        }
        None => {
            let resolved = devmap_extract::paths::store_path(&resolved_root);
            match resolved.strip_prefix(&resolved_root) {
                Ok(rel) => rel.to_path_buf(),
                Err(_) => {
                    return Err(format!(
                        "refused: resolved store path {} lies outside repository root {}",
                        resolved.display(),
                        resolved_root.display()
                    ));
                }
            }
        }
    };

    // Walk every component beneath `resolved_root` to reject `..`, root escapes,
    // and any intermediate or leaf symlinks (C11).
    let mut cur = resolved_root.clone();
    let mut saw_normal = false;
    for comp in rel_db.components() {
        match comp {
            Component::Normal(part) => {
                saw_normal = true;
                cur.push(part);
                match std::fs::symlink_metadata(&cur) {
                    Ok(meta) if meta.file_type().is_symlink() => {
                        return Err(format!(
                            "refused: store path component {} is a symlink",
                            cur.display()
                        ));
                    }
                    Ok(_) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        return Err(format!(
                            "missing store at {}",
                            resolved_root.join(&rel_db).display()
                        ));
                    }
                    Err(e) => {
                        return Err(format!("cannot inspect store path {}: {e}", cur.display()));
                    }
                }
            }
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(format!(
                    "refused: store path {} escapes repository root",
                    rel_db.display()
                ));
            }
        }
    }
    if !saw_normal {
        return Err("refused: empty store path".to_string());
    }

    let db_meta = std::fs::symlink_metadata(&cur).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            format!("missing store at {}", cur.display())
        } else {
            format!("cannot inspect store at {}: {e}", cur.display())
        }
    })?;
    if db_meta.file_type().is_symlink() {
        return Err(format!("refused: store at {} is a symlink", cur.display()));
    }
    if !db_meta.is_file() {
        return Err(format!(
            "refused: store at {} is not a regular file",
            cur.display()
        ));
    }
    #[cfg(unix)]
    let dev_ino = {
        use std::os::unix::fs::MetadataExt;
        Some((db_meta.dev(), db_meta.ino()))
    };
    #[cfg(not(unix))]
    let dev_ino = None;

    Ok((resolved_root, cur, dev_ino))
}

struct OpenedRepo {
    spec: FederatedRepoSpec,
    resolved_root: PathBuf,
    db_path: PathBuf,
    store: Store,
    status: devmap_store::StoreStatus,
    staleness_reason: Option<String>,
}

fn open_repo_readonly(
    workspace_dir: &Path,
    repo: &FederatedRepoSpec,
    opts: FederateOptions,
) -> std::result::Result<OpenedRepo, FederatedDegradedRepo> {
    let resolved_fallback_root = lexically_normalize(&resolve_repo_root(workspace_dir, &repo.root));
    let fallback_db = repo
        .db
        .as_deref()
        .map(|rel| {
            let p = Path::new(rel);
            if p.is_absolute() {
                p.to_path_buf()
            } else {
                resolved_fallback_root.join(p)
            }
        })
        .unwrap_or_else(|| devmap_extract::paths::store_path(&resolved_fallback_root));

    let (resolved_root, db_path, expected_dev_ino) =
        match resolve_and_validate_store_path(workspace_dir, repo, opts) {
            Ok(triple) => triple,
            Err(reason) => {
                return Err(FederatedDegradedRepo {
                    repo: repo.name.clone(),
                    root: resolved_fallback_root.display().to_string(),
                    db_path: fallback_db.display().to_string(),
                    degraded_reason: reason,
                });
            }
        };

    let store = match Store::open_read_only(&db_path) {
        Ok(s) => s,
        Err(err) => {
            return Err(FederatedDegradedRepo {
                repo: repo.name.clone(),
                root: resolved_root.display().to_string(),
                db_path: db_path.display().to_string(),
                degraded_reason: format!("store open refused or damaged: {err}"),
            });
        }
    };

    // C11 TOCTOU check: re-stat leaf after open and confirm (dev, ino) matches
    // the symlink-free walked leaf (not swapped to a symlink or different inode).
    #[cfg(unix)]
    if let Some((exp_dev, exp_ino)) = expected_dev_ino {
        use std::os::unix::fs::MetadataExt;
        match std::fs::symlink_metadata(&db_path) {
            Ok(after)
                if !after.file_type().is_symlink()
                    && after.is_file()
                    && after.dev() == exp_dev
                    && after.ino() == exp_ino => {}
            _ => {
                return Err(FederatedDegradedRepo {
                    repo: repo.name.clone(),
                    root: resolved_root.display().to_string(),
                    db_path: db_path.display().to_string(),
                    degraded_reason: "refused: store inode changed during read-only open (TOCTOU)"
                        .to_string(),
                });
            }
        }
    }
    #[cfg(not(unix))]
    let _ = expected_dev_ino;

    let status = match store.status(&db_path.display().to_string()) {
        Ok(st) => st,
        Err(err) => {
            return Err(FederatedDegradedRepo {
                repo: repo.name.clone(),
                root: resolved_root.display().to_string(),
                db_path: db_path.display().to_string(),
                degraded_reason: format!("store status unreadable or damaged: {err}"),
            });
        }
    };

    if status.latest_generation.is_none() {
        return Err(FederatedDegradedRepo {
            repo: repo.name.clone(),
            root: resolved_root.display().to_string(),
            db_path: db_path.display().to_string(),
            degraded_reason: "store has no indexed generation (run `devmap build`)".to_string(),
        });
    }

    // C10: surface ANY degradation or staleness honestly (never treat stale or
    // partially-degraded stores as complete/healthy).
    let mut staleness_parts: Vec<String> = Vec::new();
    if let Some(reason) = &status.degraded_reason {
        let lower = reason.to_ascii_lowercase();
        if lower.contains("damaged") || lower.contains("corrupt") {
            return Err(FederatedDegradedRepo {
                repo: repo.name.clone(),
                root: resolved_root.display().to_string(),
                db_path: db_path.display().to_string(),
                degraded_reason: format!("store is damaged: {reason}"),
            });
        }
        staleness_parts.push(reason.clone());
    }
    if status.pending_count > 0 {
        staleness_parts.push(format!(
            "{} pending unindexed change(s)",
            status.pending_count
        ));
    }
    if status.source_freshness == Some(false) {
        staleness_parts.push("index is stale relative to working tree".to_string());
    }
    let staleness_reason = (!staleness_parts.is_empty()).then(|| staleness_parts.join("; "));

    Ok(OpenedRepo {
        spec: repo.clone(),
        resolved_root,
        db_path,
        store,
        status,
        staleness_reason,
    })
}

/// Inspect every repository in `workspace_path` and report health/degradation.
pub fn federate_status(workspace_path: &Path) -> Result<FederatedStatusReport> {
    federate_status_with_opts(workspace_path, FederateOptions::default())
}

/// Inspect every repository in `workspace_path` with explicit root-confinement options.
pub fn federate_status_with_opts(
    workspace_path: &Path,
    opts: FederateOptions,
) -> Result<FederatedStatusReport> {
    let manifest = FederatedWorkspaceManifest::load_from_path(workspace_path)?;
    let workspace_dir = workspace_path.parent().unwrap_or_else(|| Path::new("."));
    let mut repos = Vec::with_capacity(manifest.repos.len());
    let mut degraded = Vec::new();
    let mut queried = 0usize;

    for spec in &manifest.repos {
        match open_repo_readonly(workspace_dir, spec, opts) {
            Ok(opened) => {
                queried += 1;
                let healthy = opened.staleness_reason.is_none();
                if let Some(reason) = &opened.staleness_reason {
                    degraded.push(FederatedDegradedRepo {
                        repo: opened.spec.name.clone(),
                        root: opened.resolved_root.display().to_string(),
                        db_path: opened.db_path.display().to_string(),
                        degraded_reason: reason.clone(),
                    });
                }
                repos.push(FederatedRepoStatus {
                    repo: opened.spec.name,
                    root: opened.resolved_root.display().to_string(),
                    db_path: opened.db_path.display().to_string(),
                    healthy,
                    schema_version: Some(CURRENT_SCHEMA_VERSION),
                    generation: opened.status.latest_generation,
                    symbols: Some(opened.status.node_count as u64),
                    edges: Some(opened.status.edge_count as u64),
                    degraded_reason: opened.staleness_reason,
                });
            }
            Err(deg) => {
                repos.push(FederatedRepoStatus {
                    repo: deg.repo.clone(),
                    root: deg.root.clone(),
                    db_path: deg.db_path.clone(),
                    healthy: false,
                    schema_version: None,
                    generation: None,
                    symbols: None,
                    edges: None,
                    degraded_reason: Some(deg.degraded_reason.clone()),
                });
                degraded.push(deg);
            }
        }
    }

    let walk_incomplete = !degraded.is_empty() || queried == 0;
    Ok(FederatedStatusReport {
        workspace_path: workspace_path.display().to_string(),
        repos_total: manifest.repos.len(),
        repos_queried: queried,
        repos_degraded: degraded,
        walk_incomplete,
        repos,
    })
}

fn bare_symbol_name(target: &str) -> &str {
    let trimmed = target.trim();
    if let Some((_, tail)) = trimmed.rsplit_once("::") {
        if !tail.is_empty() {
            return tail;
        }
    }
    if let Some((_, tail)) = trimmed.rsplit_once('.') {
        if !tail.is_empty() {
            return tail;
        }
    }
    trimmed
}

fn name_matches_target(candidate: &str, full_target: &str, bare: &str) -> bool {
    let c = candidate.trim();
    if c.is_empty() {
        return false;
    }
    if c == full_target || c == bare {
        return true;
    }
    c.strip_suffix(bare).is_some_and(|prefix| {
        prefix.ends_with("::") || prefix.ends_with('.') || prefix.ends_with('/')
    })
}

/// Collect module prefixes declared by a repository (Go `go.mod`, Python
/// packages, Rust `Cargo.toml` crate names, JS/TS `package.json` name).
fn collect_repo_modules(root: &Path) -> Vec<(String, String)> {
    let mut modules = Vec::new();
    if let Ok(manifests) = devmap_extract::collect_project_manifests(root) {
        for m in manifests.go_modules {
            if !m.prefix.is_empty() {
                let src = if m.dir.is_empty() {
                    "go.mod".to_string()
                } else {
                    format!("{}/go.mod", m.dir)
                };
                modules.push((m.prefix.clone(), format!("{src} declares `{}`", m.prefix)));
            }
        }
    }
    // Python top-level / src packages
    for base in [root.to_path_buf(), root.join("src")] {
        if let Ok(entries) = std::fs::read_dir(&base) {
            for entry in entries.flatten() {
                if entry.path().join("__init__.py").is_file() {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    if !name.starts_with('.') {
                        modules.push((name.clone(), format!("Python package `{name}`")));
                    }
                }
            }
        }
    }
    // Rust Cargo.toml [package] name
    let cargo_toml = root.join("Cargo.toml");
    if let Ok(text) = std::fs::read_to_string(&cargo_toml) {
        let mut in_pkg = false;
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with('[') {
                in_pkg = trimmed == "[package]";
                continue;
            }
            if in_pkg {
                if let Some(rest) = trimmed.strip_prefix("name") {
                    let rest = rest.trim_start();
                    if let Some(rest) = rest.strip_prefix('=') {
                        let val = rest.trim().trim_matches('"');
                        if !val.is_empty() {
                            modules.push((val.to_string(), format!("Cargo.toml crate `{val}`")));
                            let normalized = val.replace('-', "_");
                            if normalized != val {
                                modules.push((
                                    normalized.clone(),
                                    format!("Cargo.toml crate `{val}` (ident `{normalized}`)"),
                                ));
                            }
                            break;
                        }
                    }
                }
            }
        }
    }
    // JS/TS package.json name
    let pkg_json = root.join("package.json");
    if let Ok(text) = std::fs::read_to_string(&pkg_json) {
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(&text) {
            if let Some(name) = val.get("name").and_then(|v| v.as_str()) {
                if !name.is_empty() {
                    modules.push((name.to_string(), format!("package.json `{name}`")));
                }
            }
        }
    }
    modules.sort();
    modules.dedup();
    modules
}

fn specifier_matches_module(specifier: &str, prefix: &str) -> bool {
    if specifier == prefix {
        return true;
    }
    specifier.strip_prefix(prefix).is_some_and(|rest| {
        rest.starts_with('/') || rest.starts_with('.') || rest.starts_with("::")
    })
}

/// Find cross-repo references, imports, and namesakes matching `target` across
/// sibling repositories declared in `workspace_path`.
pub fn federate_impact(
    workspace_path: &Path,
    target: &str,
    source_repo: Option<&str>,
    limit: usize,
) -> Result<FederatedImpactReport> {
    federate_impact_with_opts(
        workspace_path,
        target,
        source_repo,
        limit,
        FederateOptions::default(),
    )
}

/// Find cross-repo references, imports, and namesakes matching `target` across
/// sibling repositories declared in `workspace_path` with explicit root-confinement options.
pub fn federate_impact_with_opts(
    workspace_path: &Path,
    target: &str,
    source_repo: Option<&str>,
    limit: usize,
    opts: FederateOptions,
) -> Result<FederatedImpactReport> {
    let target = target.trim();
    ensure!(!target.is_empty(), "target symbol must not be empty");
    let bare = bare_symbol_name(target);
    let capped_limit = limit.clamp(1, MAX_FEDERATE_LIMIT);

    let manifest = FederatedWorkspaceManifest::load_from_path(workspace_path)?;
    if let Some(src) = source_repo {
        ensure!(
            manifest.repos.iter().any(|r| r.name == src),
            "--source-repo {src:?} is not declared in {}",
            workspace_path.display()
        );
    }

    let workspace_dir = workspace_path.parent().unwrap_or_else(|| Path::new("."));
    let mut repos_degraded: Vec<FederatedDegradedRepo> = Vec::new();
    let mut loaded: Vec<LoadedRepo> = Vec::new();

    for spec in &manifest.repos {
        match open_repo_readonly(workspace_dir, spec, opts) {
            Ok(opened) => {
                if let Some(stale_reason) = opened.staleness_reason {
                    repos_degraded.push(FederatedDegradedRepo {
                        repo: opened.spec.name.clone(),
                        root: opened.resolved_root.display().to_string(),
                        db_path: opened.db_path.display().to_string(),
                        degraded_reason: stale_reason,
                    });
                }
                match opened.store.latest_extractions() {
                    Ok(extractions) => {
                        let modules = collect_repo_modules(&opened.resolved_root);
                        loaded.push((opened.spec, extractions, modules));
                    }
                    Err(err) => {
                        repos_degraded.push(FederatedDegradedRepo {
                            repo: opened.spec.name,
                            root: opened.resolved_root.display().to_string(),
                            db_path: opened.db_path.display().to_string(),
                            degraded_reason: format!(
                                "stored extractions could not be decoded: {err}"
                            ),
                        });
                    }
                }
            }
            Err(deg) => repos_degraded.push(deg),
        }
    }

    let repos_queried = loaded.len();

    // Step 1: Identify defining repositories for the target symbol.
    let mut defining_repos: Vec<FederatedDefinition> = Vec::new();
    let mut defining_repo_names: BTreeSet<String> = BTreeSet::new();
    let mut provider_modules: Vec<(String, String, String)> = Vec::new(); // (repo, prefix, evidence)

    for (spec, extractions, modules) in &loaded {
        if let Some(src) = source_repo {
            if spec.name != src {
                continue;
            }
        }
        let mut repo_defines = false;
        for ext in extractions {
            let mut seen_def_names: BTreeSet<String> = BTreeSet::new();
            for sym in &ext.symbols {
                if sym.is_exported
                    && (name_matches_target(&sym.name, target, bare)
                        || name_matches_target(&sym.qualified_name, target, bare))
                {
                    repo_defines = true;
                    seen_def_names.insert(sym.name.clone());
                    defining_repos.push(FederatedDefinition {
                        repo: spec.name.clone(),
                        file_path: ext.file_path.clone(),
                        symbol_name: sym.name.clone(),
                        qualified_name: sym.qualified_name.clone(),
                        kind: sym.kind.as_str().to_string(),
                        is_exported: sym.is_exported,
                    });
                }
            }
            for exp in &ext.exports {
                if name_matches_target(&exp.exported_name, target, bare)
                    && !seen_def_names.contains(&exp.exported_name)
                {
                    repo_defines = true;
                    seen_def_names.insert(exp.exported_name.clone());
                    defining_repos.push(FederatedDefinition {
                        repo: spec.name.clone(),
                        file_path: ext.file_path.clone(),
                        symbol_name: exp.exported_name.clone(),
                        qualified_name: format!("{}::{}", ext.file_path, exp.exported_name),
                        kind: "Export".to_string(),
                        is_exported: true,
                    });
                }
            }
        }
        if repo_defines {
            defining_repo_names.insert(spec.name.clone());
            for (prefix, evidence) in modules {
                provider_modules.push((spec.name.clone(), prefix.clone(), evidence.clone()));
            }
        }
    }

    // Step 2: Scan sibling repositories for imports, calls, references, and namesakes.
    let mut hits: Vec<FederatedImpactHit> = Vec::new();
    let mut skipped_dynamic_imports: usize = 0;
    let mut skipped_reexports: usize = 0;
    let mut suppressed_multi_definer_files: usize = 0;

    for (spec, extractions, _) in &loaded {
        let is_defining_repo = defining_repo_names.contains(&spec.name);
        // Skip the explicit --source-repo or the sole defining repository.
        // When 2+ repos define the symbol, a defining repo only emits hits if a
        // file explicitly imports a *different* defining repo's module (C10).
        if let Some(src) = source_repo {
            if spec.name == src {
                continue;
            }
        } else if defining_repo_names.len() == 1 && is_defining_repo {
            continue;
        }

        for ext in extractions {
            // Note re-exports referencing another module (not transitively followed).
            for exp in &ext.exports {
                if exp.module_specifier.is_some()
                    && (name_matches_target(&exp.exported_name, target, bare)
                        || exp.exported_name == "*")
                {
                    skipped_reexports += 1;
                }
            }

            // Determine whether this file imports a defining repo's module or the target symbol.
            let mut file_import_evidence: Option<String> = None;
            let mut explicit_symbol_import = false;
            let mut imported_aliases: Vec<String> = Vec::new();

            for imp in &ext.imports {
                if imp.path_load.is_some() {
                    skipped_dynamic_imports += 1;
                    continue;
                }
                let specifier = imp.module_specifier.trim();
                let imports_target_name = imp
                    .imported_names
                    .iter()
                    .any(|n| name_matches_target(n, target, bare))
                    || name_matches_target(specifier, target, bare);

                let mut matched_provider: Option<(&str, &str)> = None;
                for (prov_repo, prefix, ev) in &provider_modules {
                    if prov_repo != &spec.name && specifier_matches_module(specifier, prefix) {
                        matched_provider = Some((prov_repo.as_str(), ev.as_str()));
                        break;
                    }
                }

                // Track local aliases for imported target symbol (e.g. `from m import Foo as Bar`).
                if imports_target_name {
                    for (orig, local) in imp.imported_names.iter().zip(imp.local_names.iter()) {
                        if name_matches_target(orig, target, bare)
                            && !local.is_empty()
                            && local != bare
                        {
                            imported_aliases.push(local.clone());
                        }
                    }
                    if let Some(alias) = &imp.alias {
                        if !alias.is_empty() && alias != bare {
                            imported_aliases.push(alias.clone());
                        }
                    }
                }

                if let Some((prov_repo, ev)) = matched_provider {
                    let detail = format!("imports `{specifier}` from `{prov_repo}` ({ev})");
                    if file_import_evidence.is_none() {
                        file_import_evidence = Some(detail.clone());
                    }
                    if imports_target_name || !specifier.is_empty() {
                        if imports_target_name {
                            explicit_symbol_import = true;
                        }
                        hits.push(FederatedImpactHit {
                            repo: spec.name.clone(),
                            file_path: ext.file_path.clone(),
                            kind: FederatedHitKind::CrossRepoImport,
                            confidence: if imports_target_name { 0.85 } else { 0.75 },
                            matched_name: if imports_target_name {
                                bare.to_string()
                            } else {
                                specifier.to_string()
                            },
                            enclosing_symbol: None,
                            byte_start: imp.span.start_byte,
                            byte_end: imp.span.end_byte,
                            evidence: detail,
                        });
                    }
                } else if imports_target_name && !is_defining_repo {
                    explicit_symbol_import = true;
                    let detail = format!("imports `{bare}` via `{specifier}`");
                    if file_import_evidence.is_none() {
                        file_import_evidence = Some(detail.clone());
                    }
                    hits.push(FederatedImpactHit {
                        repo: spec.name.clone(),
                        file_path: ext.file_path.clone(),
                        kind: FederatedHitKind::CrossRepoImport,
                        confidence: 0.70,
                        matched_name: bare.to_string(),
                        enclosing_symbol: None,
                        byte_start: imp.span.start_byte,
                        byte_end: imp.span.end_byte,
                        evidence: detail,
                    });
                }
            }

            // C10: When a repo also defines the symbol itself, do not misreport
            // intra-repo calls/refs as cross-repo unless the file imports another
            // defining repo's module — and record that suppression honestly.
            if is_defining_repo && file_import_evidence.is_none() {
                if ext
                    .calls
                    .iter()
                    .any(|c| name_matches_target(&c.callee_name, target, bare))
                    || ext
                        .references
                        .iter()
                        .any(|r| name_matches_target(&r.name, target, bare))
                {
                    suppressed_multi_definer_files += 1;
                }
                continue;
            }

            let matches_name_or_alias = |candidate: &str| -> bool {
                name_matches_target(candidate, target, bare)
                    || imported_aliases
                        .iter()
                        .any(|alias| name_matches_target(candidate, alias, alias))
            };

            // Calls matching target (or aliased import of target)
            for call in &ext.calls {
                if matches_name_or_alias(&call.callee_name) {
                    let (kind, confidence, evidence) = if let Some(imp_ev) = &file_import_evidence {
                        (
                            FederatedHitKind::ImportAndCall,
                            if explicit_symbol_import { 0.90 } else { 0.85 },
                            format!("calls `{}` in file that {imp_ev}", call.callee_name),
                        )
                    } else {
                        (
                            FederatedHitKind::NamesakeCall,
                            0.45,
                            format!(
                                "namesake call `{}` without a verified cross-repo module import",
                                call.callee_name
                            ),
                        )
                    };
                    hits.push(FederatedImpactHit {
                        repo: spec.name.clone(),
                        file_path: ext.file_path.clone(),
                        kind,
                        confidence,
                        matched_name: call.callee_name.clone(),
                        enclosing_symbol: call.caller_symbol.clone(),
                        byte_start: call.span.start_byte,
                        byte_end: call.span.end_byte,
                        evidence,
                    });
                }
            }

            // Non-call references matching target (or aliased import of target)
            for reference in &ext.references {
                if matches_name_or_alias(&reference.name) {
                    let (kind, confidence, evidence) = if let Some(imp_ev) = &file_import_evidence {
                        (
                            FederatedHitKind::ImportAndReference,
                            0.85,
                            format!(
                                "references `{}` ({:?}) in file that {imp_ev}",
                                reference.name, reference.kind
                            ),
                        )
                    } else {
                        (
                            FederatedHitKind::NamesakeReference,
                            0.45,
                            format!(
                                "namesake reference `{}` ({:?}) without a verified cross-repo module import",
                                reference.name, reference.kind
                            ),
                        )
                    };
                    hits.push(FederatedImpactHit {
                        repo: spec.name.clone(),
                        file_path: ext.file_path.clone(),
                        kind,
                        confidence,
                        matched_name: reference.name.clone(),
                        enclosing_symbol: reference.enclosing_symbol.clone(),
                        byte_start: reference.span.start_byte,
                        byte_end: reference.span.end_byte,
                        evidence,
                    });
                }
            }
        }
    }

    // Deduplicate identical (repo, file_path, byte_start, byte_end, matched_name), keeping highest confidence.
    let mut deduped: BTreeMap<(String, String, usize, usize, String), FederatedImpactHit> =
        BTreeMap::new();
    for hit in hits {
        let key = (
            hit.repo.clone(),
            hit.file_path.clone(),
            hit.byte_start,
            hit.byte_end,
            hit.matched_name.clone(),
        );
        match deduped.get(&key) {
            Some(existing) if existing.confidence >= hit.confidence => {}
            _ => {
                deduped.insert(key, hit);
            }
        }
    }

    let mut all_hits: Vec<FederatedImpactHit> = deduped.into_values().collect();
    all_hits.sort_by(|a, b| {
        b.confidence
            .total_cmp(&a.confidence)
            .then_with(|| a.repo.cmp(&b.repo))
            .then_with(|| a.file_path.cmp(&b.file_path))
            .then_with(|| a.byte_start.cmp(&b.byte_start))
            .then_with(|| a.kind.cmp(&b.kind))
            .then_with(|| a.matched_name.cmp(&b.matched_name))
    });

    defining_repos.sort_by(|a, b| {
        (&a.repo, &a.file_path, &a.qualified_name).cmp(&(&b.repo, &b.file_path, &b.qualified_name))
    });

    let total = all_hits.len();
    let truncated = total > capped_limit;
    if truncated {
        all_hits.truncate(capped_limit);
    }
    let shown = all_hits.len();

    let mut walk_incomplete_reasons = Vec::new();
    if repos_queried == 0 {
        walk_incomplete_reasons.push("zero workspace repositories were queried".to_string());
    }
    if !repos_degraded.is_empty() {
        walk_incomplete_reasons.push(format!(
            "{} declared repository store(s) were degraded, stale, or unreadable",
            repos_degraded.len()
        ));
    }
    if defining_repos.is_empty() {
        walk_incomplete_reasons.push(
            "no exported definition of target symbol was found in any queried workspace repository (hits are namesake candidates only)"
                .to_string(),
        );
    }
    if skipped_dynamic_imports > 0 {
        walk_incomplete_reasons.push(format!(
            "skipped {skipped_dynamic_imports} dynamic path-loaded import(s) that cannot be resolved statically across repositories"
        ));
    }
    if skipped_reexports > 0 {
        walk_incomplete_reasons.push(format!(
            "encountered {skipped_reexports} re-export(s) that are not transitively followed across repositories"
        ));
    }
    if suppressed_multi_definer_files > 0 {
        walk_incomplete_reasons.push(format!(
            "suppressed namesake references in {suppressed_multi_definer_files} file(s) inside co-defining repositories lacking an explicit cross-repo import"
        ));
    }
    if truncated {
        walk_incomplete_reasons.push(format!(
            "result set truncated to {shown} of {total} hits (limit={capped_limit})"
        ));
    }
    let walk_incomplete = !walk_incomplete_reasons.is_empty();

    Ok(FederatedImpactReport {
        target: target.to_string(),
        workspace_path: workspace_path.display().to_string(),
        defining_repos,
        repos_queried,
        repos_degraded,
        walk_incomplete,
        walk_incomplete_reasons,
        truncated,
        total,
        shown,
        hits: all_hits,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static SEQ: AtomicU64 = AtomicU64::new(0);

    fn scratch(name: &str) -> PathBuf {
        let id = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "devmap-federate-test-{name}-{}-{id}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn degraded_stores_are_reported_never_silently_zero() {
        let root = scratch("degraded");
        let missing_repo = root.join("missing-store-repo");
        let damaged_repo = root.join("damaged-store-repo");
        let bad_schema_repo = root.join("bad-schema-repo");
        std::fs::create_dir_all(&missing_repo).unwrap();
        std::fs::create_dir_all(&damaged_repo).unwrap();
        std::fs::create_dir_all(&bad_schema_repo).unwrap();

        // Damaged store: random non-SQLite bytes
        let damaged_db = devmap_extract::paths::store_path(&damaged_repo);
        std::fs::create_dir_all(damaged_db.parent().unwrap()).unwrap();
        std::fs::write(&damaged_db, b"not a sqlite database").unwrap();

        // Schema-mismatched store: valid SQLite with user_version = 1
        let bad_schema_db = devmap_extract::paths::store_path(&bad_schema_repo);
        std::fs::create_dir_all(bad_schema_db.parent().unwrap()).unwrap();
        {
            let conn = rusqlite::Connection::open(&bad_schema_db).unwrap();
            conn.execute_batch("PRAGMA user_version = 1; CREATE TABLE t(x INT);")
                .unwrap();
        }

        // Only the Unix symlink case below adds to it.
        #[cfg_attr(not(unix), allow(unused_mut))]
        let mut repos = vec![
            serde_json::json!({"name": "missing", "root": missing_repo}),
            serde_json::json!({"name": "damaged", "root": damaged_repo}),
            serde_json::json!({"name": "bad-schema", "root": bad_schema_repo}),
        ];

        #[cfg(unix)]
        let symlink_repo = {
            let sr = root.join("symlink-store-repo");
            std::fs::create_dir_all(&sr).unwrap();
            let sdb = devmap_extract::paths::store_path(&sr);
            std::fs::create_dir_all(sdb.parent().unwrap()).unwrap();
            std::os::unix::fs::symlink(&bad_schema_db, &sdb).unwrap();
            repos.push(serde_json::json!({"name": "symlinked", "root": sr}));
            sr
        };
        #[cfg(not(unix))]
        let _ = ();

        let ws_path = root.join("workspace.json");
        std::fs::write(
            &ws_path,
            serde_json::to_string_pretty(&serde_json::json!({
                "version": 1,
                "repos": repos,
            }))
            .unwrap(),
        )
        .unwrap();

        let status = federate_status(&ws_path).unwrap();
        assert_eq!(status.repos_queried, 0);
        assert!(status.walk_incomplete);
        #[cfg(unix)]
        {
            let _ = symlink_repo;
            assert_eq!(status.repos_degraded.len(), 4);
            let sym_deg = status
                .repos_degraded
                .iter()
                .find(|d| d.repo == "symlinked")
                .expect("symlinked repo must be degraded");
            assert!(
                sym_deg.degraded_reason.contains("symlink"),
                "{}",
                sym_deg.degraded_reason
            );
        }

        let impact = federate_impact(&ws_path, "ComputeScore", None, 50).unwrap();
        assert_eq!(impact.repos_queried, 0);
        assert!(impact.walk_incomplete);
        assert!(!impact.repos_degraded.is_empty());

        // C11: relative root resolves against workspace.json directory, and absolute db outside root is refused.
        let outside_db = root.join("outside.sqlite");
        std::fs::write(&outside_db, b"dummy").unwrap();
        let ws_rel = root.join("workspace-rel.json");
        std::fs::write(
            &ws_rel,
            serde_json::to_string_pretty(&serde_json::json!({
                "version": 1,
                "repos": [
                    {"name": "escaping-db", "root": "missing-store-repo", "db": outside_db}
                ]
            }))
            .unwrap(),
        )
        .unwrap();
        let rel_status = federate_status(&ws_rel).unwrap();
        assert_eq!(rel_status.repos_degraded.len(), 1);
        assert!(
            rel_status.repos_degraded[0]
                .degraded_reason
                .contains("escapes repository root"),
            "{}",
            rel_status.repos_degraded[0].degraded_reason
        );

        // C11: external absolute root (e.g. "/") is refused unless allow_external_roots=true.
        let ws_ext = root.join("workspace-ext.json");
        std::fs::write(
            &ws_ext,
            serde_json::to_string_pretty(&serde_json::json!({
                "version": 1,
                "repos": [
                    {"name": "host-root", "root": "/"}
                ]
            }))
            .unwrap(),
        )
        .unwrap();
        let ext_status = federate_status(&ws_ext).unwrap();
        assert_eq!(ext_status.repos_degraded.len(), 1);
        assert!(
            ext_status.repos_degraded[0]
                .degraded_reason
                .contains("lies outside workspace directory"),
            "{}",
            ext_status.repos_degraded[0].degraded_reason
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(feature = "parse")]
    #[test]
    fn cross_repo_impact_finds_imports_calls_and_namesakes_across_two_indexed_repos() {
        let root = scratch("two-repos");
        let lib_repo = root.join("billing-lib");
        let svc_repo = root.join("checkout-svc");
        let extra_repo = root.join("analytics-svc");
        std::fs::create_dir_all(&lib_repo).unwrap();
        std::fs::create_dir_all(&svc_repo).unwrap();
        std::fs::create_dir_all(&extra_repo).unwrap();

        // Repo 1: Go library exporting CalculateInvoiceTotal
        std::fs::write(
            lib_repo.join("go.mod"),
            "module acme.dev/billing\n\ngo 1.22\n",
        )
        .unwrap();
        let lib_src =
            "package billing\n\nfunc CalculateInvoiceTotal(cents int) int { return cents }\n";
        std::fs::write(lib_repo.join("invoice.go"), lib_src).unwrap();
        {
            let db = devmap_extract::paths::store_path(&lib_repo);
            std::fs::create_dir_all(db.parent().unwrap()).unwrap();
            let store = Store::open(&db).unwrap();
            let exts = devmap_extract::extract_tree(&lib_repo).unwrap();
            let res = devmap_resolve::Resolver::new().resolve_all(&exts).unwrap();
            let analysis = devmap_analyze::analyze(&exts, &res);
            store
                .save_generation_with_opts(
                    &exts,
                    &res,
                    &analysis,
                    devmap_store::GenerationWriteOpts {
                        repo_root: Some(lib_repo.display().to_string()),
                        ..Default::default()
                    },
                )
                .unwrap();
        }

        // Repo 2: Go consumer importing acme.dev/billing and calling CalculateInvoiceTotal
        std::fs::write(
            svc_repo.join("go.mod"),
            "module acme.dev/checkout\n\ngo 1.22\n",
        )
        .unwrap();
        let svc_src = "package checkout\n\nimport \"acme.dev/billing\"\n\nfunc Charge() int { return billing.CalculateInvoiceTotal(100) }\n";
        std::fs::write(svc_repo.join("charge.go"), svc_src).unwrap();
        {
            let db = devmap_extract::paths::store_path(&svc_repo);
            std::fs::create_dir_all(db.parent().unwrap()).unwrap();
            let store = Store::open(&db).unwrap();
            let exts = devmap_extract::extract_tree(&svc_repo).unwrap();
            let res = devmap_resolve::Resolver::new().resolve_all(&exts).unwrap();
            let analysis = devmap_analyze::analyze(&exts, &res);
            store
                .save_generation_with_opts(
                    &exts,
                    &res,
                    &analysis,
                    devmap_store::GenerationWriteOpts {
                        repo_root: Some(svc_repo.display().to_string()),
                        ..Default::default()
                    },
                )
                .unwrap();
        }

        // Repo 3: Python service calling namesake CalculateInvoiceTotal without Go import
        let extra_src = "def report():\n    return CalculateInvoiceTotal(42)\n";
        std::fs::write(extra_repo.join("report.py"), extra_src).unwrap();
        {
            let db = devmap_extract::paths::store_path(&extra_repo);
            std::fs::create_dir_all(db.parent().unwrap()).unwrap();
            let store = Store::open(&db).unwrap();
            let exts = devmap_extract::extract_tree(&extra_repo).unwrap();
            let res = devmap_resolve::Resolver::new().resolve_all(&exts).unwrap();
            let analysis = devmap_analyze::analyze(&exts, &res);
            store
                .save_generation_with_opts(
                    &exts,
                    &res,
                    &analysis,
                    devmap_store::GenerationWriteOpts {
                        repo_root: Some(extra_repo.display().to_string()),
                        ..Default::default()
                    },
                )
                .unwrap();
        }

        let ws_path = root.join("workspace.json");
        std::fs::write(
            &ws_path,
            serde_json::to_string_pretty(&serde_json::json!({
                "version": 1,
                "repos": [
                    {"name": "billing-lib", "root": lib_repo},
                    {"name": "checkout-svc", "root": svc_repo},
                    {"name": "analytics-svc", "root": extra_repo},
                ]
            }))
            .unwrap(),
        )
        .unwrap();

        let status = federate_status(&ws_path).unwrap();
        assert_eq!(status.repos_queried, 3);
        assert!(
            status.repos_degraded.is_empty(),
            "unexpected degraded repos: {:?}",
            status.repos_degraded
        );
        assert!(!status.walk_incomplete);

        let report = federate_impact(&ws_path, "CalculateInvoiceTotal", None, 200).unwrap();
        assert_eq!(report.repos_queried, 3);
        assert!(report.repos_degraded.is_empty());
        assert!(!report.walk_incomplete);
        assert!(!report.truncated);
        assert_eq!(report.defining_repos.len(), 1);
        assert_eq!(report.defining_repos[0].repo, "billing-lib");
        assert!(
            report.hits.iter().any(|h| h.repo == "checkout-svc"
                && matches!(
                    h.kind,
                    FederatedHitKind::ImportAndCall | FederatedHitKind::CrossRepoImport
                )),
            "expected cross-repo import/call hit in checkout-svc: {:?}",
            report.hits
        );
        assert!(
            report
                .hits
                .iter()
                .any(|h| h.repo == "analytics-svc" && h.kind == FederatedHitKind::NamesakeCall),
            "expected namesake hit in analytics-svc: {:?}",
            report.hits
        );

        // Verify bounded truncation flag
        let capped = federate_impact(&ws_path, "CalculateInvoiceTotal", None, 1).unwrap();
        assert!(capped.truncated);
        assert!(capped.walk_incomplete);
        assert_eq!(capped.shown, 1);
        assert!(capped.total > 1);

        // C10: mutating a tracked file makes the store stale and MUST set walk_incomplete = true
        std::fs::write(
            svc_repo.join("charge.go"),
            "package checkout\n\nimport \"acme.dev/billing\"\n\nfunc Charge() int { return billing.CalculateInvoiceTotal(200) }\n",
        )
        .unwrap();
        let stale_impact = federate_impact(&ws_path, "CalculateInvoiceTotal", None, 200).unwrap();
        assert!(
            stale_impact.walk_incomplete,
            "stale sibling store must mark walk_incomplete=true"
        );
        assert!(
            stale_impact
                .repos_degraded
                .iter()
                .any(|d| d.repo == "checkout-svc"),
            "stale sibling repo must appear in repos_degraded: {:?}",
            stale_impact.repos_degraded
        );

        let _ = std::fs::remove_dir_all(&root);
    }
}
