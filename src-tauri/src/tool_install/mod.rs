//! In-app install / update of the `devmap` and `manvi` CLIs.
//!
//! Four-rung ladder (best available first):
//! 1. Already on PATH / saved config
//! 2. Prebuilt GitHub release (checksum-verified; mismatch refuses)
//! 3. `cargo install --git` / `go install …@latest` (toolchain, no checkout)
//! 4. Build from a local checkout (sibling or saved source root)
//!
//! Precedence for resolution: env → saved config → sibling (source) → PATH.
//! Progress streams via rate-limited `tool-install-progress` events.

pub mod release;

use crate::engine::git_cli::{self, BoundedRun};
use crate::tool_capability;
use crate::tool_config;
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Public clone / install URLs (constants — never taken from IPC).
pub const PUBLIC_DEVMAP_REPO: &str = "https://github.com/bharathvbcr/DevCouncil";
pub const PUBLIC_MANVI_REPO: &str = "https://github.com/bharathvbcr/Manvi";
pub const PUBLIC_DEVMAP_GIT: &str = "https://github.com/bharathvbcr/DevCouncil.git";
pub const PUBLIC_MANVI_GIT: &str = "https://github.com/bharathvbcr/Manvi.git";
pub const MANVI_GO_INSTALL: &str = "github.com/bharathvbcr/Manvi/manvi/cmd/manvi@latest";

/// Wall-clock budget for `cargo install` / `go install`.
pub const INSTALL_DEADLINE: Duration = Duration::from_secs(20 * 60);

/// Cap on captured install stdout/stderr combined tails.
pub const INSTALL_STDOUT_CAP: usize = 4 * 1024 * 1024;

const PROGRESS_MIN_INTERVAL: Duration = Duration::from_millis(150);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalTool {
    Devmap,
    Manvi,
}

