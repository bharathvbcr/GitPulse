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
    /// How many task attempts may be live at once across every repository,
    /// passed to the store as `runs.prepare`'s `max_active_runs`. Absent means
    /// the store's own default ([`DEFAULT_LIVE_RUNS`]); the bound is the
    /// store's ceiling ([`MAX_LIVE_RUNS`]), a resource limit on one machine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_live_runs: Option<u32>,
    /// Which of Claude Code's settings files an agent GitPulse starts in a
    /// terminal loads, passed as `--setting-sources`. Absent means the CLI's
    /// own default — all of [`CLAUDE_SETTING_SOURCES`] — so nothing changes
    /// for a user who never chose; a stored value is a non-empty proper
    /// subset in that order.
    ///
    /// Leaving out `project` and `local` stops a repository's own
    /// `.claude/settings*.json` from widening the permissions of an agent
    /// working in it, and also drops that project's allow-lists and hooks,
    /// which is why it is a choice and not a default. The managed lane always
    /// runs with `user` only; that is Manvi's decision, not this one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude_setting_sources: Option<Vec<String>>,
    /// How many terminal sessions may be open at once across every
    /// repository — shells, agent tabs and task terminals alike. Absent means
    /// [`DEFAULT_TERMINAL_SESSIONS`]; the bound is [`MAX_TERMINAL_SESSIONS`].
    /// The app applies it to its live session registry at start and on save.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_terminal_sessions: Option<u32>,
    /// Which model each agent CLI starts with, keyed by launcher name — for a
    /// plain agent tab and a task terminal attempt alike. Absent means the
    /// CLI's own choice (its settings files, its environment, its default),
    /// which is why an entry is never stored empty.
    ///
    /// Model *names* and levels only, like `permission`: which flag carries
    /// them is `terminal_command::model_control`'s table. The managed lane is
    /// not affected — Manvi builds that command line.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub models: std::collections::BTreeMap<String, ModelChoice>,
}

/// One launcher's model settings. Every field is optional and absent means
/// "whatever the CLI would choose"; which fields a launcher takes is
/// `terminal_command::model_fields`, and a field it does not take is refused
/// at save rather than silently left out of the launch.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ModelChoice {
    /// The model: an alias (`opus`, `opusplan`, `sonnet[1m]`), a full id, or
    /// for Antigravity a slug from `agy models`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Reasoning effort, one of `terminal_command::EFFORT_LEVELS`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// Models Claude Code falls back to, in order, when the main one is
    /// overloaded or unavailable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback: Option<Vec<String>>,
    /// The model Claude Code's server-side advisor tool consults.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub advisor: Option<String>,
}

impl ModelChoice {
    /// Whether nothing is chosen, which is stored as no entry at all.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// The most launchers a model map may name; there are four today.
const MAX_MODEL_ENTRIES: usize = 16;

/// Terminal sessions open at once when nothing is stored.
pub const DEFAULT_TERMINAL_SESSIONS: u32 = crate::terminal::DEFAULT_PTY_SESSIONS as u32;
/// The most a user may choose: the terminal module's resource ceiling.
pub const MAX_TERMINAL_SESSIONS: u32 = crate::terminal::MAX_PTY_SESSIONS as u32;

/// Claude Code's settings sources, in the order `--setting-sources` names
/// them. Choosing all of them is the same as passing nothing.
pub const CLAUDE_SETTING_SOURCES: [&str; 3] = ["user", "project", "local"];

/// The setting-sources choice as it is stored and passed: known names only,
/// at least one, each once, in [`CLAUDE_SETTING_SOURCES`] order — and `None`
/// when every source is chosen, because that is the CLI's own default and
/// passing it would only pin today's list of sources.
pub fn canonical_setting_sources(chosen: &[String]) -> Result<Option<Vec<String>>, String> {
    if chosen.len() > 16 {
        return Err("Too many Claude settings sources".into());
    }
    if let Some(unknown) = chosen
        .iter()
        .find(|source| !CLAUDE_SETTING_SOURCES.contains(&source.as_str()))
    {
        return Err(format!(
            "{unknown:?} is not a Claude Code settings source (user, project or local)"
        ));
    }
    let kept: Vec<String> = CLAUDE_SETTING_SOURCES
        .iter()
        .filter(|source| chosen.iter().any(|c| c == *source))
        .map(|source| (*source).to_owned())
        .collect();
    match kept.len() {
        0 => Err("Claude Code needs at least one settings source".into()),
        n if n == CLAUDE_SETTING_SOURCES.len() => Ok(None),
        _ => Ok(Some(kept)),
    }
}

/// The store's own default for live attempts, used when nothing is stored.
pub const DEFAULT_LIVE_RUNS: u32 = dc_store::workbench::DEFAULT_ACTIVE_RUNS as u32;
/// The most the store accepts. Manvi's managed runner refuses past the same
/// number, so a limit this allows is never refused one layer down.
pub const MAX_LIVE_RUNS: u32 = dc_store::workbench::MAX_ACTIVE_RUNS_CEILING as u32;

impl AgentDefaults {
    /// The limit a launch passes to the store: the stored one, or the default.
    pub fn live_runs(&self) -> u32 {
        self.max_live_runs.unwrap_or(DEFAULT_LIVE_RUNS)
    }

