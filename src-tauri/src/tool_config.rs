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

#[cfg(test)]
static CONFIG_ENV_LOCK: Mutex<()> = Mutex::new(());

/// Serialize tests that mutate [`TOOL_CONFIG_ENV`].
#[cfg(test)]
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

/// What an agent CLI started from a terminal tab begins with.
///
/// Host-scoped, for the same reason [`SessionAlertSettings`] is: a terminal
/// session exists whether or not the user has ever opened a task, so a
/// workbench database is not available to read at spawn time.
///
/// # Why the flags are not here
///
/// This stores mode *names* only. Which flag means "plan" for which CLI is
/// [`crate::workbench::terminal_command`]'s table and nowhere else — the same
/// table the workbench handoff uses. Storing expanded flags would freeze one
/// provider's spelling into the user's config file, where a CLI that renamed
/// a flag would leave a stored value nothing validates.
///
/// # Why bypass is stored but never silently applied
///
/// `sanitizeHandoff` refuses to *restore* bypass for the workbench handoff,
/// on the grounds that a mode which disables a safety control must be chosen
/// deliberately. That reasoning is about acknowledgement, not about storage:
/// what must not happen is a launch that skips permission checks without the
/// user saying so at that moment. So a stored `bypass` is honoured as a
/// preference and the acknowledgement is asked for per launch —
/// `terminal_command::apply_permission_mode` refuses the expansion without
/// one, so a frontend that forgot to ask cannot produce a bypassed session.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AgentDefaults {
    /// Default permission mode per agent CLI, keyed by launcher name
    /// (`claude`, `codex`, `grok`, `agy`).
    ///
    /// A map rather than one field per provider so that a provider gaining a
    /// policy does not change this shape, and absent means "the CLI's own
    /// default" — which is distinct from any mode this could name.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub permission: std::collections::BTreeMap<String, String>,
}

impl AgentDefaults {
    /// Rejects anything the launch path would later have to refuse or ignore.
    ///
    /// Validity is asked of the policy table rather than restated here, so a
    /// mode this accepts is exactly a mode that expands. The bounds come
    /// first: they are what keeps a hand-edited `tools.json` from making the
    /// settings panel render an unbounded list.
    pub fn validate(&self) -> Result<(), String> {
        if self.permission.len() > 16 {
            return Err("Too many agent permission defaults".into());
        }
        for (launcher, mode) in &self.permission {
            crate::workbench::terminal_command::validate_permission_default(launcher, mode)?;
        }
        Ok(())
    }
}

/// Reads `agent_defaults` without being able to fail.
///
/// Goes through `Value` rather than trying and retrying the deserializer: a
/// `Deserializer` is consumed by the attempt, so a failure part-way through
/// cannot be un-done. JSON is self-describing, so buffering the block first
/// costs one allocation and makes the retry possible at all.
fn lenient_agent_defaults<'de, D>(deserializer: D) -> Result<AgentDefaults, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = serde_json::Value::deserialize(deserializer)?;
    Ok(serde_json::from_value(raw).unwrap_or_else(|error| {
        log::warn!(
            target: "tool_config",
            "ignoring unreadable agent_defaults block, using no stored default: {error}"
        );
        AgentDefaults::default()
    }))
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

/// How GitPulse handles an agent session asking for attention.
///
/// Host-scoped and kept here rather than in the workbench profile for one
/// reason that decides it: the PTY reader thread and the cleaner-style native
/// worker both have to read this without a workbench database, and a terminal
/// session exists whether or not the user has ever opened a task. The
/// workbench's own `work_notification_settings` row governs *activity* notices
/// and is a different question with a different lifetime.
///
/// Quiet hours are local minutes of the day, `0..=1439`, and are either both
/// set or both unset — the same shape, and the same wrapping rule, that the
/// store applies to activity notices. `notify::policy::in_quiet_hours` is the
/// single implementation of that rule, and a contract test drives the store to
/// prove the two still agree.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SessionAlertSettings {
    /// Whether a terminal session may raise an OS notification at all.
    pub enabled: bool,
    /// Whether those notifications carry a sound.
    pub sound: bool,
    /// Whether a plain login shell's bell counts as attention.
    ///
    /// Off by default and deliberately so: `BEL` from a shell is usually
    /// readline telling you a completion was ambiguous, not a request for you.
    /// Agent launchers are the case this feature exists for.
    pub shell_bell: bool,
    /// Whether GitPulse adds each CLI's documented notification flags to the
    /// agent sessions it launches. Session-only; no file of the user's is
    /// written, and a key the user set elsewhere is otherwise untouched.
    pub configure_agents: bool,
    /// Whether GitPulse listens on its local socket for agent hook reports.
    pub hook_bridge: bool,
    pub quiet_start: Option<u16>,
    pub quiet_end: Option<u16>,
}