impl ExternalTool {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Devmap => "devmap",
            Self::Manvi => "manvi",
        }
    }

    fn env_bin(self) -> &'static str {
        match self {
            Self::Devmap => "GITPULSE_DEVMAP_BIN",
            Self::Manvi => "GITPULSE_MANVI_BIN",
        }
    }

    fn env_root(self) -> &'static str {
        match self {
            Self::Devmap => "GITPULSE_DEVCOUNCIL_ROOT",
            Self::Manvi => "GITPULSE_MANVI_ROOT",
        }
    }

    fn sibling_names(self) -> &'static [&'static str] {
        match self {
            Self::Devmap => &["DevCouncil", "devcouncil"],
            Self::Manvi => &["Manvi", "manvi"],
        }
    }

    fn public_git(self) -> &'static str {
        match self {
            Self::Devmap => PUBLIC_DEVMAP_GIT,
            Self::Manvi => PUBLIC_MANVI_GIT,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolLookup {
    ExplicitEnv,
    /// Binary path from persisted `tools.json`.
    SavedConfig,
    PathSearch,
    Missing,
    /// `GITPULSE_*_BIN` is set but does not name a file — search stopped.
    ExplicitMissing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallRung {
    AlreadyOnPath,
    PrebuiltRelease,
    ToolchainRemote,
    LocalCheckout,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RungStatus {
    pub rung: InstallRung,
    pub available: bool,
    pub block: Option<String>,
    pub command: Option<String>,
    pub cost: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LadderAssessment {
    pub tool: ExternalTool,
    pub selected: Option<InstallRung>,
    pub rungs: Vec<RungStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolStatus {
    pub tool: ExternalTool,
    pub installed: bool,
    pub path: Option<String>,
    pub lookup: ToolLookup,
    pub version: Option<String>,
    pub reason: Option<String>,
    pub source_checkout: Option<String>,
    pub install_ready: bool,
    pub install_block: Option<String>,
    pub install_command: String,
    /// Best available install rung when not already installed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_rung: Option<InstallRung>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ladder: Vec<RungStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stale_config: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolsStatus {
    pub devmap: ToolStatus,
    pub manvi: ToolStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallOutcome {
    pub tool: ExternalTool,
    pub ok: bool,
    pub binary: Option<String>,
    pub lookup: Option<ToolLookup>,
    pub version: Option<String>,
    pub source_used: Option<String>,
    pub command: String,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
    pub cancelled: bool,
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rung: Option<InstallRung>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreflightRequirement {
    pub name: String,
    pub found: bool,
    pub path: Option<String>,
    pub version: Option<String>,
    pub satisfies: bool,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreflightReport {
    pub tool: ExternalTool,
    pub ok: bool,
    pub requirements: Vec<PreflightRequirement>,
    pub estimate: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifyReport {
    pub tool: ExternalTool,
    pub ok: bool,
    pub binary: Option<String>,
    pub detail: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store_schema: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_store_schema: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code_graph_schema: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_code_graph_schema: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manvi_protocol: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manvi_posture: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolInstallProgress {
    pub tool: String,
    pub line: String,
    pub rung: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloneSourceOutcome {
    pub tool: ExternalTool,
    pub ok: bool,
    pub path: Option<String>,
    pub reason: Option<String>,
}

fn cancel_flag() -> &'static AtomicBool {
    static FLAG: OnceLock<AtomicBool> = OnceLock::new();
    FLAG.get_or_init(|| AtomicBool::new(false))
}

fn install_guard() -> &'static Mutex<Option<ExternalTool>> {
    static GUARD: OnceLock<Mutex<Option<ExternalTool>>> = OnceLock::new();
    GUARD.get_or_init(|| Mutex::new(None))
}

static APP: OnceLock<tauri::AppHandle> = OnceLock::new();
static PROGRESS_LAST: Mutex<Option<Instant>> = Mutex::new(None);

pub fn set_app_handle(handle: tauri::AppHandle) {
    let _ = APP.set(handle);
}

fn emit_progress(tool: ExternalTool, line: &str, rung: Option<InstallRung>) {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return;
    }
    {
        let mut last = PROGRESS_LAST
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(prev) = *last {
            if prev.elapsed() < PROGRESS_MIN_INTERVAL {
                return;
            }
        }
        *last = Some(Instant::now());
    }
    use tauri::Emitter;
    if let Some(app) = APP.get() {
        let _ = app.emit(
            "tool-install-progress",
            ToolInstallProgress {
                tool: tool.as_str().into(),
                line: trimmed.chars().take(400).collect(),
                rung: rung.map(|r| format!("{r:?}").to_ascii_lowercase()),
            },
        );
    }
}

pub fn request_cancel() {
    cancel_flag().store(true, Ordering::SeqCst);
}

fn clear_cancel() {
    cancel_flag().store(false, Ordering::SeqCst);
}

fn cancelled() -> bool {
    cancel_flag().load(Ordering::SeqCst)
}

pub fn find_sibling(names: &[&str], from: &Path) -> Option<PathBuf> {
    let mut dir = from.to_path_buf();
    for _ in 0..8 {
        for name in names {
            let candidate = dir.join(name);
            if candidate.is_dir() {
                return Some(candidate);
            }
        }
        let parent = match dir.parent() {
            Some(p) if p != dir => p.to_path_buf(),
            _ => break,
        };
        dir = parent;
    }
    None
}

fn search_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            roots.push(parent.to_path_buf());
        }
    }
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    roots.push(manifest.clone());
    if let Some(parent) = manifest.parent() {
        roots.push(parent.to_path_buf());
    }
    roots
}

/// Source root: env → saved config → sibling walk.
pub fn resolve_source_root(tool: ExternalTool) -> Result<PathBuf, String> {
    if let Ok(explicit) = std::env::var(tool.env_root()) {
        let path = PathBuf::from(&explicit);
        if path.is_dir() {
            return Ok(path);
        }
        return Err(format!(
            "{} is set to {explicit}, but that path is not a directory",
            tool.env_root()
        ));
    }
    if let Some(saved) = tool_config::saved_source_root(tool) {
        return Ok(saved);
    }
    for root in search_roots() {
        if let Some(found) = find_sibling(tool.sibling_names(), &root) {
            return Ok(found);
        }
    }
    Err(format!(
        "no {} checkout found beside GitPulse (set {} or run setup)",
        tool.sibling_names()[0],
        tool.env_root()
    ))
}

pub fn install_target(tool: ExternalTool, root: &Path) -> Result<PathBuf, String> {
    match tool {
        ExternalTool::Devmap => {
            let crate_path = root.join("rust-port/crates/devmap-cli");
            if crate_path.join("Cargo.toml").is_file() {
                return Ok(crate_path);
            }
            let alt = root.join("crates/devmap-cli");
            if alt.join("Cargo.toml").is_file() {
                return Ok(alt);
            }
            Err(format!(
                "{} has no rust-port/crates/devmap-cli (looked under {})",
                tool.env_root(),
                root.display()
            ))
        }
        ExternalTool::Manvi => {
            let module = root.join("manvi");
            if module.join("go.mod").is_file() && module.join("cmd/manvi").is_dir() {
                return Ok(module);
            }
            if root.join("go.mod").is_file() && root.join("cmd/manvi").is_dir() {
                return Ok(root.to_path_buf());
            }
            Err(format!(
                "{} has no manvi/go.mod + manvi/cmd/manvi (looked under {})",
                tool.env_root(),
                root.display()
            ))
        }
    }
}

fn documented_command(tool: ExternalTool, target: Option<&Path>) -> String {
    match (tool, target) {
        (ExternalTool::Devmap, Some(path)) => {
            format!("cargo install --path {} --locked --force", path.display())
        }
        (ExternalTool::Devmap, None) => {
            "cargo install --path <DevCouncil>/rust-port/crates/devmap-cli --locked --force".into()
        }
        (ExternalTool::Manvi, Some(path)) => {
            format!("go -C {} install ./cmd/manvi", path.display())
        }
        (ExternalTool::Manvi, None) => "go -C <Manvi>/manvi install ./cmd/manvi".into(),
    }
}

fn remote_install_command(tool: ExternalTool) -> String {
    match tool {
        ExternalTool::Devmap => {
            format!("cargo install --git {PUBLIC_DEVMAP_GIT} --locked --force devmap-cli")
        }
        ExternalTool::Manvi => format!("go install {MANVI_GO_INSTALL}"),
    }
}

fn probe_version(path: &str, tool: ExternalTool) -> Option<String> {
    let mut cmd = Command::new(path);
    match tool {
        ExternalTool::Devmap => {
            cmd.arg("--version");
        }
        ExternalTool::Manvi => {
            cmd.args(["--help"]);
        }
    }
    let run =
        git_cli::run_bounded_capped(cmd, tool.as_str(), Duration::from_secs(5), None, 64 * 1024)
            .ok()?;
    let text = String::from_utf8_lossy(&run.stdout);
    let err = String::from_utf8_lossy(&run.stderr);
    let combined = if text.trim().is_empty() {
        err.into_owned()
    } else {
        text.into_owned()
    };
    let line = combined.lines().find(|l| {
        let lower = l.to_ascii_lowercase();
        lower.contains("version") || lower.contains("devmap") || lower.contains("manvi")
    })?;
    Some(line.trim().chars().take(120).collect())
}

/// Assess every rung; pick the first available actionable install rung.
pub fn assess_ladder(tool: ExternalTool) -> LadderAssessment {
    let on_path = match tool {
        ExternalTool::Devmap => crate::devmap::cli::resolve_binary().ok().map(|r| r.path),
        ExternalTool::Manvi => crate::harness::sidecar::resolve_binary(),
    };

    let mut rungs = Vec::new();

    // Rung 1 — already present
    rungs.push(if on_path.is_some() {
        RungStatus {
            rung: InstallRung::AlreadyOnPath,
            available: true,
            block: None,
            command: None,
            cost: "nothing".into(),
        }
    } else {
        RungStatus {
            rung: InstallRung::AlreadyOnPath,
            available: false,
            block: Some(format!(
                "{} is not on PATH or in saved config",
                tool.as_str()
            )),
            command: None,
            cost: "nothing".into(),
        }
    });

    // Rung 2 — prebuilt
    let release = release::probe_release(tool);
    rungs.push(match release {
        release::ReleaseAvailability::Available { ref asset_name, .. } => RungStatus {
            rung: InstallRung::PrebuiltRelease,
            available: true,
            block: None,
            command: Some(format!("download {asset_name} + verify checksum")),
            cost: "seconds".into(),
        },
        release::ReleaseAvailability::Unavailable { reason } => RungStatus {
            rung: InstallRung::PrebuiltRelease,
            available: false,
            block: Some(reason),
            command: None,
            cost: "seconds".into(),
        },
    });

    // Rung 3 — toolchain remote
    let (toolchain_ok, toolchain_block) = match tool {
        ExternalTool::Devmap => match git_cli::find_external_tool("cargo") {
            Some(_) => (true, None),
            None => (
                false,
                Some("cargo is not installed (needed for cargo install --git)".into()),
            ),
        },
        ExternalTool::Manvi => match git_cli::find_external_tool("go") {
            Some(_) => (true, None),
            None => (
                false,
                Some("go is not installed (needed for go install @latest)".into()),
            ),
        },
    };
    rungs.push(RungStatus {
        rung: InstallRung::ToolchainRemote,
        available: toolchain_ok,
        block: toolchain_block,
        command: Some(remote_install_command(tool)),
        cost: match tool {
            // Phase 0 measured: cargo install --git succeeded in 83s; cold
            // release build 77s. Keep the UI honest rather than "several minutes".
            ExternalTool::Devmap => "~1–2 min (measured cargo --git ~83s)".into(),
            ExternalTool::Manvi => "under a minute typically".into(),
        },
    });

    // Rung 4 — local checkout
    let source = resolve_source_root(tool).ok();
    let target = source
        .as_ref()
        .and_then(|root| install_target(tool, root).ok());
    let installer_ok = match tool {
        ExternalTool::Devmap => git_cli::find_external_tool("cargo").is_some(),
        ExternalTool::Manvi => git_cli::find_external_tool("go").is_some(),
    };
    let (local_ok, local_block) = match (&target, installer_ok) {
        (Some(_), true) => (true, None),
        (None, _) => (
            false,
            Some(
                source
                    .as_ref()
                    .map(|s| {
                        install_target(tool, s)
                            .err()
                            .unwrap_or_else(|| format!("source at {} is incomplete", s.display()))
                    })
                    .unwrap_or_else(|| {
                        format!(
                            "no local checkout (clone {}, or set {})",
                            tool.public_git(),
                            tool.env_root()
                        )
                    }),
            ),
        ),
        (Some(_), false) => (
            false,
            Some(match tool {
                ExternalTool::Devmap => "cargo is not installed".into(),
                ExternalTool::Manvi => "go is not installed".into(),
            }),
        ),
    };
    rungs.push(RungStatus {
        rung: InstallRung::LocalCheckout,
        available: local_ok,
        block: local_block,
        command: Some(documented_command(tool, target.as_deref())),
        cost: match tool {
            ExternalTool::Devmap => "~1–2 min (cold release measured ~77s)".into(),
            ExternalTool::Manvi => "under a minute typically".into(),
        },
    });

    let selected = if on_path.is_some() {
        Some(InstallRung::AlreadyOnPath)
    } else {
        rungs
            .iter()
            .find(|r| r.available && !matches!(r.rung, InstallRung::AlreadyOnPath))
            .map(|r| r.rung)
    };

    LadderAssessment {
        tool,
        selected,
        rungs,
    }
}

pub fn resolve_status(tool: ExternalTool) -> ToolStatus {
    let ladder = assess_ladder(tool);
    let source = resolve_source_root(tool).ok();
    let target = source
        .as_ref()
        .and_then(|root| install_target(tool, root).ok());
    let install_command = ladder
        .selected
        .and_then(|rung| {
            ladder
                .rungs
                .iter()
                .find(|r| r.rung == rung)
                .and_then(|r| r.command.clone())
        })
        .unwrap_or_else(|| documented_command(tool, target.as_deref()));

    let install_ready = ladder
        .rungs
        .iter()
        .any(|r| r.available && !matches!(r.rung, InstallRung::AlreadyOnPath));
    let install_block = if install_ready {
        None
    } else {
        Some(
            ladder
                .rungs
                .iter()
                .filter(|r| !r.available)
                .filter_map(|r| r.block.clone())
                .collect::<Vec<_>>()
                .join("; "),
        )
    };

    let stale_config = tool_config::view().ok().and_then(|v| {
        v.stale
            .into_iter()
            .find(|s| s.field.starts_with(tool.as_str()))
            .map(|s| s.detail)
    });

    // Explicit env first — refuse rather than search past.
    if let Ok(explicit) = std::env::var(tool.env_bin()) {
        let path = PathBuf::from(&explicit);
        if path.is_file() {
            let path_s = explicit;
            let version = probe_version(&path_s, tool);
            return ToolStatus {
                tool,
                installed: true,
                path: Some(path_s),
                lookup: ToolLookup::ExplicitEnv,
                version,
                reason: None,
                source_checkout: source.map(|p| p.display().to_string()),
                install_ready,
                install_block,
                install_command,
                selected_rung: Some(InstallRung::AlreadyOnPath),
                ladder: ladder.rungs,
                stale_config,
            };
        }
        return ToolStatus {
            tool,
            installed: false,
            path: None,
            lookup: ToolLookup::ExplicitMissing,
            version: None,
            reason: Some(format!(
                "{} is set to {explicit}, but that path is not a file",
                tool.env_bin()
            )),
            source_checkout: source.map(|p| p.display().to_string()),
            install_ready,
            install_block,
            install_command,
            selected_rung: ladder.selected,
            ladder: ladder.rungs,
            stale_config,
        };
    }

    // Saved config binary (only when the file still exists).
    if let Some(path) = tool_config::saved_binary(tool) {
        let version = probe_version(&path, tool);
        return ToolStatus {
            tool,
            installed: true,
            path: Some(path),
            lookup: ToolLookup::SavedConfig,
            version,
            reason: None,
            source_checkout: source.map(|p| p.display().to_string()),
            install_ready,
            install_block,
            install_command,
            selected_rung: Some(InstallRung::AlreadyOnPath),
            ladder: ladder.rungs,
            stale_config,
        };
    }

    let resolved = match tool {
        ExternalTool::Devmap => crate::devmap::cli::resolve_binary_uncached()
            .ok()
            .map(|r| (r.path, ToolLookup::PathSearch)),
        ExternalTool::Manvi => {
            crate::harness::sidecar::resolve_binary_uncached().map(|p| (p, ToolLookup::PathSearch))
        }
    };

    if let Some((path, lookup)) = resolved {
        let version = probe_version(&path, tool);
        return ToolStatus {
            tool,
            installed: true,
            path: Some(path),
            lookup,
            version,
            reason: None,
            source_checkout: source.map(|p| p.display().to_string()),
            install_ready,
            install_block,
            install_command,
            selected_rung: Some(InstallRung::AlreadyOnPath),
            ladder: ladder.rungs,
            stale_config,
        };
    }

    ToolStatus {
        tool,
        installed: false,
        path: None,
        lookup: ToolLookup::Missing,
        version: None,
        reason: Some(format!(
            "{} is not installed (searched env, saved config, PATH; set {} or run setup)",
            tool.as_str(),
            tool.env_bin()
        )),
        source_checkout: source.map(|p| p.display().to_string()),
        install_ready,
        install_block,
        install_command,
        selected_rung: ladder.selected,
        ladder: ladder.rungs,
        stale_config,
    }
}

pub fn status_all() -> ToolsStatus {
    ToolsStatus {
        devmap: resolve_status(ExternalTool::Devmap),
        manvi: resolve_status(ExternalTool::Manvi),
    }
}

fn bytes_to_string(bytes: Vec<u8>) -> String {
    String::from_utf8_lossy(&bytes).into_owned()
}

fn timed_out(err: &str) -> bool {
    err.contains("timed out") || err.contains("timeout")
}

enum InstallRun {
    Finished(BoundedRun),
    Cancelled { stdout: String, stderr: String },
    Failed(String),
}

fn drain_capped_progress(
    pipe: Option<impl Read>,
    cap: usize,
    tool: ExternalTool,
    rung: Option<InstallRung>,
) -> Vec<u8> {
    let mut out = Vec::new();
    let Some(mut pipe) = pipe else {
        return out;
    };
    let mut buf = [0u8; 8192];
    let mut line_buf = String::new();
    loop {
        match pipe.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                let chunk = &buf[..n];
                let room = cap.saturating_sub(out.len());
                if room > 0 {
                    out.extend_from_slice(&chunk[..n.min(room)]);
                }
                if let Ok(text) = std::str::from_utf8(chunk) {
                    for ch in text.chars() {
                        if ch == '\n' {
                            emit_progress(tool, &line_buf, rung);
                            line_buf.clear();
                        } else if ch != '\r' && line_buf.len() < 500 {
                            line_buf.push(ch);
                        }
                    }
                }
            }
            Err(_) => break,
        }
    }
    if !line_buf.is_empty() {
        emit_progress(tool, &line_buf, rung);
    }
    out
}

fn run_cancellable_install(
    cmd: &mut Command,
    label: &str,
    tool: ExternalTool,
    rung: Option<InstallRung>,
) -> InstallRun {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let (mut child, guard) = match crate::procguard::spawn(cmd, label) {
        Ok(pair) => pair,
        Err(e) => return InstallRun::Failed(format!("Failed to spawn {label}: {e}")),
    };

    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();
    let (stdout_tx, stdout_rx) = std::sync::mpsc::channel();
    let (stderr_tx, stderr_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = stdout_tx.send(drain_capped_progress(
            stdout_pipe,
            INSTALL_STDOUT_CAP,
            tool,
            rung,
        ));
    });
    std::thread::spawn(move || {
        let _ = stderr_tx.send(drain_capped_progress(
            stderr_pipe,
            INSTALL_STDOUT_CAP,
            tool,
            rung,
        ));
    });

    let start = Instant::now();
    let outcome = loop {
        match guard.poll(|| child.try_wait()) {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => {
                if cancelled() {
                    guard.kill_tree(&mut child);
                    let _ = guard.reap(|| child.wait());
                    break Err("cancelled");
                }
                if start.elapsed() > INSTALL_DEADLINE {
                    guard.kill_tree(&mut child);
                    let _ = guard.reap(|| child.wait());
                    break Err("timed out");
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(_e) => {
                guard.kill_tree(&mut child);
                let _ = guard.reap(|| child.wait());
                break Err("wait failed");
            }
        }
    };

    let stdout = stdout_rx.recv().unwrap_or_default();
    let stderr = stderr_rx.recv().unwrap_or_default();
    let stdout_s = bytes_to_string(stdout);
    let stderr_s = bytes_to_string(stderr);

    match outcome {
        Ok(status) => InstallRun::Finished(BoundedRun {
            success: status.success(),
            status_code: status.code().unwrap_or(-1),
            stdout: stdout_s.into_bytes(),
            stderr: stderr_s.into_bytes(),
            incomplete: None,
        }),
        Err("cancelled") => InstallRun::Cancelled {
            stdout: stdout_s,
            stderr: stderr_s,
        },
        Err("timed out") => InstallRun::Failed(format!(
            "{label} timed out after {}s",
            INSTALL_DEADLINE.as_secs()
        )),
        Err(_) => InstallRun::Failed(format!("{label} wait failed")),
    }
}

pub fn install(tool: ExternalTool) -> InstallOutcome {
    install_with_rung(tool, None)
}

pub fn install_with_rung(tool: ExternalTool, preferred: Option<InstallRung>) -> InstallOutcome {
    clear_cancel();
    tool_capability::invalidate(tool);

    {
        let mut guard = install_guard()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if guard.is_some() {
            return InstallOutcome {
                tool,
                ok: false,
                binary: None,
                lookup: None,
                version: None,
                source_used: None,
                command: documented_command(tool, None),
                exit_code: None,
                stdout: String::new(),
                stderr: String::new(),
                timed_out: false,
                cancelled: false,
                reason: Some("another tool install is already running".into()),
                rung: preferred,
            };
        }
        *guard = Some(tool);
    }

    let outcome = install_inner(tool, preferred);

    *install_guard()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    clear_cancel();
    tool_capability::invalidate(tool);
    outcome
}

fn install_inner(tool: ExternalTool, preferred: Option<InstallRung>) -> InstallOutcome {
    let assessment = assess_ladder(tool);
    if assessment.selected == Some(InstallRung::AlreadyOnPath) && preferred.is_none() {
        let status = resolve_status(tool);
        return InstallOutcome {
            tool,
            ok: status.installed,
            binary: status.path,
            lookup: Some(status.lookup),
            version: status.version,
            source_used: status.source_checkout,
            command: "already installed".into(),
            exit_code: Some(0),
            stdout: String::new(),
            stderr: String::new(),
            timed_out: false,
            cancelled: false,
            reason: None,
            rung: Some(InstallRung::AlreadyOnPath),
        };
    }

    let rung = preferred.or(assessment
        .selected
        .filter(|r| !matches!(r, InstallRung::AlreadyOnPath)));

    let Some(rung) = rung else {
        let blocks: Vec<_> = assessment
            .rungs
            .iter()
            .filter_map(|r| r.block.clone())
            .collect();
        return InstallOutcome {
            tool,
            ok: false,
            binary: None,
            lookup: None,
            version: None,
            source_used: None,
            command: String::new(),
            exit_code: None,
            stdout: String::new(),
            stderr: String::new(),
            timed_out: false,
            cancelled: cancelled(),
            reason: Some(format!("no install rung available: {}", blocks.join("; "))),
            rung: None,
        };
    };

    // Confirm the chosen rung is still available.
    let rung_status = assessment.rungs.iter().find(|r| r.rung == rung);
    if let Some(rs) = rung_status {
        if !rs.available {
            return InstallOutcome {
                tool,
                ok: false,
                binary: None,
                lookup: None,
                version: None,
                source_used: None,
                command: rs.command.clone().unwrap_or_default(),
                exit_code: None,
                stdout: String::new(),
                stderr: String::new(),
                timed_out: false,
                cancelled: false,
                reason: rs.block.clone(),
                rung: Some(rung),
            };
        }
    }

    match rung {
        InstallRung::AlreadyOnPath => {
            let status = resolve_status(tool);
            InstallOutcome {
                tool,
                ok: status.installed,
                binary: status.path,
                lookup: Some(status.lookup),
                version: status.version,
                source_used: status.source_checkout,
                command: "already installed".into(),
                exit_code: Some(0),
                stdout: String::new(),
                stderr: String::new(),
                timed_out: false,
                cancelled: false,
                reason: None,
                rung: Some(rung),
            }
        }
        InstallRung::PrebuiltRelease => install_prebuilt_rung(tool),
        InstallRung::ToolchainRemote => install_toolchain_remote(tool),
        InstallRung::LocalCheckout => install_from_checkout(tool),
    }
}

fn finish_ok(
    tool: ExternalTool,
    rung: InstallRung,
    command: String,
    source_used: Option<String>,
    stdout: String,
    stderr: String,
    exit_code: Option<i32>,
) -> InstallOutcome {
    tool_capability::invalidate(tool);
    let status = resolve_status(tool);
    if let Some(ref path) = status.path {
        let _ = tool_config::set_binary(tool, path);
    }
    InstallOutcome {
        tool,
        ok: status.installed,
        binary: status.path.clone(),
        lookup: Some(status.lookup.clone()),
        version: status.version.clone(),
        source_used,
        command,
        exit_code,
        stdout,
        stderr,
        timed_out: false,
        cancelled: false,
        reason: if status.installed {
            None
        } else {
            Some(format!(
                "install finished but {} is still not resolvable — check PATH / saved config",
                tool.as_str()
            ))
        },
        rung: Some(rung),
    }
}

fn install_prebuilt_rung(tool: ExternalTool) -> InstallOutcome {
    emit_progress(
        tool,
        "downloading prebuilt release…",
        Some(InstallRung::PrebuiltRelease),
    );
    match release::install_prebuilt(tool) {
        Ok(path) => {
            let _ = tool_config::set_binary(tool, &path.display().to_string());
            tool_capability::invalidate(tool);
            finish_ok(
                tool,
                InstallRung::PrebuiltRelease,
                format!("prebuilt → {}", path.display()),
                None,
                String::new(),
                String::new(),
                Some(0),
            )
        }
        Err(reason) => InstallOutcome {
            tool,
            ok: false,
            binary: None,
            lookup: None,
            version: None,
            source_used: None,
            command: "download + verify checksum".into(),
            exit_code: None,
            stdout: String::new(),
            stderr: reason.clone(),
            timed_out: false,
            cancelled: cancelled(),
            reason: Some(reason),
            rung: Some(InstallRung::PrebuiltRelease),
        },
    }
}

fn install_toolchain_remote(tool: ExternalTool) -> InstallOutcome {
    let command = remote_install_command(tool);
    if cancelled() {
        return InstallOutcome {
            tool,
            ok: false,
            binary: None,
            lookup: None,
            version: None,
            source_used: None,
            command,
            exit_code: None,
            stdout: String::new(),
            stderr: String::new(),
            timed_out: false,
            cancelled: true,
            reason: Some("cancelled before start".into()),
            rung: Some(InstallRung::ToolchainRemote),
        };
    }
    let mut cmd = match tool {
        ExternalTool::Devmap => {
            let cargo = match git_cli::find_external_tool("cargo") {
                Some(c) => c,
                None => {
                    return InstallOutcome {
                        tool,
                        ok: false,
                        binary: None,
                        lookup: None,
                        version: None,
                        source_used: None,
                        command,
                        exit_code: None,
                        stdout: String::new(),
                        stderr: String::new(),
                        timed_out: false,
                        cancelled: false,
                        reason: Some("cargo is not installed".into()),
                        rung: Some(InstallRung::ToolchainRemote),
                    };
                }
            };
            let mut cmd = Command::new(&cargo);
            cmd.args([
                "install",
                "--git",
                PUBLIC_DEVMAP_GIT,
                "--locked",
                "--force",
                "devmap-cli",
            ]);
            cmd
        }
        ExternalTool::Manvi => {
            let go = match git_cli::find_external_tool("go") {
                Some(g) => g,
                None => {
                    return InstallOutcome {
                        tool,
                        ok: false,
                        binary: None,
                        lookup: None,
                        version: None,
                        source_used: None,
                        command,
                        exit_code: None,
                        stdout: String::new(),
                        stderr: String::new(),
                        timed_out: false,
                        cancelled: false,
                        reason: Some("go is not installed".into()),
                        rung: Some(InstallRung::ToolchainRemote),
                    };
                }
            };
            let mut cmd = Command::new(&go);
            cmd.args(["install", MANVI_GO_INSTALL]);
            cmd
        }
    };
    let label = match tool {
        ExternalTool::Devmap => "cargo install --git",
        ExternalTool::Manvi => "go install @latest",
    };
    match run_cancellable_install(&mut cmd, label, tool, Some(InstallRung::ToolchainRemote)) {
        InstallRun::Cancelled { stdout, stderr } => InstallOutcome {
            tool,
            ok: false,
            binary: None,
            lookup: None,
            version: None,
            source_used: None,
            command,
            exit_code: None,
            stdout,
            stderr,
            timed_out: false,
            cancelled: true,
            reason: Some("cancelled".into()),
            rung: Some(InstallRung::ToolchainRemote),
        },
        InstallRun::Failed(err) => InstallOutcome {
            tool,
            ok: false,
            binary: None,
            lookup: None,
            version: None,
            source_used: None,
            command,
            exit_code: None,
            stdout: String::new(),
            stderr: err.clone(),
            timed_out: timed_out(&err),
            cancelled: cancelled(),
            reason: Some(err),
            rung: Some(InstallRung::ToolchainRemote),
        },
        InstallRun::Finished(run) => {
            let stdout = bytes_to_string(run.stdout);
            let stderr = bytes_to_string(run.stderr);
            if !run.success {
                return InstallOutcome {
                    tool,
                    ok: false,
                    binary: None,
                    lookup: None,
                    version: None,
                    source_used: None,
                    command,
                    exit_code: Some(run.status_code),
                    stdout,
                    stderr,
                    timed_out: false,
                    cancelled: false,
                    reason: Some(format!(
                        "{} remote install exited {}",
                        tool.as_str(),
                        run.status_code
                    )),
                    rung: Some(InstallRung::ToolchainRemote),
                };
            }
            finish_ok(
                tool,
                InstallRung::ToolchainRemote,
                command,
                None,
                stdout,
                stderr,
                Some(run.status_code),
            )
        }
    }
}

fn install_from_checkout(tool: ExternalTool) -> InstallOutcome {
    let root = match resolve_source_root(tool) {
        Ok(r) => r,
        Err(reason) => {
            return InstallOutcome {
                tool,
                ok: false,
                binary: None,
                lookup: None,
                version: None,
                source_used: None,
                command: documented_command(tool, None),
                exit_code: None,
                stdout: String::new(),
                stderr: String::new(),
                timed_out: false,
                cancelled: cancelled(),
                reason: Some(reason),
                rung: Some(InstallRung::LocalCheckout),
            };
        }
    };
    let target = match install_target(tool, &root) {
        Ok(t) => t,
        Err(reason) => {
            return InstallOutcome {
                tool,
                ok: false,
                binary: None,
                lookup: None,
                version: None,
                source_used: Some(root.display().to_string()),
                command: documented_command(tool, None),
                exit_code: None,
                stdout: String::new(),
                stderr: String::new(),
                timed_out: false,
                cancelled: cancelled(),
                reason: Some(reason),
                rung: Some(InstallRung::LocalCheckout),
            };
        }
    };
    let command = documented_command(tool, Some(&target));
    if cancelled() {
        return InstallOutcome {
            tool,
            ok: false,
            binary: None,
            lookup: None,
            version: None,
            source_used: Some(root.display().to_string()),
            command,
            exit_code: None,
            stdout: String::new(),
            stderr: String::new(),
            timed_out: false,
            cancelled: true,
            reason: Some("cancelled before start".into()),
            rung: Some(InstallRung::LocalCheckout),
        };
    }

    let mut cmd = match tool {
        ExternalTool::Devmap => {
            let cargo = match git_cli::find_external_tool("cargo") {
                Some(c) => c,
                None => {
                    return InstallOutcome {
                        tool,
                        ok: false,
                        binary: None,
                        lookup: None,
                        version: None,
                        source_used: Some(root.display().to_string()),
                        command,
                        exit_code: None,
                        stdout: String::new(),
                        stderr: String::new(),
                        timed_out: false,
                        cancelled: false,
                        reason: Some("cargo is not installed".into()),
                        rung: Some(InstallRung::LocalCheckout),
                    };
                }
            };
            let mut cmd = Command::new(&cargo);
            cmd.args([
                "install",
                "--path",
                &target.display().to_string(),
                "--locked",
                "--force",
            ]);
            cmd.current_dir(&target);
            cmd
        }
        ExternalTool::Manvi => {
            let go = match git_cli::find_external_tool("go") {
                Some(g) => g,
                None => {
                    return InstallOutcome {
                        tool,
                        ok: false,
                        binary: None,
                        lookup: None,
                        version: None,
                        source_used: Some(root.display().to_string()),
                        command,
                        exit_code: None,
                        stdout: String::new(),
                        stderr: String::new(),
                        timed_out: false,
                        cancelled: false,
                        reason: Some("go is not installed".into()),
                        rung: Some(InstallRung::LocalCheckout),
                    };
                }
            };
            let mut cmd = Command::new(&go);
            cmd.args([
                "-C",
                &target.display().to_string(),
                "install",
                "./cmd/manvi",
            ]);
            cmd
        }
    };

    let label = match tool {
        ExternalTool::Devmap => "cargo install",
        ExternalTool::Manvi => "go install",
    };
    match run_cancellable_install(&mut cmd, label, tool, Some(InstallRung::LocalCheckout)) {
        InstallRun::Cancelled { stdout, stderr } => InstallOutcome {
            tool,
            ok: false,
            binary: None,
            lookup: None,
            version: None,
            source_used: Some(root.display().to_string()),
            command,
            exit_code: None,
            stdout,
            stderr,
            timed_out: false,
            cancelled: true,
            reason: Some("cancelled".into()),
            rung: Some(InstallRung::LocalCheckout),
        },
        InstallRun::Failed(err) => {
            let was_timeout = timed_out(&err);
            InstallOutcome {
                tool,
                ok: false,
                binary: None,
                lookup: None,
                version: None,
                source_used: Some(root.display().to_string()),
                command,
                exit_code: None,
                stdout: String::new(),
                stderr: err.clone(),
                timed_out: was_timeout,
                cancelled: cancelled(),
                reason: Some(err),
                rung: Some(InstallRung::LocalCheckout),
            }
        }
        InstallRun::Finished(run) => {
            let stdout = bytes_to_string(run.stdout);
            let stderr = bytes_to_string(run.stderr);
            if !run.success {
                return InstallOutcome {
                    tool,
                    ok: false,
                    binary: None,
                    lookup: None,
                    version: None,
                    source_used: Some(root.display().to_string()),
                    command,
                    exit_code: Some(run.status_code),
                    stdout,
                    stderr,
                    timed_out: false,
                    cancelled: false,
                    reason: Some(format!(
                        "{} install exited {}",
                        tool.as_str(),
                        run.status_code
                    )),
                    rung: Some(InstallRung::LocalCheckout),
                };
            }
            finish_ok(
                tool,
                InstallRung::LocalCheckout,
                command,
                Some(root.display().to_string()),
                stdout,
                stderr,
                Some(run.status_code),
            )
        }
    }
}

pub fn preflight(tool: ExternalTool) -> PreflightReport {
    let mut requirements = Vec::new();
    match tool {
        ExternalTool::Devmap => {
            let cargo = git_cli::find_external_tool("cargo");
            let ver = cargo.as_ref().and_then(|p| {
                let mut cmd = Command::new(p);
                cmd.arg("--version");
                git_cli::run_bounded_capped(cmd, "cargo", Duration::from_secs(5), None, 8 * 1024)
                    .ok()
                    .map(|r| String::from_utf8_lossy(&r.stdout).trim().to_string())
            });
            requirements.push(PreflightRequirement {
                name: "cargo".into(),
                found: cargo.is_some(),
                path: cargo.clone(),
                version: ver,
                satisfies: cargo.is_some(),
                note: cargo
                    .is_none()
                    .then(|| "needed for cargo install / --git".into()),
            });
            let cc = git_cli::find_external_tool("cc")
                .or_else(|| git_cli::find_external_tool("clang"))
                .or_else(|| git_cli::find_external_tool("gcc"));
            requirements.push(PreflightRequirement {
                name: "c_compiler".into(),
                found: cc.is_some(),
                path: cc.clone(),
                version: None,
                satisfies: cc.is_some(),
                note: cc.is_none().then(|| {
                    "devmap-extract compiles tree-sitter grammars; missing cc fails mid-build"
                        .into()
                }),
            });
        }
        ExternalTool::Manvi => {
            let go = git_cli::find_external_tool("go");
            let ver = go.as_ref().and_then(|p| {
                let mut cmd = Command::new(p);
                cmd.arg("version");
                git_cli::run_bounded_capped(cmd, "go", Duration::from_secs(5), None, 8 * 1024)
                    .ok()
                    .map(|r| String::from_utf8_lossy(&r.stdout).trim().to_string())
            });
            let satisfies = ver
                .as_deref()
                .map(|v| {
                    // go 1.26+ preferred (module directive); accept 1.22+ as soft.
                    v.contains("go1.2") || v.contains("go1.3") || v.contains("devel")
                })
                .unwrap_or(false);
            requirements.push(PreflightRequirement {
                name: "go".into(),
                found: go.is_some(),
                path: go,
                version: ver,
                satisfies,
                note: (!satisfies)
                    .then(|| "Manvi go.mod directs go 1.26.6; install a matching toolchain".into()),
            });
        }
    }
    let curl = git_cli::find_external_tool("curl");
    requirements.push(PreflightRequirement {
        name: "curl".into(),
        found: curl.is_some(),
        path: curl.clone(),
        version: None,
        satisfies: curl.is_some(),
        note: curl
            .is_none()
            .then(|| "needed only for the prebuilt-release rung".into()),
    });

    let estimate = match tool {
        ExternalTool::Devmap => {
            // Phase 0 measured on this machine: cargo install --git 83s;
            // cold release (fresh CARGO_TARGET_DIR) 77s.
            "prebuilt: seconds; cargo --git ~83s measured / cold release ~77s; checkout similar (grammars + LTO)".into()
        }
        ExternalTool::Manvi => {
            "prebuilt: seconds; go install @latest / checkout: under a minute typically".into()
        }
    };
    PreflightReport {
        tool,
        ok: requirements.iter().any(|r| r.satisfies),
        requirements,
        estimate,
    }
}

pub fn verify_connected(tool: ExternalTool) -> VerifyReport {
    match tool {
        ExternalTool::Devmap => verify_devmap(),
        ExternalTool::Manvi => verify_manvi(),
    }
}

/// Fields GitPulse needs from `devmap doctor --json`.
///
/// Contract (DevCouncil `doctor_report`): `schema_version` (on disk, may be
/// null), `expected_schema_version` (what the binary speaks),
/// `code_graph_schema_version`, plus `linked_grammar_count` / `store_path` /
/// `version` which hosts may ignore.
struct DoctorSchemas {
    /// Binary's declared store schema — primary handshake value.
    expected_schema_version: Option<i32>,
    /// On-disk store schema when a sqlite exists; null otherwise.
    schema_version: Option<i32>,
    code_graph_schema_version: Option<u32>,
}

fn parse_doctor_json(v: &serde_json::Value) -> DoctorSchemas {
    let i32_field = |keys: &[&str]| {
        keys.iter().find_map(|k| {
            v.get(*k).and_then(|x| {
                if x.is_null() {
                    None
                } else {
                    x.as_i64().map(|n| n as i32)
                }
            })
        })
    };
    let u32_field = |keys: &[&str]| {
        keys.iter().find_map(|k| {
            v.get(*k).and_then(|x| {
                if x.is_null() {
                    None
                } else {
                    x.as_u64().map(|n| n as u32)
                }
            })
        })
    };
    DoctorSchemas {
        // Prefer the binary's declared schema. Legacy aliases kept so an older
        // doctor payload does not force a --version scrape fallback.
        expected_schema_version: i32_field(&[
            "expected_schema_version",
            "store_schema",
            "store_schema_version",
        ]),
        schema_version: i32_field(&["schema_version"]),
        code_graph_schema_version: u32_field(&["code_graph_schema_version", "code_graph_schema"]),
    }
}

fn verify_devmap() -> VerifyReport {
    let expected_store = crate::codeintel::SUPPORTED_STORE_SCHEMA;
    let expected_cg = devmap_query::code_graph::CODE_GRAPH_SCHEMA_VERSION;
    let binary = match crate::devmap::cli::resolve_binary() {
        Ok(r) => r.path,
        Err(e) => {
            return VerifyReport {
                tool: ExternalTool::Devmap,
                ok: false,
                binary: None,
                detail: e,
                store_schema: None,
                expected_store_schema: Some(expected_store),
                code_graph_schema: None,
                expected_code_graph_schema: Some(expected_cg),
                manvi_protocol: None,
                manvi_posture: None,
            };
        }
    };

    // Prefer `devmap doctor --json` over scraping `--version` prose.
    let mut cmd = Command::new(&binary);
    cmd.args(["doctor", "--json"]);
    if let Ok(run) = git_cli::run_bounded_capped(
        cmd,
        "devmap doctor",
        Duration::from_secs(10),
        None,
        256 * 1024,
    ) {
        if run.success {
            if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&run.stdout) {
                let parsed = parse_doctor_json(&v);
                // Handshake the binary's declared schema against the vendored
                // constant — not on-disk `schema_version`, which is null with
                // no store and can belong to a different repo's cwd.
                let spoken = parsed.expected_schema_version;
                let ok = spoken == Some(expected_store);
                let cg = parsed.code_graph_schema_version;
                return VerifyReport {
                    tool: ExternalTool::Devmap,
                    ok,
                    binary: Some(binary),
                    detail: if ok {
                        format!(
                            "doctor ok — expected_schema_version {expected_store}, code_graph_schema_version {}",
                            cg.map(|n| n.to_string()).unwrap_or_else(|| "?".into())
                        )
                    } else {
                        format!(
                            "doctor expected_schema_version {:?} ≠ vendored {expected_store}",
                            spoken
                        )
                    },
                    store_schema: spoken.or(parsed.schema_version),
                    expected_store_schema: Some(expected_store),
                    code_graph_schema: cg,
                    expected_code_graph_schema: Some(expected_cg),
                    manvi_protocol: None,
                    manvi_posture: None,
                };
            }
        }
    }

    let mut cmd = Command::new(&binary);
    cmd.arg("--version");
    let run = match git_cli::run_bounded_capped(
        cmd,
        "devmap --version",
        Duration::from_secs(5),
        None,
        64 * 1024,
    ) {
        Ok(r) => r,
        Err(e) => {
            return VerifyReport {
                tool: ExternalTool::Devmap,
                ok: false,
                binary: Some(binary),
                detail: e,
                store_schema: None,
                expected_store_schema: Some(expected_store),
                code_graph_schema: None,
                expected_code_graph_schema: Some(expected_cg),
                manvi_protocol: None,
                manvi_posture: None,
            };
        }
    };
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let store = scrape_schema(&text, "store schema");
    let ok = store == Some(expected_store);
    VerifyReport {
        tool: ExternalTool::Devmap,
        ok,
        binary: Some(binary),
        detail: if ok {
            format!("version handshake ok — store schema {expected_store}")
        } else {
            format!(
                "version text did not confirm store schema {expected_store}: {}",
                text.lines()
                    .next()
                    .unwrap_or("")
                    .chars()
                    .take(160)
                    .collect::<String>()
            )
        },
        store_schema: store,
        expected_store_schema: Some(expected_store),
        code_graph_schema: scrape_schema_u32(&text, "code graph schema"),
        expected_code_graph_schema: Some(expected_cg),
        manvi_protocol: None,
        manvi_posture: None,
    }
}

fn scrape_schema(text: &str, label: &str) -> Option<i32> {
    let lower = text.to_ascii_lowercase();
    let needle = label.to_ascii_lowercase();
    let idx = lower.find(&needle)?;
    let after = &text[idx + label.len()..];
    let digits: String = after
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

fn scrape_schema_u32(text: &str, label: &str) -> Option<u32> {
    scrape_schema(text, label).map(|v| v as u32)
}

fn verify_manvi() -> VerifyReport {
    match crate::harness::sidecar::handshake() {
        Ok((binary, hello)) => VerifyReport {
            tool: ExternalTool::Manvi,
            ok: true,
            binary: Some(binary),
            detail: format!(
                "hello ok — protocol {}, posture {}, {} ops",
                hello.protocol,
                hello.posture,
                hello.ops.len()
            ),
            store_schema: None,
            expected_store_schema: None,
            code_graph_schema: None,
            expected_code_graph_schema: None,
            manvi_protocol: Some(hello.protocol),
            manvi_posture: Some(hello.posture),
        },
        Err(e) => VerifyReport {
            tool: ExternalTool::Manvi,
            ok: false,
            binary: crate::harness::sidecar::resolve_binary(),
            detail: e.message(),
            store_schema: None,
            expected_store_schema: None,
            code_graph_schema: None,
            expected_code_graph_schema: None,
            manvi_protocol: None,
            manvi_posture: None,
        },
    }
}

/// Clone the public source repo into `parent_dir/<Name>` and persist source_root.
pub fn clone_source(tool: ExternalTool, parent_dir: &str) -> CloneSourceOutcome {
    let parent = PathBuf::from(parent_dir);
    if !parent.is_dir() {
        return CloneSourceOutcome {
            tool,
            ok: false,
            path: None,
            reason: Some(format!("{parent_dir} is not a directory")),
        };
    }
    let url = tool.public_git();
    match crate::engine::git_writer::GitWriter::clone_repo(url, parent_dir) {
        Ok(cloned) => {
            let dest = PathBuf::from(&cloned);
            match install_target(tool, &dest) {
                Ok(_) => {
                    let _ = tool_config::set_source_root(tool, &cloned);
                    CloneSourceOutcome {
                        tool,
                        ok: true,
                        path: Some(cloned),
                        reason: None,
                    }
                }
                Err(reason) => CloneSourceOutcome {
                    tool,
                    ok: false,
                    path: Some(cloned),
                    reason: Some(format!("clone landed but layout invalid: {reason}")),
                },
            }
        }
        Err(e) => CloneSourceOutcome {
            tool,
            ok: false,
            path: None,
            reason: Some(e),
        },
    }
}

/// Select which rung `assess_ladder` would pick when prerequisites are forced.
#[cfg(test)]
pub fn select_rung_for_test(rungs: &[RungStatus]) -> Option<InstallRung> {
    if rungs
        .iter()
        .any(|r| r.rung == InstallRung::AlreadyOnPath && r.available)
    {
        return Some(InstallRung::AlreadyOnPath);
    }
    rungs
        .iter()
        .find(|r| r.available && !matches!(r.rung, InstallRung::AlreadyOnPath))
        .map(|r| r.rung)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn find_sibling_walks_ancestors() {
        let tmp = TempDir::new().unwrap();
        let nest = tmp.path().join("a/b/c");
        fs::create_dir_all(&nest).unwrap();
        let sibling = tmp.path().join("a/DevCouncil");
        fs::create_dir_all(&sibling).unwrap();
        let found = find_sibling(&["DevCouncil"], &nest).expect("sibling");
        assert_eq!(found, sibling);
    }

    #[test]
    fn find_sibling_misses_cleanly() {
        let tmp = TempDir::new().unwrap();
        let nest = tmp.path().join("alone");
        fs::create_dir_all(&nest).unwrap();
        assert!(find_sibling(&["DevCouncil", "Manvi"], &nest).is_none());
    }

    #[test]
    fn install_target_requires_real_layout() {
        let tmp = TempDir::new().unwrap();
        let err = install_target(ExternalTool::Devmap, tmp.path()).unwrap_err();
        assert!(err.contains("devmap-cli"), "{err}");

        let crate_dir = tmp.path().join("rust-port/crates/devmap-cli");
        fs::create_dir_all(&crate_dir).unwrap();
        fs::write(
            crate_dir.join("Cargo.toml"),
            "[package]\nname=\"devmap-cli\"\n",
        )
        .unwrap();
        let ok = install_target(ExternalTool::Devmap, tmp.path()).unwrap();
        assert_eq!(ok, crate_dir);
    }

    #[test]
    fn manvi_install_target_accepts_module_root() {
        let tmp = TempDir::new().unwrap();
        let module = tmp.path().join("manvi");
        fs::create_dir_all(module.join("cmd/manvi")).unwrap();
        fs::write(module.join("go.mod"), "module manvi\n").unwrap();
        let ok = install_target(ExternalTool::Manvi, tmp.path()).unwrap();
        assert_eq!(ok, module);
    }

    #[test]
    fn documented_commands_match_product_contract() {
        let cmd = documented_command(ExternalTool::Devmap, Some(Path::new("/x/devmap-cli")));
        assert!(cmd.contains("cargo install --path"));
        assert!(cmd.contains("--locked"));
        assert!(cmd.contains("--force"));

        let cmd = documented_command(ExternalTool::Manvi, Some(Path::new("/x/manvi")));
        assert!(cmd.contains("go -C"));
        assert!(cmd.contains("install ./cmd/manvi"));
    }

    #[test]
    fn remote_commands_use_public_urls() {
        let cmd = remote_install_command(ExternalTool::Devmap);
        assert!(cmd.contains(PUBLIC_DEVMAP_GIT));
        assert!(cmd.contains("devmap-cli"));
        let cmd = remote_install_command(ExternalTool::Manvi);
        assert!(cmd.contains(MANVI_GO_INSTALL));
        assert_eq!(
            MANVI_GO_INSTALL,
            "github.com/bharathvbcr/Manvi/manvi/cmd/manvi@latest"
        );
    }

    #[test]
    fn doctor_json_reads_expected_schema_not_legacy_store_schema() {
        // Real `devmap doctor --json` contract from DevCouncil — never
        // `store_schema`. A binary with no on-disk store still handshakes.
        let v: serde_json::Value = serde_json::json!({
            "schema_version": null,
            "expected_schema_version": 19,
            "code_graph_schema_version": 2,
            "linked_grammar_count": 33,
            "store_path": "./.devcouncil/codeintel/devmap.sqlite",
            "version": "0.1.0"
        });
        let parsed = parse_doctor_json(&v);
        assert_eq!(parsed.expected_schema_version, Some(19));
        assert_eq!(parsed.schema_version, None);
        assert_eq!(parsed.code_graph_schema_version, Some(2));

        let with_disk: serde_json::Value = serde_json::json!({
            "schema_version": 19,
            "expected_schema_version": 19,
            "code_graph_schema_version": 2
        });
        let parsed = parse_doctor_json(&with_disk);
        assert_eq!(parsed.schema_version, Some(19));
        assert_eq!(parsed.expected_schema_version, Some(19));
    }

    #[test]
    fn preflight_devmap_estimate_cites_measured_times() {
        let report = preflight(ExternalTool::Devmap);
        assert!(
            report.estimate.contains("83") && report.estimate.contains("77"),
            "estimate must cite phase-0 measured seconds, got: {}",
            report.estimate
        );
    }

    #[test]
    fn explicit_missing_devmap_env_is_refused() {
        let _lock = crate::harness::sidecar::test_serial();
        unsafe {
            std::env::set_var("GITPULSE_DEVMAP_BIN", "/no/such/devmap-binary");
        }
        tool_capability::invalidate(ExternalTool::Devmap);
        let status = resolve_status(ExternalTool::Devmap);
        unsafe {
            std::env::remove_var("GITPULSE_DEVMAP_BIN");
        }
        tool_capability::invalidate(ExternalTool::Devmap);
        assert!(!status.installed);
        assert_eq!(status.lookup, ToolLookup::ExplicitMissing);
        assert!(
            status
                .reason
                .as_deref()
                .unwrap_or("")
                .contains("GITPULSE_DEVMAP_BIN"),
            "{:?}",
            status.reason
        );
    }

    #[test]
    fn explicit_missing_manvi_env_is_refused() {
        let _lock = crate::harness::sidecar::test_serial();
        unsafe {
            std::env::set_var("GITPULSE_MANVI_BIN", "/no/such/manvi-binary");
        }
        tool_capability::invalidate(ExternalTool::Manvi);
        let status = resolve_status(ExternalTool::Manvi);
        unsafe {
            std::env::remove_var("GITPULSE_MANVI_BIN");
        }
        tool_capability::invalidate(ExternalTool::Manvi);
        assert!(!status.installed);
        assert_eq!(status.lookup, ToolLookup::ExplicitMissing);
    }

    #[test]
    fn install_without_source_fails_closed_on_forced_checkout_rung() {
        let _lock = crate::harness::sidecar::test_serial();
        unsafe {
            std::env::set_var("GITPULSE_DEVCOUNCIL_ROOT", "/no/such/devcouncil-root");
        }
        tool_capability::invalidate(ExternalTool::Devmap);
        let outcome = install_with_rung(ExternalTool::Devmap, Some(InstallRung::LocalCheckout));
        unsafe {
            std::env::remove_var("GITPULSE_DEVCOUNCIL_ROOT");
        }
        tool_capability::invalidate(ExternalTool::Devmap);
        assert!(!outcome.ok);
        assert!(
            outcome
                .reason
                .as_deref()
                .unwrap_or("")
                .contains("GITPULSE_DEVCOUNCIL_ROOT"),
            "{:?}",
            outcome.reason
        );
    }

    #[test]
    fn status_all_returns_both_tools() {
        let all = status_all();
        assert_eq!(all.devmap.tool, ExternalTool::Devmap);
        assert_eq!(all.manvi.tool, ExternalTool::Manvi);
        assert!(!all.devmap.install_command.is_empty());
        assert!(!all.manvi.install_command.is_empty());
        assert!(!all.devmap.ladder.is_empty());
    }

    #[test]
    fn rung_selection_skips_blocked_prebuilt() {
        let rungs = vec![
            RungStatus {
                rung: InstallRung::AlreadyOnPath,
                available: false,
                block: Some("missing".into()),
                command: None,
                cost: "nothing".into(),
            },
            RungStatus {
                rung: InstallRung::PrebuiltRelease,
                available: false,
                block: Some("no release for this platform".into()),
                command: None,
                cost: "seconds".into(),
            },
            RungStatus {
                rung: InstallRung::ToolchainRemote,
                available: true,
                block: None,
                command: Some("cargo install --git …".into()),
                cost: "minutes".into(),
            },
            RungStatus {
                rung: InstallRung::LocalCheckout,
                available: true,
                block: None,
                command: Some("cargo install --path …".into()),
                cost: "minutes".into(),
            },
        ];
        assert_eq!(
            select_rung_for_test(&rungs),
            Some(InstallRung::ToolchainRemote)
        );
    }

    #[test]
    fn rung_selection_prefers_already_on_path() {
        let rungs = vec![
            RungStatus {
                rung: InstallRung::AlreadyOnPath,
                available: true,
                block: None,
                command: None,
                cost: "nothing".into(),
            },
            RungStatus {
                rung: InstallRung::PrebuiltRelease,
                available: true,
                block: None,
                command: Some("download".into()),
                cost: "seconds".into(),
            },
        ];
        assert_eq!(
            select_rung_for_test(&rungs),
            Some(InstallRung::AlreadyOnPath)
        );
    }

    #[test]
    fn saved_config_binary_precedes_path_and_tags_lookup() {
        let _lock = crate::harness::sidecar::test_serial();
        let dir = TempDir::new().unwrap();
        let bin = dir.path().join("fake-devmap");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::write(&bin, "#!/bin/sh\necho 'devmap 0.0.0 (store schema 19)'\n").unwrap();
            let mut perms = fs::metadata(&bin).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&bin, perms).unwrap();
        }
        #[cfg(not(unix))]
        {
            fs::write(&bin, b"x").unwrap();
        }
        let cfg_path = dir.path().join("tools.json");
        unsafe {
            std::env::set_var(crate::tool_config::TOOL_CONFIG_ENV, &cfg_path);
            std::env::remove_var("GITPULSE_DEVMAP_BIN");
        }
        crate::tool_config::invalidate_cache();
        tool_capability::invalidate(ExternalTool::Devmap);
        let mut cfg = crate::tool_config::ToolConfig::default();
        cfg.devmap.binary = Some(bin.display().to_string());
        crate::tool_config::save(&cfg).unwrap();
        tool_capability::invalidate(ExternalTool::Devmap);

        let status = resolve_status(ExternalTool::Devmap);
        assert!(status.installed, "{:?}", status.reason);
        assert_eq!(status.lookup, ToolLookup::SavedConfig);
        assert_eq!(
            status.path.as_deref(),
            Some(bin.display().to_string().as_str())
        );

        unsafe {
            std::env::remove_var(crate::tool_config::TOOL_CONFIG_ENV);
        }
        crate::tool_config::invalidate_cache();
        tool_capability::invalidate(ExternalTool::Devmap);
    }

    #[test]
    fn public_urls_are_constants() {
        assert!(PUBLIC_DEVMAP_REPO.contains("DevCouncil"));
        assert!(PUBLIC_MANVI_REPO.contains("Manvi"));
    }
}