    /// The terminal session limit to apply: the stored one, or the default.
    pub fn terminal_sessions(&self) -> u32 {
        self.max_terminal_sessions
            .unwrap_or(DEFAULT_TERMINAL_SESSIONS)
    }

    /// The value for `--setting-sources`, or `None` to pass nothing.
    pub fn claude_setting_sources_arg(&self) -> Option<String> {
        self.claude_setting_sources
            .as_ref()
            .map(|sources| sources.join(","))
    }

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
        if let Some(limit) = self.max_live_runs {
            if !(1..=MAX_LIVE_RUNS).contains(&limit) {
                return Err(format!(
                    "Agents running at once must be between 1 and {MAX_LIVE_RUNS}"
                ));
            }
        }
        if let Some(sources) = &self.claude_setting_sources {
            canonical_setting_sources(sources)?;
        }
        if let Some(limit) = self.max_terminal_sessions {
            if !(1..=MAX_TERMINAL_SESSIONS).contains(&limit) {
                return Err(format!(
                    "Terminal sessions open at once must be between 1 and {MAX_TERMINAL_SESSIONS}"
                ));
            }
        }
        if self.models.len() > MAX_MODEL_ENTRIES {
            return Err("Too many agent model settings".into());
        }
        for (launcher, choice) in &self.models {
            crate::workbench::terminal_command::validate_model_choice(launcher, choice)?;
        }
        Ok(())
    }

    /// The model choice a launch of `launcher` applies, if one is stored.
    pub fn model_for(&self, launcher: &str) -> Option<&ModelChoice> {
        self.models
            .get(launcher.trim())
            .filter(|choice| !choice.is_empty())
    }
}

/// `agent_defaults` as it is written to disk: the permission map, nothing else.
///
/// That is exactly the block v1.3.5 reads, and v1.3.5 reads it with
/// `deny_unknown_fields` — one key it does not know there costs that build
/// every stored permission default, and its next save writes them away. So
/// agent settings added since live in [`StoredAgentLaunch`], a block of its
/// own that an older build skips. Read without `deny_unknown_fields`, so a
/// newer build's addition here costs this one only that key.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredAgentPermissions {
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub permission: std::collections::BTreeMap<String, String>,
}

/// `agent_launch` on disk: the agent settings added after v1.3.5.
///
/// Read key by key (see its `Deserialize`), so a value this build cannot read
/// costs that one setting, and a key it does not know is skipped.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct StoredAgentLaunch {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_live_runs: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claude_setting_sources: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_terminal_sessions: Option<u32>,
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub models: std::collections::BTreeMap<String, ModelChoice>,
}

/// `agent_launch.models`, read launcher by launcher: an entry this build
/// cannot read costs that launcher's model setting and nothing else, and a
/// key inside an entry that a newer build added is skipped rather than
/// costing the fields this build does know.
fn lenient_models(
    value: Option<&serde_json::Value>,
) -> std::collections::BTreeMap<String, ModelChoice> {
    let mut models = std::collections::BTreeMap::new();
    let Some(value) = value else {
        return models;
    };
    let serde_json::Value::Object(entries) = value else {
        log::warn!(target: "tool_config", "ignoring agent_launch.models: not an object");
        return models;
    };
    for (launcher, entry) in entries {
        let serde_json::Value::Object(fields) = entry else {
            log::warn!(target: "tool_config", "ignoring agent_launch.models.{launcher}: not an object");
            continue;
        };
        let known: serde_json::Map<String, serde_json::Value> = fields
            .iter()
            .filter(|(key, _)| {
                let known = crate::workbench::terminal_command::MODEL_FIELDS.contains(&key.as_str());
                if !known {
                    log::warn!(target: "tool_config", "ignoring agent_launch.models.{launcher}.{key}: not a field this build knows");
                }
                known
            })
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        match serde_json::from_value::<ModelChoice>(serde_json::Value::Object(known)) {
            Ok(choice) => {
                models.insert(launcher.clone(), choice);
            }
            Err(error) => log::warn!(
                target: "tool_config",
                "ignoring unreadable agent_launch.models.{launcher}, keeping the other agent settings: {error}"
            ),
        }
    }
    models
}

