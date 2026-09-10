//! Reviewable repository hygiene and tool-owned shared-cache maintenance.
mod background;
mod discovery;
pub mod global;
pub mod providers;
mod tree;

use crate::engine::git_cli::{
    build_capture_command, capture_command, git_with_stdin, run_observed, validate_repo,
    ProcessObserver,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheEntry {
    pub id: String,
    pub label: String,
    pub path: Option<String>,
    pub bytes: Option<u64>,
    pub action: Option<String>,
    pub note: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheInventory {
    pub entries: Vec<CacheEntry>,
    pub measured_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HygienePlan {
    pub id: String,
    pub repo_path: String,
    pub target: String,
    pub label: String,
    pub path: String,
    pub scope: String,
    pub command: String,
    pub bytes: u64,
    pub files: usize,
    pub expires_at: u64,
    pub warning: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HygieneOutcome {
    pub success: bool,
    pub message: String,
    pub bytes_before: u64,
    pub bytes_after: Option<u64>,
    pub elapsed_ms: u64,
}

#[derive(Clone)]
struct Invocation {
    argv: Vec<String>,
    environment: Vec<(String, String)>,
    cwd: PathBuf,
}

struct Prepared {
    view: HygienePlan,
    snapshot: tree::Snapshot,
    invocation: Option<Invocation>,
    created: Instant,
    min_age_days: u32,
    cancel: Arc<AtomicBool>,
}

#[derive(Default)]
struct Plans {
    pending: HashMap<String, Prepared>,
    active: HashMap<String, (String, Arc<AtomicBool>)>,
}

struct ActivePlan<'a>(&'a str);
impl Drop for ActivePlan<'_> {
    fn drop(&mut self) {
        match plans().lock() {
            Ok(mut store) => {
                store.active.remove(self.0);
            }
            Err(_) => log::error!("Hygiene active-plan store is unavailable"),
        }
    }
}

static PLANS: OnceLock<Mutex<Plans>> = OnceLock::new();
static EXECUTION: Mutex<()> = Mutex::new(());
static PREPARATION: Mutex<()> = Mutex::new(());
static INVENTORY: Mutex<()> = Mutex::new(());
static SEQUENCE: AtomicU64 = AtomicU64::new(0);
const TTL: Duration = Duration::from_secs(300);
const MAX_PLANS: usize = 4;

struct Cancellation<'a>(&'a AtomicBool);
impl ProcessObserver for Cancellation<'_> {
    fn cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

fn plans() -> &'static Mutex<Plans> {
    PLANS.get_or_init(|| Mutex::new(Plans::default()))
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn home_dir() -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .ok_or("Home directory unavailable")?;
    crate::engine::git_cli::canonicalize_plain(Path::new(&home)).map_err(|e| e.to_string())
}

fn tool(program: &str, args: &[&str], cwd: &Path) -> Result<String, String> {
    let result = capture_command(
        program,
        args,
        Some(cwd),
        Duration::from_secs(5),
        &[
            ("GOTOOLCHAIN", "local"),
            ("GOWORK", "off"),
            ("UV_PYTHON_DOWNLOADS", "never"),
            ("COREPACK_ENABLE_NETWORK", "0"),
        ],
    )?;
    if !result.success {
        return Err(format!(
            "{program} discovery failed (exit {})",
            result.status_code
        ));
    }
    if result.stdout.len() > 8192 {
        return Err("Tool discovery output exceeded its budget".into());
    }
    String::from_utf8(result.stdout)
        .map(|s| s.trim().to_owned())
        .map_err(|e| e.to_string())
}

