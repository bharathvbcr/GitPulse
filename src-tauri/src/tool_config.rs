//! Persisted paths for optional external CLIs (`devmap`, `manvi`).
//!
//! Lives in the platform config dir (mirroring [`crate::logging`]'s
//! `default_log_dir` shape, but under Application Support / config rather
//! than Logs). `GITPULSE_TOOL_CONFIG` overrides the path so tests cannot
//! write the user's real file.
//!
//! Writes are atomic (temp + `sync_all` + rename). A saved path that no
//! longer resolves is reported as stale with its reason — never silently
//! dropped, never silently used.

use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

pub const CONFIG_VERSION: u32 = 1;
pub const TOOL_CONFIG_ENV: &str = "GITPULSE_TOOL_CONFIG";

static CONFIG_ENV_LOCK: Mutex<()> = Mutex::new(());

/// Serialize tests (and any other callers) that mutate [`TOOL_CONFIG_ENV`].
pub(crate) fn lock_config_env() -> std::sync::MutexGuard<'static, ()> {
    CONFIG_ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn is_false(v: &bool) -> bool {
    !*v
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolPaths {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_root: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub disabled: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct OnboardingState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skipped_tools: Vec<String>,
    #[serde(default)]
    pub dismissed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolConfig {
    pub version: u32,
    #[serde(default)]
    pub devmap: ToolPaths,
    #[serde(default)]
    pub manvi: ToolPaths,
    #[serde(default)]
    pub onboarding: OnboardingState,
}

impl Default for ToolConfig {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
            devmap: ToolPaths::default(),
            manvi: ToolPaths::default(),
            onboarding: OnboardingState::default(),
        }
    }
}

/// Why a saved path cannot be used. Distinct from "nothing configured".
/// Named apart from the frontend metrics `StaleReason` union so the enum
/// contract does not conflate the two.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConfigPathStaleReason {
    Missing,
    NotAFile,
    NotADirectory,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StalePath {
    pub field: String,
    pub path: String,
    pub reason: ConfigPathStaleReason,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolConfigView {
    pub config: ToolConfig,
    pub path: String,
    pub stale: Vec<StalePath>,
}

fn cache() -> &'static Mutex<Option<(PathBuf, ToolConfig)>> {
    static CACHE: OnceLock<Mutex<Option<(PathBuf, ToolConfig)>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

/// Where the platform keeps application support / config for GitPulse.
pub fn default_config_dir() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME").map(|home| {
            PathBuf::from(home)
                .join("Library")
                .join("Application Support")
                .join("GitPulse")
        })
    }
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("APPDATA").map(|base| PathBuf::from(base).join("GitPulse"))
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
            .map(|base| base.join("gitpulse"))
    }
}

pub fn config_path() -> Result<PathBuf, String> {
    if let Ok(explicit) = std::env::var(TOOL_CONFIG_ENV) {
        if explicit.is_empty() {
            return Err(format!("{TOOL_CONFIG_ENV} is set but empty"));
        }
        return Ok(PathBuf::from(explicit));
    }
    let dir = default_config_dir().ok_or_else(|| {
        "cannot resolve platform config directory (HOME / APPDATA / XDG_CONFIG_HOME unset)"
            .to_string()
    })?;
    Ok(dir.join("tools.json"))
}

fn parse_config(text: &str) -> Result<ToolConfig, String> {
    let cfg: ToolConfig =
        serde_json::from_str(text).map_err(|e| format!("tools.json is not valid JSON: {e}"))?;
    if cfg.version == 0 || cfg.version > CONFIG_VERSION {
        return Err(format!(
            "unsupported tools.json version {} (this build speaks {CONFIG_VERSION})",
            cfg.version
        ));
    }
    Ok(cfg)
}

/// Load the config from disk (or default). Does not refuse on missing file.
pub fn load() -> Result<ToolConfig, String> {
    let path = config_path()?;
    {
        let guard = cache()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((cached_path, cfg)) = guard.as_ref() {
            if *cached_path == path {
                return Ok(cfg.clone());
            }
        }
    }
    let cfg = if path.is_file() {
        let text = fs::read_to_string(&path)
            .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
        parse_config(&text)?
    } else {
        ToolConfig::default()
    };
    *cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some((path, cfg.clone()));
    Ok(cfg)
}

/// Invalidate the in-process cache so the next [`load`] re-reads disk.
pub fn invalidate_cache() {
    *cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
}