impl Default for SessionAlertSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            sound: false,
            shell_bell: false,
            configure_agents: true,
            hook_bridge: true,
            quiet_start: None,
            quiet_end: None,
        }
    }
}

impl SessionAlertSettings {
    pub fn validate(&self) -> Result<(), String> {
        let bound = |m: Option<u16>| m.is_none_or(|m| m <= 1439);
        if !bound(self.quiet_start) || !bound(self.quiet_end) {
            return Err("Quiet hours must be local minutes of the day".into());
        }
        if self.quiet_start.is_some() != self.quiet_end.is_some() {
            return Err("Quiet hours need both a start and an end".into());
        }
        if self.quiet_start.is_some() && self.quiet_start == self.quiet_end {
            return Err("Quiet-hour start and end must differ".into());
        }
        Ok(())
    }
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
    /// Absent in a `tools.json` written before this existed, which is exactly
    /// why the default is the shipped behaviour rather than "off".
    #[serde(default)]
    pub session_alerts: SessionAlertSettings,
    /// Absent in a `tools.json` written before this existed; the default is
    /// "no stored default", which leaves every CLI on its own.
    ///
    /// Read leniently, unlike its neighbours. `#[serde(default)]` covers a
    /// *missing* block; a malformed one still fails the whole parse, and the
    /// whole parse failing costs the user every other setting in the file —
    /// their saved `devmap` path included. A preference this feature added
    /// must not be able to do that, so a block this cannot read degrades to
    /// "no stored default" on its own and says so in the log.
    #[serde(default, deserialize_with = "lenient_agent_defaults")]
    pub agent_defaults: AgentDefaults,
}

impl Default for ToolConfig {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
            devmap: ToolPaths::default(),
            manvi: ToolPaths::default(),
            onboarding: OnboardingState::default(),
            session_alerts: SessionAlertSettings::default(),
            agent_defaults: AgentDefaults::default(),
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

/// The session-notification preferences, or the defaults when `tools.json`
/// cannot be read.
///
/// Falling back to the defaults rather than to silence is deliberate: an
/// unreadable config must not be the reason an agent waits unannounced. The
/// read error is surfaced through [`view`], which is where a fault belongs.
pub fn session_alerts() -> SessionAlertSettings {
    load()
        .ok()
        .map(|cfg| cfg.session_alerts)
        .filter(|cfg| cfg.validate().is_ok())
        .unwrap_or_default()
}

/// The stored agent defaults, or the shipped ones.
///
/// Validated on the way out as well as on the way in. `tools.json` is a plain
/// file a user may edit, and a mode that no longer expands must degrade to
/// "the CLI's own default" rather than reach a spawn that would refuse it —
/// an invalid stored value should cost a preference, not a terminal tab.
/// Invalid entries are dropped individually so one bad key cannot discard the
/// rest, and the drop is logged rather than silent.
pub fn agent_defaults() -> AgentDefaults {
    let Ok(cfg) = load() else {
        return AgentDefaults::default();
    };
    let mut defaults = cfg.agent_defaults;
    defaults.permission.retain(|launcher, mode| {
        let ok = crate::workbench::terminal_command::validate_permission_default(launcher, mode)
            .is_ok();
        if !ok {
            log::warn!(
                target: "tool_config",
                "ignoring stored permission default {launcher}={mode}: not a mode this build can apply"
            );
        }
        ok
    });
    if defaults.permission.len() > 16 {
        log::warn!(target: "tool_config", "ignoring oversized agent permission defaults");
        defaults.permission.clear();
    }
    defaults
}

pub fn set_agent_defaults(next: AgentDefaults) -> Result<(), String> {
    next.validate()?;
    let mut cfg = load()?;
    cfg.agent_defaults = next;
    save(&cfg)?;
    Ok(())
}

pub fn set_session_alerts(next: SessionAlertSettings) -> Result<(), String> {
    next.validate()?;
    let mut cfg = load()?;
    cfg.session_alerts = next;
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
        let serial = crate::tool_config::lock_config_env();
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("tools.json");
        // Dropped before `dir`, so the variable stops naming the temporary
        // directory before the directory goes away — and before `serial`, so
        // the restore is still serialized.
        let _env = crate::test_support::env::bind_env(&serial)
            .set(TOOL_CONFIG_ENV, &path)
            .invalidating(invalidate_cache);
        f(&path);
    }