fn cache_path(id: &str, home: &Path) -> Result<PathBuf, String> {
    let raw = match id {
        "cargo" => {
            return Ok(std::env::var_os("CARGO_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".cargo")))
        }
        "go" => tool("go", &["env", "GOCACHE"], home)?,
        "go-modules" => tool("go", &["env", "GOMODCACHE"], home)?,
        "npm" => tool("npm", &["config", "get", "cache"], home)?,
        "uv" => tool("uv", &["cache", "dir"], home)?,
        "pnpm" => tool("pnpm", &["store", "path"], home)?,
        _ => return Err("Unknown cache provider".into()),
    };
    if raw.chars().any(char::is_control) || !Path::new(&raw).is_absolute() {
        return Err("Provider did not return one absolute cache path".into());
    }
    Ok(PathBuf::from(raw))
}

fn managed_cache_path(path: &Path, home: &Path) -> Result<PathBuf, String> {
    let resolved = crate::engine::git_cli::canonicalize_plain(path).map_err(|e| e.to_string())?;
    let home = crate::engine::git_cli::canonicalize_plain(home).map_err(|e| e.to_string())?;
    // macOS aliases /var to /private/var. Resolve that platform prefix, but
    // reject a symlink at the cache itself and below the user's home.
    if std::fs::symlink_metadata(path)
        .map_err(|e| e.to_string())?
        .file_type()
        .is_symlink()
    {
        return Err("Symlinked cache requires manual maintenance".into());
    }
    let relative = resolved
        .strip_prefix(&home)
        .map_err(|_| "Cache is outside the home directory; use its owning tool manually")?;
    let text = relative.to_string_lossy().replace('\\', "/");
    let permitted = [
        "Library/Caches/",
        ".cache/",
        ".npm",
        "Library/pnpm/store/",
        ".local/share/pnpm/store/",
    ];
    if !permitted.iter().any(|prefix| {
        if *prefix == ".npm" {
            text == *prefix
        } else {
            text.starts_with(prefix) && text.len() > prefix.len()
        }
    }) {
        return Err("Custom cache location requires manual review; cleanup is restricted to managed cache roots".into());
    }
    tree::no_symlinks(&resolved)?;
    if resolved.ancestors().any(|p| p.join(".git").exists()) {
        return Err("Shared cache overlaps a Git repository; use repository cleanup".into());
    }
    Ok(resolved)
}

fn invocation(id: &str, path: &Path, home: &Path) -> Result<Invocation, String> {
    let path = path.to_str().ok_or("Cache path is not UTF-8")?;
    let (argv, mut environment): (Vec<&str>, Vec<(String, String)>) = match id {
        "go" => (
            vec!["go", "clean", "-cache"],
            vec![
                ("GOCACHE".into(), path.into()),
                ("GOTOOLCHAIN".into(), "local".into()),
                ("GOWORK".into(), "off".into()),
            ],
        ),
        "npm" => (vec!["npm", "--cache", path, "cache", "verify"], vec![]),
        "uv" => (
            vec!["uv", "--cache-dir", path, "cache", "prune"],
            vec![("UV_LOCK_TIMEOUT".into(), "10".into())],
        ),
        "pnpm" => (vec!["pnpm", "--store-dir", path, "store", "prune"], vec![]),
        _ => {
            return Err("This cache is maintained automatically or intentionally preserved".into())
        }
    };
    environment.push(("COREPACK_ENABLE_NETWORK".into(), "0".into()));
    Ok(Invocation {
        argv: argv.into_iter().map(str::to_owned).collect(),
        environment,
        cwd: home.to_owned(),
    })
}

pub fn cache_inventory() -> Result<CacheInventory, String> {
    let _lock = INVENTORY
        .try_lock()
        .map_err(|_| "A shared-cache scan is already running")?;
    let home = home_dir()?;
    let mut entries = Vec::new();
    let started = Instant::now();
    for (id, label, action, note) in [
        ("cargo", "Cargo registry and Git downloads", None, providers::CACHE_ADVICE[0].2),
        ("go", "Go build cache", Some("Clear build cache"), providers::CACHE_ADVICE[1].2),
        ("go-modules", "Go module downloads", None, "Preserved: deleting modules requires downloads and may remove installed toolchain downloads. Build-cache maintenance does not clear modules or fuzzing inputs."),
        ("npm", "npm package cache", Some("Verify and collect unused data"), "npm cache verify checks integrity and collects unneeded entries without clearing the entire cache."),
        ("uv", "uv cache", Some("Prune unused cache entries"), "uv owns cache locks and pruning. Pruning can also remove centralized project environments, which uv recreates. Local virtual environments are preserved; future installs may need downloads."),
        ("pnpm", "pnpm store", Some("Prune unreferenced packages"), "Remove packages the store considers unreferenced. Switching branches may require downloading them again."),
    ] {
        let mut item = CacheEntry { id: id.into(), label: label.into(), path: None, bytes: None, action: action.map(str::to_owned), note: note.into(), error: None };
        let result = (|| {
            if started.elapsed() > Duration::from_secs(45) { return Err("Shared-cache scan budget reached; this provider was not scanned".into()); }
            let path = cache_path(id, &home)?;
            item.path = Some(path.to_string_lossy().into_owned());
            // Inventory Cargo's cache payload only; never include binaries,
            // credentials, configuration or the global tracker in reclaim.
            if id == "cargo" {
                let mut total = 0u64;
                for part in ["registry", "git"] {
                    let part = path.join(part);
                    if part.exists() { total = total.saturating_add(tree::snapshot(&part, false, &AtomicBool::new(false))?.bytes); }
                }
                item.bytes = Some(total);
            } else {
                let measured = tree::snapshot(&path, false, &AtomicBool::new(false))?;
                item.bytes = Some(measured.bytes);
                if action.is_some() { managed_cache_path(&path, &home)?; }
            }
            Ok::<(), String>(())
        })();
        if let Err(error) = result { item.error = Some(error); item.action = None; }
        entries.push(item);
    }
    Ok(CacheInventory {
        entries,
        measured_at: now(),
    })
}

fn local_checks(repo: &Path, relative: &str) -> Result<PathBuf, String> {
    let path = repo.join(tree::relative_path(relative)?);
    tree::no_symlinks(&path)?;
    if !path.is_dir() || !path.starts_with(repo) || path == repo {
        return Err("Invalid artifact directory".into());
    }
    providers::local_provider(repo, relative)?;
    for ancestor in path.ancestors().take_while(|p| *p != repo) {
        if ancestor.join(".git").exists() {
            return Err("Nested repository or worktree is protected".into());
        }
    }
    let spec = format!(":(literal){relative}");
    let tracked = git_with_stdin(
        repo,
        &["-c", "core.fsmonitor=false", "ls-files", "-z", "--", &spec],
        &[],
    )?;
    if !tracked.is_empty() {
        return Err("Tracked files are protected; cleanup does not edit the index".into());
    }
    let ignored = git_with_stdin(
        repo,
        &[
            "-c",
            "core.fsmonitor=false",
            "check-ignore",
            "--no-index",
            "-z",
            "--stdin",
        ],
        format!("{relative}/\0").as_bytes(),
    )
    .map_err(|_| {
        "Directory is not ignored, or the ignore check failed. Add a reviewed ignore rule first."
    })?;
    if ignored.is_empty() {
        return Err("No ignore rule covers this directory".into());
    }
    Ok(path)
}

fn activity_check(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        let processes = capture_command(
            "ps",
            &["-A", "-o", "comm="],
            None,
            Duration::from_secs(5),
            &[],
        )?;
        if !processes.success {
            return Err("Could not verify active build processes".into());
        }
        for line in processes.stdout_text().lines() {
            let name = Path::new(line.trim())
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("");
            if matches!(
                name,
                "cargo"
                    | "rustc"
                    | "rustdoc"
                    | "go"
                    | "swift"
                    | "swift-frontend"
                    | "javac"
                    | "gradle"
                    | "dotnet"
                    | "cmake"
                    | "ninja"
                    | "make"
                    | "xcodebuild"
                    | "pytest"
                    | "uv"
                    | "pnpm"
                    | "npm"
            ) {
                return Err(format!("An active {name} process may be using build output. Stop builds before cleanup."));
            }
        }
        let path = path.to_str().ok_or("Non-UTF-8 path")?;
        let output = capture_command(
            "lsof",
            &["-nP", "-t", "+D", path],
            None,
            Duration::from_secs(10),
            &[],
        )?;
        if !output.stderr.is_empty() {
            return Err("Open-file check was incomplete; cleanup is unavailable".into());
        }
        if !output.stdout.is_empty() {
            return Err(
                "Files are open in this directory. Stop the processes using it before cleanup."
                    .into(),
            );
        }
        if output.status_code != 1 {
            return Err("Open-file check returned an unexpected status".into());
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Err("Active-file verification is unavailable on this platform".into())
    }
}

pub fn prepare(repo_path: &str, target: &str, min_age_days: u32) -> Result<HygienePlan, String> {
    prepare_with_activity(repo_path, target, min_age_days, activity_check)
}

fn prepare_with_activity(
    repo_path: &str,
    target: &str,
    min_age_days: u32,
    activity: impl Fn(&Path) -> Result<(), String>,
) -> Result<HygienePlan, String> {
    let _lock = PREPARATION
        .try_lock()
        .map_err(|_| "Another cleanup preview is being inspected")?;
    let repo = validate_repo(repo_path)?;
    devmap_query::hygiene::validate_retention(min_age_days, false)?;
    let cancel = Arc::new(AtomicBool::new(false));
    let (path, invocation, label, scope, warning) = if let Some(relative) =
        target.strip_prefix("local:")
    {
        let path = local_checks(&repo, relative)?;
        let label = providers::local_provider(&repo, relative)?.to_owned();
        let native = if path.file_name().is_some_and(|n| n == ".gocache") {
            Some(invocation("go", &path, &home_dir()?)?)
        } else {
            None
        };
        (path, native, label, "repository", "Removes the reviewed generated files permanently. Rebuilding costs time. Sizes are logical bytes, not guaranteed free disk space; hardlinks and filesystem clones can share storage.")
    } else if let Some(id) = target.strip_prefix("cache:") {
        let home = home_dir()?;
        let path = managed_cache_path(&cache_path(id, &home)?, &home)?;
        let invocation = invocation(id, &path, &home)?;
        (path, Some(invocation), format!("{id} cache maintenance"), "shared", "This cache is shared by every project. The tool decides which entries to remove. Current size is not a promised saving. Future builds or installs may be slower or need downloads.")
    } else {
        return Err("Unknown hygiene target".into());
    };
    let snapshot = tree::snapshot(&path, scope == "repository", &cancel)?;
    if scope == "repository" {
        tree::require_age(&snapshot, min_age_days, SystemTime::now())?;
    }
    activity(&path)?;
    let command = match &invocation {
        Some(invocation) => crate::harness::render_command(
            &invocation
                .argv
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
        ),
        None => format!(
            "Remove {} reviewed files from {}",
            snapshot.entries.values().filter(|e| !e.directory).count(),
            path.display()
        ),
    };
    let id = format!(
        "{}-{}-{}",
        std::process::id(),
        now(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    );
    let view = HygienePlan {
        id: id.clone(),
        repo_path: repo.to_string_lossy().into_owned(),
        target: target.into(),
        label,
        path: path.to_string_lossy().into_owned(),
        scope: scope.into(),
        command,
        bytes: snapshot.bytes,
        files: snapshot.entries.values().filter(|e| !e.directory).count(),
        expires_at: now() + TTL.as_secs(),
        warning: warning.into(),
    };
    let mut store = plans().lock().map_err(|_| "Plan store is unavailable")?;
    store.pending.retain(|_, p| p.created.elapsed() < TTL);
    if store.pending.len() >= MAX_PLANS {
        return Err("Too many pending previews; cancel one or wait for it to expire".into());
    }
    store.pending.insert(
        id,
        Prepared {
            view: view.clone(),
            snapshot,
            invocation,
            created: Instant::now(),
            min_age_days,
            cancel,
        },
    );
    Ok(view)
}

pub fn cancel(repo_path: &str, id: &str) -> Result<(), String> {
    let repo = validate_repo(repo_path)?.to_string_lossy().into_owned();
    let mut store = plans().lock().map_err(|_| "Plan store is unavailable")?;
    if let Some(plan) = store.pending.get(id) {
        if plan.view.repo_path != repo {
            return Err("Plan belongs to another repository".into());
        }
        store.pending.remove(id);
    } else if let Some((owner, flag)) = store.active.get(id) {
        if owner != &repo {
            return Err("Plan belongs to another repository".into());
        }
        flag.store(true, Ordering::Relaxed);
    }
    Ok(())
}

/// The command adapter supplies the existing gate. The callback receives the
/// exact argv or file boundary used below, never anything from the frontend.
pub fn execute<T>(
    repo_path: &str,
    id: &str,
    mut gate: impl FnMut(Option<&[String]>, &str) -> Result<T, String>,
) -> Result<(T, HygieneOutcome), String> {
    let _lock = global::mutation_lock()?;
    execute_with_activity(repo_path, id, &mut gate, activity_check)
}

fn execute_with_activity<T>(
    repo_path: &str,
    id: &str,
    mut gate: impl FnMut(Option<&[String]>, &str) -> Result<T, String>,
    activity: impl Fn(&Path) -> Result<(), String>,
) -> Result<(T, HygieneOutcome), String> {
    let _lock = EXECUTION
        .try_lock()
        .map_err(|_| "Another hygiene operation is running; wait for it to finish")?;
    let repo = validate_repo(repo_path)?;
    let plan = {
        let mut store = plans().lock().map_err(|_| "Plan store is unavailable")?;
        let plan = store
            .pending
            .get(id)
            .ok_or("Preview expired, was cancelled or already used; create a fresh preview")?;
        if plan.view.repo_path != repo.to_string_lossy() {
            return Err("Plan belongs to another repository".into());
        }
        let plan = store.pending.remove(id).ok_or("Preview unavailable")?;
        if plan.created.elapsed() >= TTL {
            return Err("Preview expired; review a fresh plan".into());
        }
        store.active.insert(
            id.into(),
            (plan.view.repo_path.clone(), plan.cancel.clone()),
        );
        plan
    };
    let _active = ActivePlan(id);
    let path = Path::new(&plan.view.path);
    if let Some(relative) = plan.view.target.strip_prefix("local:") {
        if local_checks(&repo, relative)? != path {
            return Err("Artifact path changed".into());
        }
    } else {
        let home = home_dir()?;
        let id = plan
            .view
            .target
            .strip_prefix("cache:")
            .ok_or("Invalid cache target")?;
        if managed_cache_path(&cache_path(id, &home)?, &home)? != path {
            return Err("Cache configuration changed; review again".into());
        }
    }
    let current = tree::snapshot(path, plan.view.scope == "repository", &plan.cancel)?;
    if current != plan.snapshot {
        return Err("Files changed since preview; create a fresh preview".into());
    }
    if plan.view.scope == "repository" {
        tree::require_age(&current, plan.min_age_days, SystemTime::now())?;
    }
    activity(path)?;
    let policy = gate(
        plan.invocation.as_ref().map(|i| i.argv.as_slice()),
        &plan.view.path,
    )?;
    // The gate can block on a policy provider: check the snapshot again after
    // it returns instead of treating the pre-gate scan as current permission.
    if tree::snapshot(path, plan.view.scope == "repository", &plan.cancel)? != current {
        return Err("Files changed while checking policy; review again".into());
    }
    // Ignore rules and the Git index live outside the artifact snapshot and
    // can change while a policy provider is answering.
    if let Some(relative) = plan.view.target.strip_prefix("local:") {
        if local_checks(&repo, relative)? != path {
            return Err("Artifact path changed while checking policy".into());
        }
    }
    activity(path)?;
    let started = Instant::now();
    let result = match &plan.invocation {
        None => tree::remove_reviewed(path, &current, &plan.cancel),
        Some(invocation) => {
            let args: Vec<&str> = invocation.argv[1..].iter().map(String::as_str).collect();
            let env: Vec<(&str, &str)> = invocation
                .environment
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect();
            let path_var = std::env::var_os("PATH");
            let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
            let mut command = build_capture_command(
                &invocation.argv[0],
                &args,
                Some(&invocation.cwd),
                &env,
                path_var.as_deref(),
                home.as_deref(),
            );
            match run_observed(&mut command, &invocation.argv[0], Duration::from_secs(120), None, 64 * 1024, &mut Cancellation(&plan.cancel)) {
                Ok(out) if out.cancelled => Err("Cancelled; the owning tool may already have removed some entries".into()),
                Ok(out) if out.success && out.incomplete.is_none() && out.stderr_incomplete.is_none() => Ok(()),
                Ok(out) if out.success => Err("Tool completed but its output was incomplete; verify the cache before retrying".into()),
                Ok(out) => Err(format!("Maintenance tool failed (exit {}); cache contents may have changed. Inspect the tool locally for details.", out.status_code)),
                Err(error) => Err(error),
            }
        }
    };
    let after = if !path.exists() {
        Some(0)
    } else {
        tree::snapshot(path, false, &AtomicBool::new(false))
            .ok()
            .map(|s| s.bytes)
    };
    let success = result.is_ok();
    let message = match result { Ok(()) => "Maintenance completed. Sizes show logical bytes before and after; other processes and shared file storage can affect disk-space savings.".into(), Err(error) => format!("Maintenance did not complete: {error}") };
    Ok((
        policy,
        HygieneOutcome {
            success,
            message,
            bytes_before: current.bytes,
            bytes_after: after,
            elapsed_ms: started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
        },
    ))
}

#[cfg(test)]
mod tests;