/// Atomic write: sibling temp, `sync_all`, rename.
pub fn save(config: &ToolConfig) -> Result<PathBuf, String> {
    let path = config_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create {}: {e}", parent.display()))?;
    }
    let mut to_write = config.clone();
    to_write.version = CONFIG_VERSION;
    let body = serde_json::to_string_pretty(&to_write)
        .map_err(|e| format!("failed to serialise tools.json: {e}"))?;
    let tmp = path.with_extension("json.tmp");
    {
        let mut file =
            File::create(&tmp).map_err(|e| format!("failed to create {}: {e}", tmp.display()))?;
        file.write_all(body.as_bytes())
            .map_err(|e| format!("failed to write {}: {e}", tmp.display()))?;
        file.sync_all()
            .map_err(|e| format!("failed to sync {}: {e}", tmp.display()))?;
    }
    fs::rename(&tmp, &path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!(
            "failed to rename {} → {}: {e}",
            tmp.display(),
            path.display()
        )
    })?;
    *cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some((path.clone(), to_write));
    crate::tool_capability::invalidate_all();
    Ok(path)
}

fn stale_binary(field: &str, path: &str) -> Option<StalePath> {
    let p = Path::new(path);
    if p.is_file() {
        return None;
    }
    let reason = if p.exists() {
        ConfigPathStaleReason::NotAFile
    } else {
        ConfigPathStaleReason::Missing
    };
    Some(StalePath {
        field: field.into(),
        path: path.into(),
        reason: reason.clone(),
        detail: match reason {
            ConfigPathStaleReason::Missing => format!("{path} no longer exists"),
            ConfigPathStaleReason::NotAFile => format!("{path} exists but is not a file"),
            ConfigPathStaleReason::NotADirectory => unreachable!(),
        },
    })
}

fn stale_dir(field: &str, path: &str) -> Option<StalePath> {
    let p = Path::new(path);
    if p.is_dir() {
        return None;
    }
    let reason = if p.exists() {
        ConfigPathStaleReason::NotADirectory
    } else {
        ConfigPathStaleReason::Missing
    };
    Some(StalePath {
        field: field.into(),
        path: path.into(),
        reason: reason.clone(),
        detail: match reason {
            ConfigPathStaleReason::Missing => format!("{path} no longer exists"),
            ConfigPathStaleReason::NotADirectory => format!("{path} exists but is not a directory"),
            ConfigPathStaleReason::NotAFile => unreachable!(),
        },
    })
}

pub fn collect_stale(config: &ToolConfig) -> Vec<StalePath> {
    let mut out = Vec::new();
    if let Some(p) = config.devmap.binary.as_deref() {
        if let Some(s) = stale_binary("devmap.binary", p) {
            out.push(s);
        }
    }
    if let Some(p) = config.devmap.source_root.as_deref() {
        if let Some(s) = stale_dir("devmap.source_root", p) {
            out.push(s);
        }
    }
    if let Some(p) = config.manvi.binary.as_deref() {
        if let Some(s) = stale_binary("manvi.binary", p) {
            out.push(s);
        }
    }
    if let Some(p) = config.manvi.source_root.as_deref() {
        if let Some(s) = stale_dir("manvi.source_root", p) {
            out.push(s);
        }
    }
    out
}

pub fn view() -> Result<ToolConfigView, String> {
    let path = config_path()?;
    let config = load()?;
    let stale = collect_stale(&config);
    Ok(ToolConfigView {
        config,
        path: path.display().to_string(),
        stale,
    })
}

/// Saved binary path for a tool, only when the file still exists.
pub fn saved_binary(tool: crate::tool_install::ExternalTool) -> Option<String> {
    let cfg = load().ok()?;
    let path = match tool {
        crate::tool_install::ExternalTool::Devmap => cfg.devmap.binary.clone(),
        crate::tool_install::ExternalTool::Manvi => cfg.manvi.binary.clone(),
    }?;
    if Path::new(&path).is_file() {
        Some(path)
    } else {
        None
    }
}

/// Saved source root for a tool, only when the directory still exists.
pub fn saved_source_root(tool: crate::tool_install::ExternalTool) -> Option<PathBuf> {
    let cfg = load().ok()?;
    let path = match tool {
        crate::tool_install::ExternalTool::Devmap => cfg.devmap.source_root.clone(),
        crate::tool_install::ExternalTool::Manvi => cfg.manvi.source_root.clone(),
    }?;
    let pb = PathBuf::from(&path);
    if pb.is_dir() {
        Some(pb)
    } else {
        None
    }
}

/// Persist a binary path after a successful install.
pub fn set_binary(tool: crate::tool_install::ExternalTool, binary: &str) -> Result<(), String> {
    let mut cfg = load()?;
    match tool {
        crate::tool_install::ExternalTool::Devmap => {
            cfg.devmap.binary = Some(binary.to_string());
        }
        crate::tool_install::ExternalTool::Manvi => {
            cfg.manvi.binary = Some(binary.to_string());
        }
    }
    save(&cfg)?;
    Ok(())
}