    /// Writes a `tools.json` by hand, the way a user or an older build could.
    fn write_raw(path: &Path, json: &str) {
        fs::write(path, json).unwrap();
        invalidate_cache();
    }

    #[test]
    fn agent_defaults_round_trip_and_reject_what_cannot_be_applied() {
        with_temp_config(|_| {
            let mut defaults = AgentDefaults::default();
            defaults
                .permission
                .insert("claude".into(), "acceptEdits-ish".into());
            assert!(
                set_agent_defaults(defaults.clone()).is_err(),
                "a mode the policy table cannot expand was stored"
            );

            defaults.permission.clear();
            defaults.permission.insert("claude".into(), "edit".into());
            defaults.permission.insert("codex".into(), "bypass".into());
            set_agent_defaults(defaults.clone()).unwrap();
            assert_eq!(agent_defaults(), defaults);

            // A launcher with no policy is refused at the boundary rather than
            // stored and ignored later.
            let mut bad = AgentDefaults::default();
            bad.permission.insert("manvi".into(), "edit".into());
            assert!(set_agent_defaults(bad.clone()).is_err());
            bad.permission.clear();
            bad.permission.insert("shell".into(), "edit".into());
            assert!(set_agent_defaults(bad).is_err());
        });
    }

    /// A preference file is editable by hand and readable by a build that has
    /// since dropped a mode. An entry that can no longer be applied must cost
    /// that one preference — not the rest of them, and not the terminal.
    #[test]
    fn a_hand_edited_config_degrades_per_entry_rather_than_wholesale() {
        with_temp_config(|path| {
            write_raw(
                path,
                r#"{"version":1,"agent_defaults":{"permission":{
                    "claude":"edit","codex":"teleport","manvi":"edit","shell":"ask","nope":"ask"
                }}}"#,
            );
            let defaults = agent_defaults();
            assert_eq!(defaults.permission.len(), 1);
            assert_eq!(defaults.permission.get("claude").unwrap(), "edit");
        });
    }

    #[test]
    fn an_oversized_permission_map_is_dropped_rather_than_rendered() {
        with_temp_config(|path| {
            let entries = (0..24)
                .map(|i| format!(r#""launcher{i}":"edit""#))
                .collect::<Vec<_>>()
                .join(",");
            write_raw(
                path,
                &format!(r#"{{"version":1,"agent_defaults":{{"permission":{{{entries}}}}}}}"#),
            );
            assert!(agent_defaults().permission.is_empty());
        });
    }

    /// The absent-file case is the shipped one and must mean "every CLI on its
    /// own", which is the least authority this feature can grant.
    #[test]
    fn agent_defaults_are_empty_when_nothing_was_ever_stored() {
        with_temp_config(|path| {
            assert!(!path.exists());
            assert_eq!(agent_defaults(), AgentDefaults::default());
            assert!(agent_defaults().permission.is_empty());
        });
    }

    /// A config whose `agent_defaults` is the wrong shape entirely must not
    /// take the rest of the file down with it.
    #[test]
    fn a_malformed_agent_defaults_block_does_not_discard_the_other_settings() {
        with_temp_config(|path| {
            write_raw(
                path,
                r#"{"version":1,"devmap":{"binary":"/tmp/fake"},"agent_defaults":{"permission":"edit"}}"#,
            );
            // The block is unreadable, so the defaults are the shipped ones and
            // the unrelated saved path is still reported.
            assert!(agent_defaults().permission.is_empty());
            assert_eq!(
                load().map(|c| c.devmap.binary).unwrap_or_default(),
                Some("/tmp/fake".to_owned())
            );
        });
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