impl StoredAgentLaunch {
    fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

impl<'de> Deserialize<'de> for StoredAgentLaunch {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = serde_json::Value::deserialize(deserializer)?;
        let serde_json::Value::Object(fields) = raw else {
            log::warn!(target: "tool_config", "ignoring agent_launch: not an object");
            return Ok(Self::default());
        };
        fn field<T: serde::de::DeserializeOwned>(
            fields: &serde_json::Map<String, serde_json::Value>,
            key: &str,
        ) -> Option<T> {
            let value = fields.get(key)?;
            match serde_json::from_value(value.clone()) {
                Ok(read) => Some(read),
                Err(error) => {
                    log::warn!(
                        target: "tool_config",
                        "ignoring unreadable agent_launch.{key}, keeping the other agent settings: {error}"
                    );
                    None
                }
            }
        }
        Ok(Self {
            max_live_runs: field(&fields, "max_live_runs"),
            claude_setting_sources: field(&fields, "claude_setting_sources"),
            max_terminal_sessions: field(&fields, "max_terminal_sessions"),
            models: lenient_models(fields.get("models")),
        })
    }
}

/// Reads `agent_defaults` without being able to fail.
///
/// Goes through `Value` so a block of the wrong shape is buffered and then
/// dropped, rather than failing the parse of the whole file.
fn lenient_agent_defaults<'de, D>(deserializer: D) -> Result<StoredAgentPermissions, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = serde_json::Value::deserialize(deserializer)?;
    Ok(serde_json::from_value(raw).unwrap_or_else(|error| {
        log::warn!(
            target: "tool_config",
            "ignoring unreadable agent_defaults block, using no stored default: {error}"
        );
        StoredAgentPermissions::default()
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
    pub agent_defaults: StoredAgentPermissions,
    /// Agent settings added after v1.3.5; see [`StoredAgentPermissions`] for
    /// why they are not in `agent_defaults`.
    #[serde(default, skip_serializing_if = "StoredAgentLaunch::is_empty")]
    pub agent_launch: StoredAgentLaunch,
    /// Top-level keys this build does not know, written back as they were
    /// read. Without this, saving any setting here would delete what a newer
    /// build stored, so moving between versions would quietly cost settings.
    #[serde(flatten)]
    pub unknown: serde_json::Map<String, serde_json::Value>,
}

impl Default for ToolConfig {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
            devmap: ToolPaths::default(),
            manvi: ToolPaths::default(),
            onboarding: OnboardingState::default(),
            session_alerts: SessionAlertSettings::default(),
            agent_defaults: StoredAgentPermissions::default(),
            agent_launch: StoredAgentLaunch::default(),
            unknown: serde_json::Map::new(),
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
    let mut defaults = AgentDefaults {
        permission: cfg.agent_defaults.permission,
        max_live_runs: cfg.agent_launch.max_live_runs,
        claude_setting_sources: cfg.agent_launch.claude_setting_sources,
        max_terminal_sessions: cfg.agent_launch.max_terminal_sessions,
        models: cfg.agent_launch.models,
    };
    if defaults.models.len() > MAX_MODEL_ENTRIES {
        log::warn!(target: "tool_config", "ignoring oversized agent model settings");
        defaults.models.clear();
    }
    defaults.models.retain(|launcher, choice| {
        if choice.is_empty() {
            return false;
        }
        match crate::workbench::terminal_command::validate_model_choice(launcher, choice) {
            Ok(()) => true,
            Err(error) => {
                // Costs that launcher's model setting, not a terminal tab:
                // the launch would refuse a choice it cannot apply in full.
                log::warn!(
                    target: "tool_config",
                    "ignoring stored model setting for {launcher}: {error}"
                );
                false
            }
        }
    });
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
    if let Some(limit) = defaults
        .max_live_runs
        .filter(|limit| !(1..=MAX_LIVE_RUNS).contains(limit))
    {
        // A hand-edited out-of-range limit costs the preference, not every
        // launch: the store would refuse it as input on each one.
        log::warn!(
            target: "tool_config",
            "ignoring stored agents-at-once limit {limit}: outside 1..={MAX_LIVE_RUNS}"
        );
        defaults.max_live_runs = None;
    }
    if let Some(limit) = defaults
        .max_terminal_sessions
        .filter(|limit| !(1..=MAX_TERMINAL_SESSIONS).contains(limit))
    {
        log::warn!(
            target: "tool_config",
            "ignoring stored terminal session limit {limit}: outside 1..={MAX_TERMINAL_SESSIONS}"
        );
        defaults.max_terminal_sessions = None;
    }
    if let Some(sources) = defaults.claude_setting_sources.take() {
        defaults.claude_setting_sources =
            canonical_setting_sources(&sources).unwrap_or_else(|error| {
                log::warn!(
                    target: "tool_config",
                    "ignoring stored Claude settings sources: {error}"
                );
                None
            });
    }
    defaults
}

pub fn set_agent_defaults(next: AgentDefaults) -> Result<(), String> {
    next.validate()?;
    let sources = match &next.claude_setting_sources {
        Some(sources) => canonical_setting_sources(sources)?,
        None => None,
    };
    let mut cfg = load()?;
    cfg.agent_defaults = StoredAgentPermissions {
        permission: next.permission,
    };
    cfg.agent_launch = StoredAgentLaunch {
        max_live_runs: next.max_live_runs,
        claude_setting_sources: sources,
        max_terminal_sessions: next.max_terminal_sessions,
        models: next
            .models
            .into_iter()
            .filter(|(_, choice)| !choice.is_empty())
            .collect(),
    };
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

    /// How many agents run at once is the user's: stored as chosen within the
    /// store's bounds, refused outside them at save, and degraded per key —
    /// never by discarding the permission defaults beside it — when a
    /// hand-edited file carries a value no launch could pass.
    #[test]
    fn the_agents_at_once_limit_is_bounded_by_the_store_and_degrades_alone() {
        assert_eq!(DEFAULT_LIVE_RUNS, 8);
        assert_eq!(MAX_LIVE_RUNS, 64);
        with_temp_config(|path| {
            assert_eq!(agent_defaults().live_runs(), DEFAULT_LIVE_RUNS);
            // What a launch passes: the host reads the stored limit at each
            // preparation, so a save applies to the next one.
            let host = crate::workbench::WorkbenchState::default();
            assert_eq!(host.live_runs(), DEFAULT_LIVE_RUNS);
            for limit in [1, 2, 9, MAX_LIVE_RUNS] {
                let defaults = AgentDefaults {
                    max_live_runs: Some(limit),
                    ..AgentDefaults::default()
                };
                set_agent_defaults(defaults.clone()).unwrap();
                assert_eq!(agent_defaults(), defaults);
                assert_eq!(agent_defaults().live_runs(), limit);
                assert_eq!(host.live_runs(), limit);
            }
            for limit in [0, MAX_LIVE_RUNS + 1, u32::MAX] {
                let refused = set_agent_defaults(AgentDefaults {
                    max_live_runs: Some(limit),
                    ..AgentDefaults::default()
                });
                assert!(refused.is_err(), "{limit} was stored");
            }
            assert_eq!(
                agent_defaults().live_runs(),
                MAX_LIVE_RUNS,
                "a refused save changed the limit"
            );

            for raw in [
                r#""max_live_runs":0"#,
                r#""max_live_runs":65"#,
                r#""max_live_runs":"12""#,
                r#""max_live_runs":-3"#,
                r#""max_live_runs":1.5"#,
                r#""max_live_runs":[8]"#,
            ] {
                write_raw(
                    path,
                    &format!(
                        r#"{{"version":1,"agent_defaults":{{"permission":{{"claude":"edit"}}}},"agent_launch":{{{raw}}}}}"#
                    ),
                );
                let defaults = agent_defaults();
                assert_eq!(defaults.max_live_runs, None, "{raw} survived");
                assert_eq!(defaults.live_runs(), DEFAULT_LIVE_RUNS, "{raw}");
                assert_eq!(
                    defaults.permission.get("claude").map(String::as_str),
                    Some("edit"),
                    "{raw} took the permission defaults with it"
                );
            }
        });
    }

    /// What v1.3.5 reads `agent_defaults` as: its exact struct, strict about
    /// unknown keys. Copied from `git show v1.3.5:src-tauri/src/tool_config.rs`.
    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    #[allow(dead_code)]
    struct ShippedAgentDefaults {
        #[serde(default)]
        permission: std::collections::BTreeMap<String, String>,
    }

    /// The downgrade defect: the agents-at-once limit was written inside
    /// `agent_defaults`, which v1.3.5 reads with `deny_unknown_fields`, so
    /// opening the file in that build discarded every stored permission
    /// default and its next save wrote them away. Settings added since must
    /// land where an older build does not look.
    #[test]
    fn every_agent_setting_is_stored_where_the_last_release_still_reads_its_own() {
        with_temp_config(|path| {
            let mut defaults = AgentDefaults {
                max_live_runs: Some(24),
                claude_setting_sources: Some(vec!["user".into()]),
                ..AgentDefaults::default()
            };
            defaults.permission.insert("claude".into(), "edit".into());
            set_agent_defaults(defaults.clone()).unwrap();
            let written: serde_json::Value =
                serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
            let block = &written["agent_defaults"];
            assert_eq!(
                block.as_object().unwrap().keys().collect::<Vec<_>>(),
                ["permission"],
                "{written}"
            );
            let shipped: ShippedAgentDefaults = serde_json::from_value(block.clone())
                .expect("the last release can no longer read its agent defaults");
            assert_eq!(shipped.permission.get("claude").unwrap(), "edit");
            assert_eq!(written["agent_launch"]["max_live_runs"], 24);
            assert_eq!(written["agent_launch"]["claude_setting_sources"][0], "user");
            invalidate_cache();
            assert_eq!(agent_defaults(), defaults);

            // Nothing chosen beyond the defaults writes no new block at all.
            set_agent_defaults(AgentDefaults::default()).unwrap();
            let written: serde_json::Value =
                serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
            assert!(written.get("agent_launch").is_none(), "{written}");
        });
    }

    /// The same class one version on: a key a newer build stores must survive
    /// this build saving any setting, or moving between versions quietly costs
    /// settings again.
    #[test]
    fn keys_a_newer_build_stored_survive_a_save_here() {
        with_temp_config(|path| {
            write_raw(
                path,
                r#"{"version":1,"agent_defaults":{"permission":{"claude":"edit"},"from_later":1},
                    "agent_launch":{"max_live_runs":12,"from_later":true},
                    "from_a_newer_build":{"kept":[1,2,3]}}"#,
            );
            let read = agent_defaults();
            assert_eq!(read.permission.get("claude").unwrap(), "edit");
            assert_eq!(read.live_runs(), 12);
            set_onboarding(OnboardingState {
                dismissed: true,
                ..OnboardingState::default()
            })
            .unwrap();
            let written: serde_json::Value =
                serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
            assert_eq!(written["from_a_newer_build"]["kept"][2], 3, "{written}");
            assert_eq!(written["agent_launch"]["max_live_runs"], 12);
            assert_eq!(written["onboarding"]["dismissed"], true);
        });
    }

    /// Which Claude settings files an agent loads is the user's to narrow:
    /// stored canonically, refused when it names nothing or something Claude
    /// does not know, all-of-them stored as "pass nothing", and a hand-edited
    /// bad value costs only itself.
    #[test]
    fn claude_setting_sources_are_canonical_and_degrade_alone() {
        let owned = |items: &[&str]| items.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        assert_eq!(
            canonical_setting_sources(&owned(&["local", "user", "user"])).unwrap(),
            Some(owned(&["user", "local"]))
        );
        assert_eq!(
            canonical_setting_sources(&owned(&["project", "local", "user"])).unwrap(),
            None
        );
        for refused in [
            owned(&[]),
            owned(&["policy"]),
            owned(&["user", "User"]),
            owned(&["user,project"]),
            owned(&[""]),
            vec!["user".to_owned(); 17],
        ] {
            assert!(canonical_setting_sources(&refused).is_err(), "{refused:?}");
        }
        with_temp_config(|path| {
            for (chosen, stored, arg) in [
                (owned(&["user"]), Some(owned(&["user"])), Some("user")),
                (
                    owned(&["local", "user"]),
                    Some(owned(&["user", "local"])),
                    Some("user,local"),
                ),
                (owned(&["user", "project", "local"]), None, None),
            ] {
                set_agent_defaults(AgentDefaults {
                    claude_setting_sources: Some(chosen.clone()),
                    ..AgentDefaults::default()
                })
                .unwrap();
                let read = agent_defaults();
                assert_eq!(read.claude_setting_sources, stored, "{chosen:?}");
                assert_eq!(read.claude_setting_sources_arg().as_deref(), arg);
            }
            for empty in [owned(&[]), owned(&["nope"])] {
                assert!(set_agent_defaults(AgentDefaults {
                    claude_setting_sources: Some(empty),
                    ..AgentDefaults::default()
                })
                .is_err());
            }
            for raw in [
                r#""claude_setting_sources":[]"#,
                r#""claude_setting_sources":["policy"]"#,
                r#""claude_setting_sources":"user""#,
                r#""claude_setting_sources":[1]"#,
            ] {
                write_raw(
                    path,
                    &format!(
                        r#"{{"version":1,"agent_defaults":{{"permission":{{"claude":"edit"}}}},"agent_launch":{{"max_live_runs":9,{raw}}}}}"#
                    ),
                );
                let read = agent_defaults();
                assert_eq!(read.claude_setting_sources, None, "{raw} survived");
                assert_eq!(read.live_runs(), 9, "{raw} took the limit with it");
                assert_eq!(read.permission.get("claude").unwrap(), "edit", "{raw}");
            }
        });
    }

    /// How many terminal sessions may be open is the user's: stored where the
    /// last release does not look, refused outside the terminal module's
    /// bounds at save, and degraded alone when a hand-edited value is bad.
    #[test]
    fn the_terminal_session_limit_is_bounded_and_degrades_alone() {
        assert_eq!(DEFAULT_TERMINAL_SESSIONS, 32);
        assert_eq!(MAX_TERMINAL_SESSIONS, 128);
        with_temp_config(|path| {
            assert_eq!(
                agent_defaults().terminal_sessions(),
                DEFAULT_TERMINAL_SESSIONS
            );
            for limit in [1, 33, MAX_TERMINAL_SESSIONS] {
                let defaults = AgentDefaults {
                    max_terminal_sessions: Some(limit),
                    ..AgentDefaults::default()
                };
                set_agent_defaults(defaults.clone()).unwrap();
                assert_eq!(agent_defaults(), defaults);
                assert_eq!(agent_defaults().terminal_sessions(), limit);
            }
            let written: serde_json::Value =
                serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
            assert_eq!(
                written["agent_launch"]["max_terminal_sessions"],
                MAX_TERMINAL_SESSIONS
            );
            assert!(written["agent_defaults"]
                .get("max_terminal_sessions")
                .is_none());
            for limit in [0, MAX_TERMINAL_SESSIONS + 1, u32::MAX] {
                assert!(
                    set_agent_defaults(AgentDefaults {
                        max_terminal_sessions: Some(limit),
                        ..AgentDefaults::default()
                    })
                    .is_err(),
                    "{limit} was stored"
                );
            }
            // The one path from the setting to the live registry, taken at
            // startup and after every save: a clone the app handed to a lane
            // sees it too.
            let terminals = crate::terminal::TerminalSessions::default();
            let lane = terminals.clone();
            set_agent_defaults(AgentDefaults {
                max_terminal_sessions: Some(48),
                ..AgentDefaults::default()
            })
            .unwrap();
            assert_eq!(terminals.apply_stored_limit(), 48);
            assert_eq!(lane.session_limit(), 48);
            set_agent_defaults(AgentDefaults::default()).unwrap();
            assert_eq!(
                terminals.apply_stored_limit(),
                DEFAULT_TERMINAL_SESSIONS as usize
            );
            write_raw(path, "{ not json");
            assert_eq!(
                terminals.apply_stored_limit(),
                DEFAULT_TERMINAL_SESSIONS as usize,
                "an unreadable file must mean the default, not the last limit"
            );
            for raw in [
                r#""max_terminal_sessions":0"#,
                r#""max_terminal_sessions":129"#,
                r#""max_terminal_sessions":"40""#,
                r#""max_terminal_sessions":-1"#,
                r#""max_terminal_sessions":2.5"#,
            ] {
                write_raw(
                    path,
                    &format!(
                        r#"{{"version":1,"agent_defaults":{{"permission":{{"claude":"edit"}}}},"agent_launch":{{"max_live_runs":9,{raw}}}}}"#
                    ),
                );
                let read = agent_defaults();
                assert_eq!(read.max_terminal_sessions, None, "{raw} survived");
                assert_eq!(read.terminal_sessions(), DEFAULT_TERMINAL_SESSIONS);
                assert_eq!(read.live_runs(), 9, "{raw} took the agent limit with it");
                assert_eq!(read.permission.get("claude").unwrap(), "edit", "{raw}");
            }
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

#[cfg(test)]
mod model_setting_tests {
    use super::*;

    fn with_temp_config(f: impl FnOnce(&Path)) {
        let serial = crate::tool_config::lock_config_env();
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("tools.json");
        let _env = crate::test_support::env::bind_env(&serial)
            .set(TOOL_CONFIG_ENV, &path)
            .invalidating(invalidate_cache);
        f(&path);
    }

    fn write_raw(path: &Path, json: &str) {
        fs::write(path, json).unwrap();
        invalidate_cache();
    }

    fn read_json(path: &Path) -> serde_json::Value {
        serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
    }

    fn choice(model: &str) -> ModelChoice {
        ModelChoice {
            model: Some(model.into()),
            ..ModelChoice::default()
        }
    }

    #[test]
    fn model_settings_round_trip_where_older_builds_skip_them() {
        with_temp_config(|path| {
            let mut defaults = AgentDefaults::default();
            defaults.permission.insert("claude".into(), "edit".into());
            defaults.models.insert(
                "claude".into(),
                ModelChoice {
                    model: Some("opus[1m]".into()),
                    effort: Some("xhigh".into()),
                    fallback: Some(vec!["sonnet".into(), "haiku".into()]),
                    advisor: Some("fable".into()),
                },
            );
            defaults.models.insert(
                "agy".into(),
                ModelChoice {
                    model: Some("gemini-3.8-flash-high".into()),
                    effort: Some("low".into()),
                    ..ModelChoice::default()
                },
            );
            set_agent_defaults(defaults.clone()).unwrap();
            assert_eq!(agent_defaults(), defaults);
            let written = read_json(path);
            // In `agent_launch`, never in `agent_defaults`: v1.3.5 reads that
            // block with `deny_unknown_fields`, and one unknown key there
            // costs it every stored permission default.
            assert!(
                written["agent_defaults"].get("models").is_none(),
                "{written}"
            );
            assert_eq!(
                written["agent_launch"]["models"]["claude"]["advisor"],
                "fable"
            );
            assert_eq!(
                written["agent_launch"]["models"]["claude"]["fallback"],
                serde_json::json!(["sonnet", "haiku"])
            );
            assert_eq!(
                agent_defaults().model_for("agy").unwrap().model.as_deref(),
                Some("gemini-3.8-flash-high")
            );
            assert!(agent_defaults().model_for("codex").is_none());
            // Clearing them removes the key rather than storing an empty map.
            set_agent_defaults(AgentDefaults {
                permission: defaults.permission.clone(),
                ..AgentDefaults::default()
            })
            .unwrap();
            assert!(read_json(path)["agent_launch"].get("models").is_none());
        });
    }

    /// An empty choice is "the CLI's own model", which is no entry at all.
    #[test]
    fn an_empty_choice_is_stored_as_absence() {
        with_temp_config(|path| {
            let mut defaults = AgentDefaults::default();
            defaults
                .models
                .insert("claude".into(), ModelChoice::default());
            defaults.models.insert("codex".into(), choice("gpt-6"));
            set_agent_defaults(defaults).unwrap();
            let written = read_json(path);
            assert!(written["agent_launch"]["models"].get("claude").is_none());
            let read = agent_defaults();
            assert!(read.model_for("claude").is_none());
            assert_eq!(read.models.len(), 1);
        });
    }

    #[test]
    fn a_choice_a_launch_could_not_apply_in_full_is_refused_at_save() {
        with_temp_config(|_| {
            for (launcher, bad) in [
                (
                    "codex",
                    ModelChoice {
                        effort: Some("high".into()),
                        ..ModelChoice::default()
                    },
                ),
                (
                    "agy",
                    ModelChoice {
                        advisor: Some("opus".into()),
                        ..ModelChoice::default()
                    },
                ),
                ("manvi", choice("opus")),
                ("shell", choice("opus")),
                ("claude", choice("-x")),
                ("claude", choice("a,b")),
                ("claude", choice("")),
            ] {
                let mut defaults = AgentDefaults::default();
                defaults.models.insert(launcher.into(), bad.clone());
                assert!(
                    set_agent_defaults(defaults).is_err(),
                    "{launcher}: {bad:?} was saved"
                );
            }
            let mut many = AgentDefaults::default();
            for i in 0..=MAX_MODEL_ENTRIES {
                many.models.insert(format!("launcher{i}"), choice("opus"));
            }
            assert!(set_agent_defaults(many).is_err());
        });
    }

    /// A hand-edited or newer-build `tools.json` costs the entry it broke and
    /// nothing else: the other launchers' models, the permission defaults and
    /// the limits all survive.
    #[test]
    fn a_broken_model_entry_degrades_alone() {
        with_temp_config(|path| {
            for (models, survivors) in [
                // Not an object at all.
                (r#""opus""#, vec![]),
                (r#"[1,2]"#, vec![]),
                // One launcher unreadable, one fine.
                (
                    r#"{"claude":"opus","codex":{"model":"gpt-6"}}"#,
                    vec!["codex"],
                ),
                (
                    r#"{"claude":{"model":7},"codex":{"model":"gpt-6"}}"#,
                    vec!["codex"],
                ),
                // Readable but not applicable: the field the CLI lacks.
                (
                    r#"{"codex":{"model":"gpt-6","effort":"high"},"grok":{"model":"grok-5"}}"#,
                    vec!["grok"],
                ),
                // Readable but malformed values.
                (
                    r#"{"claude":{"model":"--yolo"},"grok":{"model":"grok-5"}}"#,
                    vec!["grok"],
                ),
                (
                    r#"{"claude":{"fallback":[]},"grok":{"model":"grok-5"}}"#,
                    vec!["grok"],
                ),
                (
                    r#"{"claude":{"effort":"ultra"},"grok":{"model":"grok-5"}}"#,
                    vec!["grok"],
                ),
                // Not a launcher this build has.
                (
                    r#"{"gemini":{"model":"x"},"grok":{"model":"grok-5"}}"#,
                    vec!["grok"],
                ),
                // Empty entries are not stored choices.
                (r#"{"claude":{},"grok":{"model":"grok-5"}}"#, vec!["grok"]),
            ] {
                write_raw(
                    path,
                    &format!(
                        r#"{{"version":1,"devmap":{{"binary":"/opt/devmap"}},"agent_defaults":{{"permission":{{"claude":"edit"}}}},"agent_launch":{{"max_live_runs":9,"models":{models}}}}}"#
                    ),
                );
                let read = agent_defaults();
                let mut kept: Vec<&str> = read.models.keys().map(String::as_str).collect();
                kept.sort_unstable();
                assert_eq!(kept, survivors, "{models}");
                assert_eq!(read.live_runs(), 9, "{models} took the limit with it");
                assert_eq!(read.permission.get("claude").unwrap(), "edit", "{models}");
                assert_eq!(
                    load().unwrap().devmap.binary.as_deref(),
                    Some("/opt/devmap"),
                    "{models} cost the saved devmap path"
                );
            }
        });
    }

    /// A key a newer build added inside an entry is skipped; the fields this
    /// build knows in that same entry are kept.
    #[test]
    fn a_field_from_a_newer_build_costs_only_that_field() {
        with_temp_config(|path| {
            write_raw(
                path,
                r#"{"version":1,"agent_launch":{"models":{"claude":{"model":"opus","thinking":"on","effort":"high"}}}}"#,
            );
            let read = agent_defaults();
            let claude = read
                .model_for("claude")
                .expect("the whole entry was dropped");
            assert_eq!(claude.model.as_deref(), Some("opus"));
            assert_eq!(claude.effort.as_deref(), Some("high"));
        });
    }

    #[test]
    fn an_oversized_model_map_is_dropped_rather_than_rendered() {
        with_temp_config(|path| {
            let entries: Vec<String> = (0..=MAX_MODEL_ENTRIES)
                .map(|i| format!(r#""l{i}":{{"model":"m"}}"#))
                .collect();
            write_raw(
                path,
                &format!(
                    r#"{{"version":1,"agent_launch":{{"max_live_runs":5,"models":{{{}}}}}}}"#,
                    entries.join(",")
                ),
            );
            let read = agent_defaults();
            assert!(read.models.is_empty());
            assert_eq!(read.live_runs(), 5);
        });
    }
}