pub fn set_source_root(tool: crate::tool_install::ExternalTool, root: &str) -> Result<(), String> {
    let mut cfg = load()?;
    match tool {
        crate::tool_install::ExternalTool::Devmap => {
            cfg.devmap.source_root = Some(root.to_string());
        }
        crate::tool_install::ExternalTool::Manvi => {
            cfg.manvi.source_root = Some(root.to_string());
        }
    }
    save(&cfg)?;
    Ok(())
}

pub fn set_onboarding(state: OnboardingState) -> Result<(), String> {
    let mut cfg = load()?;
    cfg.onboarding = state;
    save(&cfg)?;
    Ok(())
}

pub fn clear_binary(tool: crate::tool_install::ExternalTool) -> Result<(), String> {
    let mut cfg = load()?;
    match tool {
        crate::tool_install::ExternalTool::Devmap => cfg.devmap.binary = None,
        crate::tool_install::ExternalTool::Manvi => cfg.manvi.binary = None,
    }
    save(&cfg)?;
    Ok(())
}

pub fn is_disabled(tool: crate::tool_install::ExternalTool) -> bool {
    let Ok(cfg) = load() else {
        return false;
    };
    match tool {
        crate::tool_install::ExternalTool::Devmap => cfg.devmap.disabled,
        crate::tool_install::ExternalTool::Manvi => cfg.manvi.disabled,
    }
}

pub fn set_disabled(tool: crate::tool_install::ExternalTool, disabled: bool) -> Result<(), String> {
    let mut cfg = load()?;
    match tool {
        crate::tool_install::ExternalTool::Devmap => cfg.devmap.disabled = disabled,
        crate::tool_install::ExternalTool::Manvi => cfg.manvi.disabled = disabled,
    }
    save(&cfg)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_temp_config(f: impl FnOnce(&Path)) {
        let _guard = crate::tool_config::lock_config_env();
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("tools.json");
        // SAFETY: serialized behind lock_config_env; restored below.
        unsafe {
            std::env::set_var(TOOL_CONFIG_ENV, &path);
        }
        invalidate_cache();
        f(&path);
        unsafe {
            std::env::remove_var(TOOL_CONFIG_ENV);
        }
        invalidate_cache();
    }

    #[test]
    fn load_defaults_when_missing() {
        with_temp_config(|path| {
            assert!(!path.exists());
            let cfg = load().unwrap();
            assert_eq!(cfg.version, CONFIG_VERSION);
            assert!(cfg.devmap.binary.is_none());
        });
    }

    #[test]
    fn atomic_save_round_trips() {
        with_temp_config(|path| {
            let mut cfg = ToolConfig::default();
            cfg.devmap.binary = Some("/tmp/fake-devmap".into());
            save(&cfg).unwrap();
            assert!(path.is_file());
            invalidate_cache();
            let loaded = load().unwrap();
            assert_eq!(loaded.devmap.binary.as_deref(), Some("/tmp/fake-devmap"));
        });
    }

    #[test]
    fn stale_binary_is_reported_not_silently_dropped() {
        with_temp_config(|_| {
            let mut cfg = ToolConfig::default();
            cfg.devmap.binary = Some("/no/such/devmap-binary-for-stale-test".into());
            save(&cfg).unwrap();
            let view = view().unwrap();
            assert_eq!(view.stale.len(), 1);
            assert_eq!(view.stale[0].field, "devmap.binary");
            assert_eq!(view.stale[0].reason, ConfigPathStaleReason::Missing);
            // Config still carries the path — not silently dropped.
            assert_eq!(
                view.config.devmap.binary.as_deref(),
                Some("/no/such/devmap-binary-for-stale-test")
            );
            assert!(saved_binary(crate::tool_install::ExternalTool::Devmap).is_none());
        });
    }

    #[test]
    fn disabled_round_trips_and_is_reported() {
        with_temp_config(|_| {
            set_disabled(crate::tool_install::ExternalTool::Devmap, true).unwrap();
            assert!(is_disabled(crate::tool_install::ExternalTool::Devmap));
            set_disabled(crate::tool_install::ExternalTool::Devmap, false).unwrap();
            assert!(!is_disabled(crate::tool_install::ExternalTool::Devmap));
        });
    }

    #[test]
    fn truncated_temp_does_not_replace_good_config() {
        with_temp_config(|path| {
            let mut cfg = ToolConfig::default();
            cfg.manvi.source_root = Some("/good/root".into());
            save(&cfg).unwrap();
            // A half-written temp beside the real file must not be loadable as config.
            let tmp = path.with_extension("json.tmp");
            fs::write(&tmp, "{").unwrap();
            invalidate_cache();
            let loaded = load().unwrap();
            assert_eq!(loaded.manvi.source_root.as_deref(), Some("/good/root"));
        });
    }
}
