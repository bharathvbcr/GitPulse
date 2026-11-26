//! Periodic Manvi system steward (`harness::steward`).
//!
//! When Manvi is configured (`manvi use` cloud or local, or Manvi's built-in
//! default cloud mode paired with a valid `~/.anthropic-accounts` account or
//! GitPulse Keychain Claude key) and enabled in
//! [`crate::tool_config::ManviStewardSettings`], a single supervised worker in
//! the GitPulse app process checks host load and disk headroom every few
//! minutes (and wakes early when the governor enters [`Tier::Severe`]):
//!
//! 1. **Synthesize guidance via Manvi (`work.assist.complete`)** on whatever
//!    model Manvi is configured to use (Claude API or loopback local server),
//!    caching the last verified `manvi use` selection across severe-load spawn
//!    deferrals and skipping a *local* model call under severe
//!    memory/swap/compressor pressure so loading local weights never worsens a
//!    memory crisis.
//! 2. **Message active agents** across trusted repositories (deduplicated per
//!    canonical Git `common_dir`) via `engine::agent_bus::post_peer_message`
//!    and per-session mailboxes (`agent_mailbox::Mailbox` with
//!    `EnvelopeProvenance::SystemSteward`), delivering at `0ms` on the next
//!    tool boundary (`peer-inbox`) without stuffing or waking idle sessions.
//! 3. **Slow runaway agent trees at critical pressure** by publishing a
//!    fail-safe, short-TTL lease (`<config>/steward/active.json`, verified by
//!    writer `(pid, start_us)` and `expires_at_ms`) that `hooks::load_gate`
//!    enforces even when agents bypass advisory rules, and by placing active
//!    agent child/worker processes into OS background scheduler/I/O throttle
//!    (through `sysload::relief::steward_throttle`, which is the one owner of
//!    every priority change: the pressure relief's own slowing is never undone
//!    from here, and this one is never undone by the relief), revoking
//!    immediately when critical pressure clears, a repository opts out, or
//!    GitPulse exits. The relief (`sysload::relief`) slows and freezes on the
//!    sampler's tick with no Manvi, so this slowdown adds the lease and the
//!    directive text, not the only slowing.
//! 4. **Reclaim leftover Cargo targets, agent scratchpads, and shared caches
//!    when storage or resources are critical** by building a pre-verified idle
//!    candidate inventory (including idle linked-worktree Cargo targets and,
//!    under critical disk, recent idle scratch builds), having Manvi cloud
//!    evaluate and select which candidate IDs to reclaim when cloud is
//!    configured, and executing exclusively through `storage::hygiene::{prepare,
//!    execute}` and `storage::hygiene::agent::remove_judged` under
//!    `engine::process::with_disk_relief`, gated through `harness::guard_command`
//!    and `harness::guard_file`.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::{self, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::agent_mailbox::{EnvelopeProvenance, Mailbox};
use crate::ai::ManviError;
use crate::engine::agent_bus::{self, MessageOrigin};
use crate::engine::git_cli::{find_git_root, resolve_git_common_dir};
use crate::harness::manvi_use::{self, ManviStatus};
use crate::harness::sidecar::ModelSelection;
use crate::ipc_error::IpcErrorKind;
use crate::sysload::procscan::AgentThrottleCandidate;
use crate::sysload::{Binding, HostLoadPulse, HostLoadStatus, PressureInput, SensorState, Tier};
use crate::tool_config::{
    EffectiveStewardPolicy, ManviStewardSettings, STEWARD_MAX_INTERVAL_SECS,
    STEWARD_MIN_INTERVAL_SECS,
};
use crate::watcher::WatcherState;
use crate::workbench::WorkbenchState;

/// Optional environment override (`0`/`false`/`off` or `1`/`true`/`on`).
pub const STEWARD_ENV: &str = "GITPULSE_MANVI_STEWARD";
/// Subdirectory inside the per-user GitPulse config folder.
pub const STEWARD_SUBDIR: &str = "steward";
/// Active critical-slowdown lease read by `hooks::load_gate`.
pub const ACTIVE_FILE: &str = "active.json";
/// Bounded run history retained in memory for Settings & Health inspection.
pub const MAX_HISTORY: usize = 20;
/// Maximum output tokens requested from `work.assist.complete` for one directive.
const MAX_OUTPUT_TOKENS: i64 = 320;
/// Maximum output tokens requested from Manvi cloud when selecting cleanup candidates.
const MAX_CLEAN_SELECT_TOKENS: i64 = 1_024;
/// Maximum pre-verified idle candidates sent to Manvi cloud in one evaluation pass.
const MAX_CLOUD_CANDIDATES: usize = 32;
/// Maximum deletions executed in one critical hygiene pass.
const MAX_EXECUTED_CLEAN_ITEMS: usize = 24;
/// Maximum trusted repositories scanned during one critical cleanup pass.
const MAX_CLEANUP_DISCOVERED_REPOS: usize = 64;
/// Wall-clock budget for Stage 1 candidate discovery across trusted repositories.
const STAGE1_INVENTORY_BUDGET: Duration = Duration::from_secs(90);
/// Maximum sanitized directive length sent to agent buses/mailboxes.
const MAX_DIRECTIVE_CHARS: usize = 1_200;
/// Cooperative hook pacing pause (ms) for general shell calls under critical load.
pub(crate) const CRITICAL_PACE_MS: u64 = 1_000;
/// Minimum gap between automatic critical-storage cleanup sweeps.
const AUTO_CLEAN_COOLDOWN_MS: u64 = 30 * 60 * 1_000;
/// Minimum shared-cache size before critical cleanup asks its owning tool to prune.
const MIN_SHARED_CACHE_CLEAN_BYTES: u64 = 512 * 1024 * 1024;
/// Byte cap on one steward-triggered agent scratch cleanup pass.
const MAX_STEWARD_AGENT_SCRATCH_BYTES: u64 = 64 * 1024 * 1024 * 1024;
/// Debounce between early `critical_wake` notifications from `HostLoadPulse`.
const CRITICAL_WAKE_DEBOUNCE: Duration = Duration::from_secs(15);
/// Background loop sleep quantum (re-reads `interval_secs` dynamically).
const LOOP_POLL: Duration = Duration::from_secs(15);
/// Maximum size accepted when reading `steward/active.json`.
const MAX_ACTIVE_RECORD_BYTES: u64 = 8 * 1024;

static SEQUENCE: AtomicU64 = AtomicU64::new(0);
static SERVICE: OnceLock<Arc<StewardRuntime>> = OnceLock::new();

const STEWARD_SYSTEM_PROMPT: &str = "\
You are GitPulse's Manvi system steward monitoring a developer workstation running coding agents. \
Given the measured host telemetry and actions GitPulse just took, write a concise, direct 2 to 4 \
sentence operational directive for the active coding agents on this machine. \
State the exact bottleneck (memory/swap/compressor, CPU/thermal, or low disk), instruct agents \
to cap build/test concurrency (e.g. CARGO_BUILD_JOBS=2 --test-threads=2, GOMAXPROCS=4 -p 2) or \
pause heavy builds until pressure clears, and when disk is low instruct them to remove throwaway \
target directories under .gitpulse/scratch. Plain text only; no markdown headings, no code fences, \
and never invent numbers not in the telemetry.";

const STEWARD_CLEAN_SYSTEM_PROMPT: &str = "\
You are GitPulse's Manvi cloud storage steward evaluating pre-verified idle build and cache \
candidates on a developer workstation under critical storage or resource pressure. \
Every candidate in `candidates` has already passed GitPulse's producer-marker, git-ignore, \
untracked, symlink, open-file, task-lease, and process-table checks. \
Select which candidate `id`s to reclaim now: \
1. When `disk_critical` is true, reclaim enough idle space to relieve the disk emergency: \
prioritize finished-session scratchpads (`session_scratchpad`) and orphaned repository/Claude \
scratch Cargo targets (`repo_agent_scratch`) first — including recent idle scratch builds \
(`recent_below_retention: true`), preferring checkouts with `checkout_has_live_agent: false` \
before checkouts with a live agent — then large aged Cargo/build targets (`cargo_target`, \
`worktree_cargo_target`, `repo_build_cache`, checking `private_bytes` so tiny CoW clones are \
not unnecessarily deleted), and finally `shared_cache` entries (`go`, `uv`, `npm`, `pnpm`) when \
more space is needed after weighing rebuild/download cost in `note`. \
2. When `disk_critical` is false (host memory/swap/CPU is critical while disk is above the \
severe floor), be conservative: select clearly leftover scratchpads and large aged unused Cargo \
targets when freeing disk helps swap or tight disk headroom, or return an empty `selected_ids` \
array (`[]`) when free disk is ample and disk cleanup is unnecessary. \
Respond ONLY with a single JSON object and no markdown code fences: \
{\"selected_ids\":[\"c1\",\"c2\"],\"rationale\":\"one concise sentence\"}";

/// One hygiene target inspected or cleaned during a critical storage pass.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StewardCleanItem {
    pub repo_path: String,
    pub target: String,
    /// `"reclaimed"` | `"partially_reclaimed"` | `"skipped"` | `"failed"`
    pub status: String,
    pub measured_bytes: u64,
    pub bytes_after: Option<u64>,
    pub detail: String,
}

/// Summary of one periodic or manual Manvi steward check.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StewardTickRecord {
    pub id: String,
    /// `"scheduled"` | `"critical_wake"` | `"manual"`
    pub trigger: String,
    pub ran_at_ms: u64,
    /// `"acted"` | `"healthy"` | `"disabled"` | `"manvi_unconfigured"` | `"manvi_deferred"` | `"sensor_unavailable"` | `"failed"`
    pub outcome: String,
    /// `"clear"` | `"elevated"` | `"severe"` | `"unknown"`
    pub tier: String,
    pub binding: String,
    /// `"cloud"` | `"local"` | `"unconfigured"` | `"unavailable"`
    pub manvi_mode: String,
    pub manvi_model: String,
    /// True only when Manvi's `work.assist.complete` produced the directive;
    /// false when deterministic fallback text was used.
    pub model_generated: bool,
    pub summary: String,
    pub directive: String,
    pub messaged_sessions: u32,
    pub messaged_repositories: u32,
    pub skipped_untrusted_repositories: u32,
    pub throttled_processes: u32,
    pub restored_processes: u32,
    pub reclaimed_bytes: u64,
    pub cleaned_items: Vec<StewardCleanItem>,
    pub warnings: Vec<String>,
}

/// Complete Manvi steward state returned to Settings and Health over IPC.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManviStewardStatus {
    pub settings: ManviStewardSettings,
    pub effective: EffectiveStewardPolicy,
    pub repo_common_dir: Option<String>,
    /// What `GITPULSE_MANVI_STEWARD` says, for a person to read.
    pub env_override: Option<String>,
    /// What `GITPULSE_MANVI_STEWARD` forces: `Some(true)` on, `Some(false)`
    /// off, `None` when it is unset or unreadable. Code reads this, never the
    /// note's text.
    pub env_forced: Option<bool>,
    /// `"ready"` | `"not_chosen"` | `"not_installed"` | `"needs_update"` | `"failed"` | `"deferred"` | `"unsupported"`
    /// | `"rejected"` (Anthropic refused the key on the last model call)
    pub manvi_state: String,
    pub manvi_mode: String,
    pub manvi_model: String,
    pub manvi_detail: String,
    pub running: bool,
    pub critical_throttle_active: bool,
    pub throttled_count: u32,
    pub last_check_ms: Option<u64>,
    pub next_check_ms: Option<u64>,
    pub history: Vec<StewardTickRecord>,
}

/// Cross-process critical slowdown lease written to `<config>/steward/active.json`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ActiveThrottleRecord {
    pub version: u32,
    pub writer_pid: u32,
    pub writer_start_us: u64,
    pub issued_at_ms: u64,
    pub expires_at_ms: u64,
    pub tier: String,
    pub binding: String,
    pub reason: String,
    pub pace_ms: u64,
    /// Canonical common Git directories of the repositories this slowdown
    /// covers: trusted ones whose policy slows agents. The steward decides
    /// trust in-process, where session-only grants are visible, so a hook
    /// never reads the trust store. A command anywhere else is not paced.
    #[serde(default)]
    pub repositories: Vec<String>,
}

/// Version 2 added `repositories`. A hook built before it would ignore that
/// list and pace every repository, so it is handed a version it refuses.
pub(crate) const LEASE_VERSION: u32 = 2;

/// Most repositories one lease names; `MAX_ACTIVE_RECORD_BYTES` bounds the
/// file as a whole.
const MAX_LEASE_REPOSITORIES: usize = 64;

#[derive(Debug, Clone)]
pub(crate) enum ManviProbe {
    Answered(Box<ManviStatus>),
    Deferred(String),
    /// `manvi use` could not be asked at all (spawn failure, timeout, bad
    /// output). Unlike an answer of `not_chosen`, this says nothing about
    /// whether Manvi is configured, so it must not stand the steward down.
    Unreadable(String),
}

#[derive(Debug, Clone, Default)]
pub(crate) struct CleanPassOutcome {
    pub reclaimed_bytes: u64,
    pub items: Vec<StewardCleanItem>,
    pub warnings: Vec<String>,
    /// True when the cleaner lock was busy or checks were shed under load,
    /// so an automatic pass that reclaimed 0 bytes must not lock out retries
    /// for 30 minutes.
    pub deferred_or_busy: bool,
}

#[derive(Default)]
struct InnerState {
    history: Vec<StewardTickRecord>,
    last_check_ms: Option<u64>,
    last_clean_ms: Option<u64>,
    /// `(common_dir_or_root) -> (last_messaged_ms, last_tier_rank, last_binding)`
    last_messaged: HashMap<String, (u64, u8, String)>,
    /// Processes whose OS background throttle WE turned on, or that inherited
    /// it from one we did (`pid -> (start_us, repo_common_dir)`).
    throttled: BTreeMap<i32, (u64, Option<String>)>,
    /// Candidates already at background priority on the first tick of the
    /// current critical episode: someone else put them there, so they are
    /// never restored by us. Cleared when the episode ends.
    background_before_episode: BTreeSet<(i32, u64)>,
    critical_throttle_active: bool,
    /// Monotonic counter bumped whenever global or per-repository settings are saved.
    settings_epoch: u64,
    /// Last non-deferred `manvi use --json` answer so severe-load spawn deferrals
    /// never make the steward forget Manvi's configured model.
    cached_manvi: Option<ManviStatus>,
    /// True once GitPulse's Keychain Claude key was validated (`sk-ant-...`) so
    /// subsequent background checks only verify unencrypted item presence
    /// (`find-generic-password` without `-w`) instead of decrypting the secret
    /// or prompting on a locked login Keychain every few minutes.
    verified_keychain_key: bool,
    /// The cloud model whose call Manvi last answered `E_PROVIDER_AUTH`: the
    /// key passes its format check but Anthropic refused it. While the chosen
    /// model is that cloud model, the status reads `rejected` instead of ready.
    /// Any GitPulse call through Manvi's Anthropic route records here (the
    /// steward's directive, its cleanup selection, and every AI feature). A
    /// later Anthropic call that succeeds, a saved or deleted Keychain key, or
    /// a new `manvi use` choice clears it.
    credential_rejected: Option<String>,
    wake_requested: Option<&'static str>,
    last_critical_wake: Option<Instant>,
}

/// Publishes a status to the app (a Tauri event), set by [`install`].
type StatusEmitter = Arc<dyn Fn(&ManviStewardStatus) + Send + Sync>;

pub(crate) struct StewardRuntime {
    inner: Mutex<InnerState>,
    wake_cv: Condvar,
    running_tick: AtomicBool,
    closed: AtomicBool,
    workbench: Mutex<Option<WorkbenchState>>,
    watcher: Mutex<Option<WatcherState>>,
    emit: Mutex<Option<StatusEmitter>>,
}

impl Default for StewardRuntime {
    fn default() -> Self {
        Self {
            inner: Mutex::new(InnerState::default()),
            wake_cv: Condvar::new(),
            running_tick: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            workbench: Mutex::new(None),
            watcher: Mutex::new(None),
            emit: Mutex::new(None),
        }
    }
}

fn runtime() -> &'static Arc<StewardRuntime> {
    SERVICE.get_or_init(|| Arc::new(StewardRuntime::default()))
}

fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

fn tier_rank(tier: Tier) -> u8 {
    match tier {
        Tier::Clear => 0,
        Tier::Elevated => 1,
        Tier::Severe => 2,
    }
}

fn tier_name(tier: Tier) -> &'static str {
    match tier {
        Tier::Clear => "clear",
        Tier::Elevated => "elevated",
        Tier::Severe => "severe",
    }
}

fn binding_key(binding: Option<Binding>) -> &'static str {
    match binding {
        None => "none",
        Some(Binding::MemoryPressure) => "memory_pressure",
        Some(Binding::CompressorThrash) => "compressor_thrash",
        Some(Binding::CompressorSegments) => "compressor_segments",
        Some(Binding::Swap) => "swap",
        Some(Binding::LowDisk) => "low_disk",
        Some(Binding::DiskFilling) => "disk_filling",
        Some(Binding::Thermal) => "thermal",
        Some(Binding::CpuLoad) => "cpu_load",
        Some(Binding::Battery) => "battery",
        Some(Binding::ExecLatency) => "exec_latency",
    }
}

/// Parses `GITPULSE_MANVI_STEWARD` into `(Some(forced_enabled), Some(description))`.
pub(crate) fn env_override_of(raw: Option<&str>) -> (Option<bool>, Option<String>) {
    let Some(trimmed) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return (None, None);
    };
    match trimmed.to_ascii_lowercase().as_str() {
        "0" | "false" | "off" | "no" => (
            Some(false),
            Some(format!("{STEWARD_ENV}={trimmed} forces the steward off")),
        ),
        "1" | "true" | "on" | "yes" => (
            Some(true),
            Some(format!("{STEWARD_ENV}={trimmed} forces the steward on")),
        ),
        _ => (
            None,
            Some(format!(
                "{STEWARD_ENV}={trimmed} is unrecognized (expected 1/0, on/off, true/false); ignored"
            )),
        ),
    }
}

/// Effective steward policy for `common_dir` with optional environment force flag applied.
pub(crate) fn effective_policy_for(
    settings: &ManviStewardSettings,
    common_dir: Option<&str>,
    forced_enabled: Option<bool>,
) -> EffectiveStewardPolicy {
    let mut eff = settings.effective_for(common_dir);
    if let Some(on) = forced_enabled {
        eff.enabled = on;
    }
    eff
}

/// Whether global defaults or at least one repository override enables the steward.
pub(crate) fn is_master_enabled(
    settings: &ManviStewardSettings,
    forced_enabled: Option<bool>,
) -> bool {
    let any_repo_enabled = settings
        .repositories
        .values()
        .any(|o| o.enabled == Some(true));
    forced_enabled.unwrap_or(settings.enabled || any_repo_enabled)
}

/// Whether global defaults or at least one repository override permits critical slowdown.
pub(crate) fn any_slow_policy_enabled(
    settings: &ManviStewardSettings,
    forced_enabled: Option<bool>,
) -> bool {
    let global = effective_policy_for(settings, None, forced_enabled);
    if global.enabled && global.slow_critical {
        return true;
    }
    settings.repositories.keys().any(|common| {
        let eff = effective_policy_for(settings, Some(common.as_str()), forced_enabled);
        eff.enabled && eff.slow_critical
    })
}

/// Resolves a repository path (or working directory inside one) to its canonical
/// common Git directory string, matching [`crate::tool_config::ReviewGateSettings`].
pub fn canonical_common_dir(repo_or_cwd: &str) -> Option<String> {
    let trimmed = repo_or_cwd.trim();
    if trimmed.is_empty() {
        return None;
    }
    let root = find_git_root(Path::new(trimmed))?;
    let common = resolve_git_common_dir(&root).ok()?;
    let canon = crate::engine::process::canonicalize_plain(&common).unwrap_or(common);
    canon.to_str().map(str::to_owned)
}

/// Verified Claude credential posture for promoting Manvi's built-in default
/// cloud mode (`provider_from == "default"`) in the system steward.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StewardClaudeCredential {
    /// A valid `~/.anthropic-accounts` account resolved by `anthropic-account which`.
    Account(crate::harness::anthropic_account::Account),
    /// No `~/.anthropic-accounts` registry exists, and GitPulse's Keychain holds
    /// a validated `sk-ant-...` API key.
    KeychainKey,
    /// `~/.anthropic-accounts` exists on disk, so GitPulse must not fall back to
    /// its own Keychain key, and reading the registry failed.
    BrokenAccount(String),
    /// GitPulse's Keychain item could not be validated.
    InvalidKeychain(String),
    /// Neither `~/.anthropic-accounts` nor a saved Keychain Claude key is present.
    Absent,
}

pub(crate) fn detect_steward_claude_credential_cached(
    already_verified: &mut bool,
    detect_account: impl FnOnce() -> Result<
        crate::harness::anthropic_account::Detection,
        crate::ipc_error::CommandError,
    >,
    keychain_item_exists: impl FnOnce() -> Result<bool, crate::ipc_error::CommandError>,
    keychain_present: impl FnOnce() -> Result<bool, crate::ipc_error::CommandError>,
) -> Result<StewardClaudeCredential, crate::ipc_error::CommandError> {
    use crate::harness::anthropic_account::Detection;
    match detect_account()? {
        Detection::Found { account, .. } => Ok(StewardClaudeCredential::Account(account)),
        Detection::Broken(why) => Ok(StewardClaudeCredential::BrokenAccount(why)),
        Detection::Absent => match keychain_item_exists() {
            Ok(false) => {
                *already_verified = false;
                Ok(StewardClaudeCredential::Absent)
            }
            Err(err) if err.origin() == Some(IpcErrorKind::DeferredUnderLoad) => Err(err),
            Err(err) => {
                *already_verified = false;
                Ok(StewardClaudeCredential::InvalidKeychain(err.into_message()))
            }
            Ok(true) if *already_verified => Ok(StewardClaudeCredential::KeychainKey),
            Ok(true) => match keychain_present() {
                Ok(true) => {
                    *already_verified = true;
                    Ok(StewardClaudeCredential::KeychainKey)
                }
                Ok(false) => {
                    *already_verified = false;
                    Ok(StewardClaudeCredential::Absent)
                }
                Err(err) if err.origin() == Some(IpcErrorKind::DeferredUnderLoad) => Err(err),
                Err(err) => {
                    *already_verified = false;
                    Ok(StewardClaudeCredential::InvalidKeychain(err.into_message()))
                }
            },
        },
    }
}

#[cfg(all(test, unix))]
pub(crate) fn detect_steward_claude_credential_with(
    detect_account: impl FnOnce() -> Result<
        crate::harness::anthropic_account::Detection,
        crate::ipc_error::CommandError,
    >,
    keychain_present: impl FnOnce() -> Result<bool, crate::ipc_error::CommandError>,
) -> Result<StewardClaudeCredential, crate::ipc_error::CommandError> {
    let mut verified = false;
    detect_steward_claude_credential_cached(
        &mut verified,
        detect_account,
        || Ok(true),
        keychain_present,
    )
}

fn detect_steward_claude_credential(
    already_verified: &mut bool,
) -> Result<StewardClaudeCredential, crate::ipc_error::CommandError> {
    detect_steward_claude_credential_cached(
        already_verified,
        crate::harness::anthropic_account::try_detect,
        || {
            let Ok(keychain) = crate::harness::claude_key::Keychain::system() else {
                return Ok(false);
            };
            keychain.item_exists()
        },
        || {
            let Ok(keychain) = crate::harness::claude_key::Keychain::system() else {
                return Ok(false);
            };
            keychain.present()
        },
    )
}

/// Resolves Manvi's configuration for the system steward: explicit `manvi use
/// cloud` / `manvi use local` works as-is, AND Manvi's default cloud mode
/// (`provider_from == "default"`) is considered configured when a valid
/// `~/.anthropic-accounts` account or GitPulse Keychain Claude key exists.
pub(crate) fn resolve_steward_manvi_status(
    manvi: ManviStatus,
    read_cred: impl FnOnce() -> Result<StewardClaudeCredential, crate::ipc_error::CommandError>,
) -> Result<ManviStatus, crate::ipc_error::CommandError> {
    if !manvi_use::is_default_cloud(&manvi) {
        return Ok(manvi);
    }
    match read_cred()? {
        StewardClaudeCredential::Account(account) => {
            let model = manvi.model.trim().to_string();
            Ok(manvi_use::promote_default_cloud(
                manvi,
                format!(
                    "Claude API via Manvi default ({model}, account `{}`)",
                    account.name
                ),
            ))
        }
        StewardClaudeCredential::KeychainKey => {
            let model = manvi.model.trim().to_string();
            Ok(manvi_use::promote_default_cloud(
                manvi,
                format!("Claude API via Manvi default ({model}, Keychain key)"),
            ))
        }
        StewardClaudeCredential::BrokenAccount(why) => {
            let sanitized = why
                .replace("could not", "couldn't")
                .replace("Could not", "Couldn't")
                .replace("do not", "don't")
                .replace("Do not", "Don't");
            let mut out = manvi;
            out.state = "failed".into();
            out.selection = None;
            out.typed = None;
            out.detail = format!(
                "Manvi's default cloud mode couldn't read ~/.anthropic-accounts ({sanitized}). Fix ~/.anthropic-accounts or choose a local model."
            );
            Ok(out)
        }
        StewardClaudeCredential::InvalidKeychain(why) => {
            let sanitized = why
                .replace("could not", "couldn't")
                .replace("Could not", "Couldn't")
                .replace("do not", "don't")
                .replace("Do not", "Don't");
            let mut out = manvi;
            out.state = "failed".into();
            out.selection = None;
            out.typed = None;
            out.detail = sanitized;
            Ok(out)
        }
        StewardClaudeCredential::Absent => Ok(manvi),
    }
}

fn read_steward_manvi(
    already_verified: &mut bool,
) -> Result<ManviStatus, crate::ipc_error::CommandError> {
    let raw = manvi_use::try_read()?;
    resolve_steward_manvi_status(raw, || detect_steward_claude_credential(already_verified))
}

fn probe_live_manvi_on(rt: &StewardRuntime, disk_remedy: bool) -> ManviProbe {
    let prev_verified = crate::supervisor::lock_or_recover(&rt.inner).verified_keychain_key;
    let mut verified = prev_verified;
    let attempt = if disk_remedy {
        crate::engine::process::with_disk_relief(|| read_steward_manvi(&mut verified))
    } else {
        read_steward_manvi(&mut verified)
    };
    match attempt {
        Ok(status) => {
            if verified != prev_verified {
                crate::supervisor::lock_or_recover(&rt.inner).verified_keychain_key = verified;
            }
            ManviProbe::Answered(Box::new(status))
        }
        Err(err) if err.origin() == Some(IpcErrorKind::DeferredUnderLoad) => {
            ManviProbe::Deferred(err.message().to_string())
        }
        Err(err) => ManviProbe::Unreadable(err.message().to_string()),
    }
}

/// Whether a `manvi use` answer names a model the steward can use.
fn is_manvi_ready(manvi: &ManviStatus) -> bool {
    manvi.state == "ready" && manvi.typed.is_some() && manvi.unsupported.is_empty()
}

/// True when the last `manvi use` answer this GitPulse cached names a usable
/// model. Spawns nothing (terminal launch asks it), so it is false until the
/// steward's first read after startup.
pub fn manvi_ready_cached() -> bool {
    SERVICE.get().is_some_and(|rt| {
        crate::supervisor::lock_or_recover(&rt.inner)
            .cached_manvi
            .as_ref()
            .is_some_and(is_manvi_ready)
    })
}

/// What a status read shows when `manvi use` could not be asked and nothing
/// earlier was cached.
fn unreadable_manvi_status(why: &str) -> ManviStatus {
    ManviStatus {
        state: "failed".into(),
        detail: format!("Couldn't ask Manvi: {why}"),
        ..ManviStatus::default()
    }
}

fn resolve_manvi_for_status(rt: &StewardRuntime) -> ManviStatus {
    match probe_live_manvi_on(rt, crate::sysload::disk_remedy_due()) {
        ManviProbe::Answered(status) => {
            crate::supervisor::lock_or_recover(&rt.inner).cached_manvi =
                Some(ManviStatus::clone(&status));
            *status
        }
        ManviProbe::Deferred(why) => {
            let cached = crate::supervisor::lock_or_recover(&rt.inner)
                .cached_manvi
                .clone();
            cached.unwrap_or_else(|| ManviStatus {
                state: "deferred".into(),
                detail: format!("Host load deferred checking Manvi: {why}"),
                ..ManviStatus::default()
            })
        }
        ManviProbe::Unreadable(why) => crate::supervisor::lock_or_recover(&rt.inner)
            .cached_manvi
            .clone()
            .unwrap_or_else(|| unreadable_manvi_status(&why)),
    }
}

fn resolve_manvi_cached_or_probe(
    rt: &StewardRuntime,
    settings: &ManviStewardSettings,
    repo_path: Option<&str>,
    prefer_cached: bool,
) -> ManviStatus {
    let (forced, _) = env_override_of(std::env::var(STEWARD_ENV).ok().as_deref());
    let master_on = is_master_enabled(settings, forced);
    let cached = crate::supervisor::lock_or_recover(&rt.inner)
        .cached_manvi
        .clone();
    if !master_on && repo_path.is_none() {
        return cached.unwrap_or_else(|| ManviStatus {
            state: "unchecked".into(),
            detail: "Manvi model wasn't checked because the system steward is switched off.".into(),
            ..ManviStatus::default()
        });
    }
    if prefer_cached || !master_on {
        if let Some(prev) = cached {
            return prev;
        }
    }
    resolve_manvi_for_status(rt)
}

/// Updates the steward's cached Manvi status immediately when the user changes
/// `manvi use` in the UI (`cmd_manvi_use`), waking the worker and emitting a
/// fresh status snapshot.
pub fn note_manvi_status(status: &ManviStatus) {
    let rt = runtime();
    adopt_manvi_status(&mut crate::supervisor::lock_or_recover(&rt.inner), status);
    rt.wake_cv.notify_one();
    let settings = crate::tool_config::manvi_steward_settings();
    let out = build_status(rt, settings, status, None);
    emit_status(rt, &out);
}

/// Invalidates and re-evaluates the steward's cached Manvi status when the
/// user saves or deletes a Keychain Claude key (`cmd_claude_key_save` /
/// `cmd_claude_key_delete`), so default-cloud promotion updates immediately.
pub fn note_claude_credential_changed() {
    let rt = runtime();
    forget_credential(&mut crate::supervisor::lock_or_recover(&rt.inner));
    rt.wake_cv.notify_one();
    let settings = crate::tool_config::manvi_steward_settings();
    let manvi = resolve_manvi_cached_or_probe(rt, &settings, None, false);
    let out = build_status(rt, settings, &manvi, None);
    emit_status(rt, &out);
}

/// A new `manvi use` choice: the cached answer is replaced, and a rejection
/// recorded against the old choice no longer applies.
fn adopt_manvi_status(state: &mut InnerState, status: &ManviStatus) {
    state.cached_manvi = Some(status.clone());
    state.credential_rejected = None;
    state.settings_epoch = state.settings_epoch.wrapping_add(1);
}

/// A saved or deleted Keychain key: everything learned about the old one goes.
fn forget_credential(state: &mut InnerState) {
    state.cached_manvi = None;
    state.verified_keychain_key = false;
    state.credential_rejected = None;
    state.settings_epoch = state.settings_epoch.wrapping_add(1);
}

/// Records what one call through Manvi's Anthropic route said about the key:
/// `Some(true)` the key was refused for `model`, `Some(false)` a call
/// succeeded (so the key works, whatever model it was for), `None` nothing
/// about the key. Returns whether the recorded state changed.
fn record_cloud_key_answer(state: &mut InnerState, model: &str, answer: Option<bool>) -> bool {
    let next = match answer {
        Some(true) => Some(model.to_string()),
        Some(false) => None,
        None => return false,
    };
    let changed = state.credential_rejected != next;
    state.credential_rejected = next;
    changed
}

/// What a `work.assist.complete` answer on `provider` says about the key:
/// only an Anthropic route counts, a refusal is `E_PROVIDER_AUTH`, and only a
/// real call (not a dry run, which reaches no provider) proves the key works.
pub(crate) fn key_answer_of(
    provider: &str,
    dry_run: bool,
    answer: &Result<serde_json::Value, ManviError>,
) -> Option<bool> {
    if provider != "anthropic" {
        return None;
    }
    match answer {
        Err(failure) if failure.code == crate::ai::MANVI_PROVIDER_AUTH => Some(true),
        Ok(_) if !dry_run => Some(false),
        _ => None,
    }
}

/// What one steward model call (always on the cloud route when this is asked)
/// says about the key: a success proves it works, Anthropic's refusal
/// (`credential_rejected`) says it does not, any other failure says nothing.
fn key_answer_of_call<T>(answer: &Result<T, ModelCallError>) -> Option<bool> {
    match answer {
        Ok(_) => Some(false),
        Err(failure) if failure.credential_rejected => Some(true),
        Err(_) => None,
    }
}

/// The cleanup selection's model call. It runs on the same key as every other
/// cloud call, so what its answer says about the key is reported too.
fn cleanup_evaluator<'a>(
    model: &'a str,
    call: impl Fn(&str, &str, i64) -> Result<String, ModelCallError> + 'a,
) -> impl Fn(&str, &str, i64) -> Result<String, String> + 'a {
    move |system, user, max_tokens| {
        let answer = call(system, user, max_tokens);
        note_cloud_key_answer(model, key_answer_of_call(&answer));
        answer.map_err(|failure| failure.message)
    }
}

#[cfg(test)]
thread_local! {
    /// Every [`note_cloud_key_answer`] made on this thread, in order. The
    /// runtime it writes is process-wide and any parallel test may change it,
    /// so a test of a call site reads this instead.
    static KEY_ANSWERS_NOTED: std::cell::RefCell<Vec<(String, Option<bool>)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Takes the answers this thread reported through [`note_cloud_key_answer`].
#[cfg(test)]
pub(crate) fn take_key_answers_noted_here() -> Vec<(String, Option<bool>)> {
    KEY_ANSWERS_NOTED.with(|noted| std::mem::take(&mut *noted.borrow_mut()))
}

/// Called by every GitPulse call through Manvi's Anthropic route so a key
/// Anthropic refused shows in the steward's status whichever feature found
/// out, and a working key clears it.
pub fn note_cloud_key_answer(model: &str, answer: Option<bool>) {
    #[cfg(test)]
    KEY_ANSWERS_NOTED.with(|noted| noted.borrow_mut().push((model.to_string(), answer)));
    let rt = runtime();
    let cached = {
        let mut guard = crate::supervisor::lock_or_recover(&rt.inner);
        if !record_cloud_key_answer(&mut guard, model, answer) {
            return;
        }
        guard.cached_manvi.clone()
    };
    if let Some(manvi) = cached {
        let settings = crate::tool_config::manvi_steward_settings();
        let out = build_status(rt, settings, &manvi, None);
        emit_status(rt, &out);
    }
}

/// Installs and starts the periodic Manvi steward background loop in the
/// GitPulse desktop app process. Idempotent.
pub fn install<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    workbench: WorkbenchState,
    watcher: WatcherState,
) {
    use tauri::Emitter as _;
    static STARTED: AtomicBool = AtomicBool::new(false);
    let rt = Arc::clone(runtime());
    *crate::supervisor::lock_or_recover(&rt.workbench) = Some(workbench);
    *crate::supervisor::lock_or_recover(&rt.watcher) = Some(watcher);
    let handle = app.clone();
    *crate::supervisor::lock_or_recover(&rt.emit) = Some(Arc::new(move |status| {
        let _ = handle.emit("manvi-steward-updated", status);
    }));
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    let worker_rt = Arc::clone(&rt);
    if let Err(error) =
        crate::supervisor::spawn_once("manvi-steward", "Manvi system steward", move || {
            crate::supervisor::run_supervised(
                "manvi-steward",
                crate::supervisor::Policy::RESTART,
                move || worker_loop(&worker_rt),
            );
        })
    {
        log::warn!(target: "steward", "Manvi system steward did not start: {error}");
    }
}

/// Canonical critical-pressure predicate on a sampler pulse.
pub(crate) fn is_critical_pulse(pulse: &HostLoadPulse) -> bool {
    pulse.pressure == PressureInput::Critical
        || !pulse.governor.admit
        || pulse.governor.shed_reactive
}

/// Canonical critical-pressure predicate on a full `HostLoadStatus`, kept in
/// lockstep with [`is_critical_pulse`] so an early `PressureInput::Critical`
/// wakeup acts immediately instead of being misclassified as `"healthy"`.
pub(crate) fn is_critical_load(load: &HostLoadStatus) -> bool {
    let verdict = &load.governor.verdict;
    verdict.tier >= Tier::Severe
        || !verdict.admit
        || verdict.shed.reactive
        || load.pressure == PressureInput::Critical
}

/// Notifies the steward from `start_host_load_sampler`'s tick callback so an
/// entry into critical pressure wakes the steward within seconds instead of
/// waiting out the full periodic interval. Never blocks the sampler thread.
pub fn notify_pulse(pulse: &HostLoadPulse) {
    let Some(rt) = SERVICE.get() else {
        return;
    };
    notify_pulse_on(rt, pulse);
}

pub(crate) fn notify_pulse_on(rt: &StewardRuntime, pulse: &HostLoadPulse) {
    if rt.closed.load(Ordering::Relaxed) || !pulse.enabled || !pulse.state.governs() {
        return;
    }
    let Ok(mut guard) = rt.inner.try_lock() else {
        return;
    };
    // When another live GitPulse process holds the host-load writer election
    // (`Role::Reader`), that primary instance governs the machine; only wake if
    // this instance needs to release throttles it held before losing election.
    if pulse.role == crate::sysload::Role::Reader {
        guard.last_critical_wake = None;
        if guard.critical_throttle_active || !guard.throttled.is_empty() {
            guard.wake_requested = Some("scheduled");
            rt.wake_cv.notify_one();
        }
        return;
    }
    let critical = is_critical_pulse(pulse);
    if critical {
        if !guard.critical_throttle_active {
            let debounced = guard
                .last_critical_wake
                .is_some_and(|at| at.elapsed() < CRITICAL_WAKE_DEBOUNCE);
            if !debounced {
                guard.last_critical_wake = Some(Instant::now());
                guard.wake_requested = Some("critical_wake");
                rt.wake_cv.notify_one();
            }
        }
    } else {
        // Always clear `last_critical_wake` once a governable non-critical sample
        // arrives (even when `slow_critical` is disabled so `critical_throttle_active`
        // stayed false), ensuring a subsequent critical episode wakes immediately.
        guard.last_critical_wake = None;
        if guard.critical_throttle_active {
            // Wake promptly to release OS background throttles and active.json once pressure clears.
            guard.wake_requested = Some("scheduled");
            rt.wake_cv.notify_one();
        }
    }
}

/// Restores any OS-throttled agent processes and removes `steward/active.json`
/// on application shutdown.
pub fn shutdown() {
    let Some(rt) = SERVICE.get() else {
        return;
    };
    rt.closed.store(true, Ordering::SeqCst);
    rt.wake_cv.notify_all();
    release_all_live_throttles(rt);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WorkerDecision {
    Run(&'static str),
    /// `None` parks indefinitely on `wake_cv` (used while the steward is
    /// disabled everywhere so zero periodic timer wakeups occur).
    Sleep(Option<Duration>),
    ReleaseAndSleep(Option<Duration>),
}

/// Longest gap between ticks while a critical slowdown is in force.
const CRITICAL_RENEW_SECS: u64 = 300;
/// The longest lease [`lease_ttl_ms`] issues; readers refuse a longer one.
const MAX_LEASE_TTL_MS: u64 = 900_000;

/// How often the lease is renewed while a critical slowdown holds.
pub(crate) fn renew_interval_secs(interval_secs: u64) -> u64 {
    interval_secs
        .clamp(STEWARD_MIN_INTERVAL_SECS, STEWARD_MAX_INTERVAL_SECS)
        .min(CRITICAL_RENEW_SECS)
}

/// Lease lifetime: three renewals, at least five minutes (a tick that runs a
/// long cleanup must not outlive it) and at most fifteen. A writer that dies
/// sooner is caught by the reader's writer-liveness check, not by expiry.
pub(crate) fn lease_ttl_ms(interval_secs: u64) -> u64 {
    renew_interval_secs(interval_secs)
        .saturating_mul(3)
        .saturating_mul(1_000)
        .clamp(300_000, MAX_LEASE_TTL_MS)
}

/// Pure scheduler step for [`worker_loop`]. While the steward is disabled
/// globally and across every repository override, clears pending wakes and
/// parks indefinitely (`None`) without running scheduled checks, spawning
/// `manvi use`, or polluting `history` with `"disabled"` entries.
pub(crate) fn decide_worker_step_with_epoch(
    rt: &StewardRuntime,
    settings: &ManviStewardSettings,
    forced_enabled: Option<bool>,
    now_ms: u64,
) -> (WorkerDecision, u64) {
    let mut guard = crate::supervisor::lock_or_recover(&rt.inner);
    let epoch = guard.settings_epoch;
    if !is_master_enabled(settings, forced_enabled) {
        guard.wake_requested = None;
        guard.last_critical_wake = None;
        if guard.critical_throttle_active || !guard.throttled.is_empty() {
            return (WorkerDecision::ReleaseAndSleep(None), epoch);
        }
        return (WorkerDecision::Sleep(None), epoch);
    }
    // While a critical slowdown holds, the lease is renewed at least every
    // CRITICAL_RENEW_SECS even when the configured interval is longer, so it
    // cannot expire under sustained pressure (see `lease_ttl_ms`).
    let interval_ms = if guard.critical_throttle_active {
        renew_interval_secs(settings.interval_secs)
    } else {
        settings
            .interval_secs
            .clamp(STEWARD_MIN_INTERVAL_SECS, STEWARD_MAX_INTERVAL_SECS)
    } * 1_000;
    if let Some(wake) = guard.wake_requested.take() {
        return (WorkerDecision::Run(wake), epoch);
    }
    let due = guard
        .last_check_ms
        .is_none_or(|last| now_ms.saturating_sub(last) >= interval_ms);
    if due {
        return (WorkerDecision::Run("scheduled"), epoch);
    }
    let remaining_ms = guard
        .last_check_ms
        .map(|last| interval_ms.saturating_sub(now_ms.saturating_sub(last)))
        .unwrap_or(LOOP_POLL.as_millis() as u64);
    (
        WorkerDecision::Sleep(Some(Duration::from_millis(remaining_ms).min(LOOP_POLL))),
        epoch,
    )
}

/// Waits on `wake_cv` until `sleep_for` elapses (`None` = park indefinitely),
/// checking `closed`, `wake_requested`, and `settings_epoch` under the mutex
/// BEFORE sleeping so notifications arriving across lock boundaries are never lost.
pub(crate) fn wait_for_worker_wakeup(
    rt: &StewardRuntime,
    sleep_for: Option<Duration>,
    seen_epoch: u64,
) {
    let guard = crate::supervisor::lock_or_recover(&rt.inner);
    match sleep_for {
        Some(dur) => {
            let _ = rt.wake_cv.wait_timeout_while(guard, dur, |g| {
                !rt.closed.load(Ordering::Relaxed)
                    && g.wake_requested.is_none()
                    && g.settings_epoch == seen_epoch
            });
        }
        None => {
            drop(rt.wake_cv.wait_while(guard, |g| {
                !rt.closed.load(Ordering::Relaxed)
                    && g.wake_requested.is_none()
                    && g.settings_epoch == seen_epoch
            }));
        }
    }
}

fn worker_loop(rt: &Arc<StewardRuntime>) {
    // A GitPulse that crashed mid-slowdown left its throttles in place; undo
    // them before anything else (whatever the settings now say).
    if let Some(dir) = crate::tool_config::default_config_dir() {
        let restored = restore_throttle_journal(&dir, |pid, start_us| {
            crate::sysload::relief::steward_throttle(pid, start_us, false)
        });
        if restored > 0 {
            log::info!(target: "steward", "restored {restored} agent processes a previous GitPulse left throttled");
        }
    }
    // Seed cached `manvi use` status once while startup is calm (only when
    // enabled) so a later severe-load wakeup never lacks Manvi's model selection.
    let initial_settings = crate::tool_config::manvi_steward_settings();
    let (initial_forced, _) = env_override_of(std::env::var(STEWARD_ENV).ok().as_deref());
    if is_master_enabled(&initial_settings, initial_forced) {
        if let ManviProbe::Answered(initial) = probe_live_manvi_on(rt, false) {
            crate::supervisor::lock_or_recover(&rt.inner).cached_manvi = Some(*initial);
        }
        // Brief initial settle so the host-load sampler completes its first
        // ENTER_SAMPLES ticks, waking immediately on critical pulse or settings save.
        let initial_epoch = crate::supervisor::lock_or_recover(&rt.inner).settings_epoch;
        wait_for_worker_wakeup(rt, Some(Duration::from_secs(8)), initial_epoch);
    }
    while !rt.closed.load(Ordering::Relaxed) {
        let settings = crate::tool_config::manvi_steward_settings();
        let (forced, _) = env_override_of(std::env::var(STEWARD_ENV).ok().as_deref());
        let (decision, seen_epoch) =
            decide_worker_step_with_epoch(rt, &settings, forced, now_epoch_ms());
        let trigger = match decision {
            WorkerDecision::Run(trigger) => Some(trigger),
            WorkerDecision::ReleaseAndSleep(sleep_for) => {
                release_all_live_throttles(rt);
                wait_for_worker_wakeup(rt, sleep_for, seen_epoch);
                None
            }
            WorkerDecision::Sleep(sleep_for) => {
                wait_for_worker_wakeup(rt, sleep_for, seen_epoch);
                None
            }
        };
        if rt.closed.load(Ordering::Relaxed) {
            break;
        }
        if let Some(trigger) = trigger {
            if trigger != "manual" && crate::sysload::status().role == crate::sysload::Role::Reader
            {
                // Another live GitPulse holds the host-load writer election and
                // runs periodic stewardship; release only our own throttles and
                // advance `last_check_ms` so we do not duplicate LLM calls or
                // bus directives.
                release_all_live_throttles(rt);
                crate::supervisor::lock_or_recover(&rt.inner).last_check_ms = Some(now_epoch_ms());
                continue;
            }
            let workbench = crate::supervisor::lock_or_recover(&rt.workbench).clone();
            let watcher = crate::supervisor::lock_or_recover(&rt.watcher).clone();
            if let Some(wb) = workbench {
                if run_live_tick(rt, trigger, None, &wb, watcher.as_ref()).is_err() {
                    // A manual check holds the tick. Wait for it (or another
                    // wake) instead of re-deciding "due" in a tight loop.
                    let epoch = crate::supervisor::lock_or_recover(&rt.inner).settings_epoch;
                    wait_for_worker_wakeup(rt, Some(LOOP_POLL), epoch);
                }
            }
        }
    }
}

/// Reads current steward status without triggering a check or model call.
pub fn status(repo_path: Option<&str>) -> ManviStewardStatus {
    let rt = runtime();
    let settings = crate::tool_config::manvi_steward_settings();
    let manvi = resolve_manvi_cached_or_probe(rt, &settings, repo_path, repo_path.is_none());
    build_status(rt, settings, &manvi, repo_path)
}

/// Runs an immediate steward check (`trigger = "manual"`) and returns the
/// updated status without redundant `manvi use` subprocess spawns.
pub fn run_now(
    repo_path: Option<&str>,
    workbench: &WorkbenchState,
    watcher: Option<&WatcherState>,
) -> Result<ManviStewardStatus, String> {
    let rt = runtime();
    let (_, updated) = run_live_tick(rt, "manual", repo_path, workbench, watcher)?;
    Ok(updated)
}

/// Saves global steward settings and reconciles any live OS/hook throttles
/// immediately rather than waiting for the next scheduled check.
pub fn save_settings(
    mut next: ManviStewardSettings,
    repo_path: Option<&str>,
) -> Result<ManviStewardStatus, String> {
    crate::tool_config::set_manvi_steward_settings(next)?;
    next = crate::tool_config::manvi_steward_settings();
    let rt = runtime();
    bump_settings_and_reconcile_live_throttles(rt, &next);
    rt.wake_cv.notify_one();
    let manvi = resolve_manvi_cached_or_probe(rt, &next, repo_path, true);
    let out = build_status(rt, next, &manvi, repo_path);
    emit_status(rt, &out);
    Ok(out)
}

/// Sets or clears (`None`) a per-repository steward override for `repo_path`,
/// immediately releasing any OS-throttled processes in that repository if its
/// effective `enabled && slow_critical` policy turned off.
pub fn save_repo_override(
    repo_path: &str,
    override_policy: Option<crate::tool_config::RepoStewardOverride>,
) -> Result<ManviStewardStatus, String> {
    let common_dir = canonical_common_dir(repo_path).ok_or_else(|| {
        "Selected path is not inside a Git repository, so a repository override cannot be saved."
            .to_string()
    })?;
    let next = crate::tool_config::set_manvi_steward_repo_override(&common_dir, override_policy)?;
    let rt = runtime();
    bump_settings_and_reconcile_live_throttles(rt, &next);
    rt.wake_cv.notify_one();
    let manvi = resolve_manvi_cached_or_probe(rt, &next, Some(repo_path), true);
    let out = build_status(rt, next, &manvi, Some(repo_path));
    emit_status(rt, &out);
    Ok(out)
}

fn release_all_live_throttles(rt: &StewardRuntime) {
    let dir = crate::tool_config::default_config_dir();
    if let Some(dir) = &dir {
        let _ = clear_active_throttle_if_owned_or_stale(dir, now_epoch_ms());
    }
    let throttled = {
        let mut guard = crate::supervisor::lock_or_recover(&rt.inner);
        guard.critical_throttle_active = false;
        std::mem::take(&mut guard.throttled)
    };
    if throttled.is_empty() {
        return;
    }
    for (pid, (start_us, _)) in throttled {
        let _ = crate::sysload::relief::steward_throttle(pid, start_us, false);
    }
    if let Some(dir) = &dir {
        let _ = write_throttle_journal(dir, &BTreeMap::new());
    }
}

fn bump_settings_and_reconcile_live_throttles(
    rt: &StewardRuntime,
    settings: &ManviStewardSettings,
) {
    let (forced_enabled, _) = env_override_of(std::env::var(STEWARD_ENV).ok().as_deref());
    let any_slow = any_slow_policy_enabled(settings, forced_enabled);
    if !any_slow {
        if let Some(dir) = crate::tool_config::default_config_dir() {
            let _ = clear_active_throttle(&dir);
        }
    }
    let to_restore = {
        let mut guard = crate::supervisor::lock_or_recover(&rt.inner);
        guard.settings_epoch = guard.settings_epoch.wrapping_add(1);
        let mut removed = Vec::new();
        guard.throttled.retain(|&pid, (start_us, common)| {
            let eff = effective_policy_for(settings, common.as_deref(), forced_enabled);
            if any_slow && eff.enabled && eff.slow_critical {
                true
            } else {
                removed.push((pid, *start_us));
                false
            }
        });
        if !any_slow {
            guard.critical_throttle_active = false;
        }
        (!removed.is_empty()).then(|| (removed, guard.throttled.clone()))
    };
    let Some((to_restore, remaining)) = to_restore else {
        return;
    };
    for (pid, start_us) in to_restore {
        let _ = crate::sysload::relief::steward_throttle(pid, start_us, false);
    }
    if let Some(dir) = crate::tool_config::default_config_dir() {
        let _ = write_throttle_journal(&dir, &remaining);
    }
}

fn emit_status(rt: &StewardRuntime, status: &ManviStewardStatus) {
    let cb = crate::supervisor::lock_or_recover(&rt.emit).clone();
    if let Some(emit) = cb {
        emit(status);
    }
}

fn present_manvi(manvi: &ManviStatus) -> (String, String, String, String) {
    if manvi.state == "ready" && !manvi.unsupported.is_empty() {
        return (
            "unsupported".into(),
            manvi.mode.clone(),
            manvi.model.clone(),
            manvi.unsupported.clone(),
        );
    }
    let detail = if !manvi.detail.is_empty() {
        manvi.detail.clone()
    } else if !manvi.unsupported.is_empty() {
        manvi.unsupported.clone()
    } else if manvi.state == "ready" {
        match manvi.mode.as_str() {
            "cloud" => format!("Claude API via Manvi ({})", manvi.model),
            "local" => format!(
                "Local model via Manvi ({} at {})",
                manvi.model, manvi.local_base_url
            ),
            _ => manvi.model.clone(),
        }
    } else {
        String::new()
    };
    (
        manvi.state.clone(),
        manvi.mode.clone(),
        manvi.model.clone(),
        detail,
    )
}

/// A ready cloud model whose key Anthropic refused reads `rejected`, with the
/// sentence that says what to do. A local model, or a cloud model other than
/// the one refused, is left as it is: the refusal was not about it.
fn present_rejected_credential(
    rejected_model: Option<&str>,
    manvi: &ManviStatus,
    manvi_state: String,
    manvi_detail: String,
) -> (String, String) {
    if manvi_state == "ready"
        && manvi.mode == "cloud"
        && rejected_model == Some(manvi.model.as_str())
    {
        (
            "rejected".to_string(),
            crate::ai::REJECTED_KEY_MESSAGE.to_string(),
        )
    } else {
        (manvi_state, manvi_detail)
    }
}

fn build_status(
    rt: &StewardRuntime,
    settings: ManviStewardSettings,
    manvi: &ManviStatus,
    repo_path: Option<&str>,
) -> ManviStewardStatus {
    build_status_with_env(
        rt,
        settings,
        manvi,
        repo_path,
        std::env::var(STEWARD_ENV).ok().as_deref(),
    )
}

pub(crate) fn build_status_with_env(
    rt: &StewardRuntime,
    settings: ManviStewardSettings,
    manvi: &ManviStatus,
    repo_path: Option<&str>,
    raw_env: Option<&str>,
) -> ManviStewardStatus {
    let repo_common_dir = repo_path.and_then(canonical_common_dir);
    let (forced, env_note) = env_override_of(raw_env);
    let effective = effective_policy_for(&settings, repo_common_dir.as_deref(), forced);
    let (manvi_state, manvi_mode, manvi_model, manvi_detail) = present_manvi(manvi);
    let guard = crate::supervisor::lock_or_recover(&rt.inner);
    let interval_ms = settings
        .interval_secs
        .clamp(STEWARD_MIN_INTERVAL_SECS, STEWARD_MAX_INTERVAL_SECS)
        * 1_000;
    // When `repo_path` resolves to a repository, `next_check_ms` reflects whether
    // that repository participates; for machine-wide status (`repo_path == None`),
    // `next_check_ms` reflects whether global defaults OR any per-repository
    // override schedules checks (`is_master_enabled`).
    let schedule_enabled = if repo_common_dir.is_some() {
        effective.enabled
    } else {
        is_master_enabled(&settings, forced)
    };
    let next_check_ms = if schedule_enabled && manvi_state == "ready" {
        Some(
            guard
                .last_check_ms
                .map(|last| last.saturating_add(interval_ms))
                .unwrap_or_else(now_epoch_ms),
        )
    } else {
        None
    };
    // A rejected key still passes every local check, so only the provider's
    // refusal can say it is not usable, and only a call that succeeds clears
    // this: the steward's own calls, which it makes only under load (a calm,
    // healthy tick calls no model), or any AI feature's. A fix the app cannot
    // see (`~/.anthropic-accounts` edited) therefore reads rejected until one of
    // those calls is made.
    let (manvi_state, manvi_detail) = present_rejected_credential(
        guard.credential_rejected.as_deref(),
        manvi,
        manvi_state,
        manvi_detail,
    );
    ManviStewardStatus {
        settings,
        effective,
        repo_common_dir,
        env_override: env_note,
        env_forced: forced,
        manvi_state,
        manvi_mode,
        manvi_model,
        manvi_detail,
        running: rt.running_tick.load(Ordering::Relaxed),
        critical_throttle_active: guard.critical_throttle_active,
        throttled_count: u32::try_from(guard.throttled.len()).unwrap_or(u32::MAX),
        last_check_ms: guard.last_check_ms,
        next_check_ms,
        history: guard.history.clone(),
    }
}

/* ── Cross-process critical slowdown record (`steward/active.json`) ──────── */

pub(crate) fn write_active_throttle(
    config_dir: &Path,
    record: &ActiveThrottleRecord,
) -> Result<(), String> {
    let bytes = serde_json::to_vec(record).map_err(|e| e.to_string())?;
    write_private_steward_file(config_dir, ACTIVE_FILE, &bytes)
}

/// Writes `<config_dir>/steward/<name>` atomically (temp file and rename),
/// owner-only and never through a link.
fn write_private_steward_file(config_dir: &Path, name: &str, bytes: &[u8]) -> Result<(), String> {
    let dir = config_dir.join(STEWARD_SUBDIR);
    crate::hooks::advisory_once::private_dir(&dir)?;
    crate::hooks::advisory_once::write_private_atomic(&dir, name, bytes)
}

/// The processes this GitPulse put (or let inherit) into background priority,
/// kept on disk so a GitPulse that crashed or was killed has them restored at
/// its next start instead of leaving them slowed until they exit.
pub const THROTTLE_JOURNAL_FILE: &str = "throttled.json";
const MAX_THROTTLE_JOURNAL_BYTES: u64 = 256 * 1024;

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct ThrottleJournal {
    version: u32,
    writer_pid: u32,
    writer_start_us: u64,
    /// `(pid, start_us)`: the start time keeps a recycled pid from being touched.
    entries: Vec<(i32, u64)>,
}

/// Records `throttled`, or removes the journal when it is empty.
pub(crate) fn write_throttle_journal(
    config_dir: &Path,
    throttled: &BTreeMap<i32, (u64, Option<String>)>,
) -> Result<(), String> {
    if throttled.is_empty() {
        let path = config_dir.join(STEWARD_SUBDIR).join(THROTTLE_JOURNAL_FILE);
        return match fs::symlink_metadata(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.to_string()),
            Ok(meta) if meta.file_type().is_symlink() || !meta.is_file() => {
                Err("refusing to remove non-regular steward throttle journal".into())
            }
            Ok(_) => fs::remove_file(&path).map_err(|e| e.to_string()),
        };
    }
    let writer_pid = std::process::id();
    let journal = ThrottleJournal {
        version: 1,
        writer_pid,
        writer_start_us: crate::sysload::process_start_us(writer_pid).unwrap_or(0),
        entries: throttled
            .iter()
            .map(|(pid, (start_us, _))| (*pid, *start_us))
            .collect(),
    };
    let bytes = serde_json::to_vec(&journal).map_err(|e| e.to_string())?;
    write_private_steward_file(config_dir, THROTTLE_JOURNAL_FILE, &bytes)
}

/// Restores what a previous GitPulse's journal lists and removes it, unless
/// the GitPulse that wrote it is still running (it still owns those
/// throttles). Returns how many processes were put back to normal priority;
/// exited and recycled pids refuse the restore through their start time.
pub(crate) fn restore_throttle_journal(
    config_dir: &Path,
    restore: impl Fn(i32, u64) -> Result<bool, String>,
) -> u32 {
    let path = config_dir.join(STEWARD_SUBDIR).join(THROTTLE_JOURNAL_FILE);
    let Ok(meta) = fs::symlink_metadata(&path) else {
        return 0;
    };
    if meta.file_type().is_symlink() || !meta.is_file() {
        return 0;
    }
    let mut opts = OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NOFOLLOW);
    }
    let mut raw = Vec::new();
    let read = opts.open(&path).ok().and_then(|f| {
        f.take(MAX_THROTTLE_JOURNAL_BYTES + 1)
            .read_to_end(&mut raw)
            .ok()
    });
    let journal = read
        .filter(|_| raw.len() as u64 <= MAX_THROTTLE_JOURNAL_BYTES)
        .and_then(|_| serde_json::from_slice::<ThrottleJournal>(&raw).ok())
        .filter(|j| j.version == 1);
    let Some(journal) = journal else {
        let _ = fs::remove_file(&path);
        return 0;
    };
    if journal.writer_pid != std::process::id()
        && crate::sysload::process_start_us(journal.writer_pid)
            .is_some_and(|live| live == journal.writer_start_us)
    {
        return 0;
    }
    let restored = journal
        .entries
        .iter()
        .filter(|(pid, start_us)| matches!(restore(*pid, *start_us), Ok(true)))
        .count();
    let _ = fs::remove_file(&path);
    u32::try_from(restored).unwrap_or(u32::MAX)
}

pub(crate) fn clear_active_throttle(config_dir: &Path) -> Result<(), String> {
    let path = config_dir.join(STEWARD_SUBDIR).join(ACTIVE_FILE);
    match fs::symlink_metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.to_string()),
        Ok(meta) => {
            if meta.file_type().is_symlink() || !meta.is_file() {
                return Err("refusing to remove non-regular steward active record".into());
            }
            fs::remove_file(&path).map_err(|e| e.to_string())
        }
    }
}

/// Removes `<config_dir>/steward/active.json` unless it is a valid, unexpired
/// lease owned by a *different* still-live GitPulse writer process. Used during
/// process shutdown and passive stand-downs so a secondary `Role::Reader`
/// instance or test binary never clobbers the primary writer's active slowdown.
pub(crate) fn clear_active_throttle_if_owned_or_stale(
    config_dir: &Path,
    now_ms: u64,
) -> Result<(), String> {
    let path = config_dir.join(STEWARD_SUBDIR).join(ACTIVE_FILE);
    let meta = match fs::symlink_metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.to_string()),
        Ok(meta) => meta,
    };
    if meta.file_type().is_symlink() || !meta.is_file() {
        return Err("refusing to remove non-regular steward active record".into());
    }
    if meta.len() <= MAX_ACTIVE_RECORD_BYTES {
        let mut opts = OpenOptions::new();
        opts.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.custom_flags(libc::O_NOFOLLOW);
        }
        let mut raw = Vec::new();
        if opts
            .open(&path)
            .ok()
            .and_then(|f| {
                f.take(MAX_ACTIVE_RECORD_BYTES + 1)
                    .read_to_end(&mut raw)
                    .ok()
            })
            .is_some()
            && (raw.len() as u64) <= MAX_ACTIVE_RECORD_BYTES
        {
            if let Ok(record) = serde_json::from_slice::<ActiveThrottleRecord>(&raw) {
                // A live foreign writer owns its lease whatever version it
                // writes; an older GitPulse still running keeps its own.
                if now_ms <= record.expires_at_ms && record.writer_pid != std::process::id() {
                    let foreign_writer_alive = crate::sysload::process_start_us(record.writer_pid)
                        .is_some_and(|live_start| live_start == record.writer_start_us);
                    if foreign_writer_alive {
                        return Ok(());
                    }
                }
            }
        }
    }
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

/// Reads an active critical-throttle lease for `cwd` when present, secure,
/// unexpired, owned by a still-live GitPulse writer process, and permitted by
/// global + per-repository steward settings (plus `GITPULSE_MANVI_STEWARD`).
///
/// Fast-path: when `<config_dir>/steward/active.json` does not exist (the
/// normal case on a healthy host), performs one `symlink_metadata` call and
/// returns `None` without opening `tools.json` or spawning anything.
#[cfg(test)]
pub(crate) fn active_throttle_for_cwd_with_env(
    config_dir: &Path,
    cwd: &str,
    now_ms: u64,
    raw_env: Option<&str>,
) -> Option<ActiveThrottleRecord> {
    active_throttle_for_cwd_with_settings(config_dir, cwd, now_ms, raw_env, || {
        crate::tool_config::manvi_steward_settings_in(config_dir)
    })
}

fn active_throttle_for_cwd_with_settings(
    config_dir: &Path,
    cwd: &str,
    now_ms: u64,
    raw_env: Option<&str>,
    load_settings: impl FnOnce() -> ManviStewardSettings,
) -> Option<ActiveThrottleRecord> {
    let path = config_dir.join(STEWARD_SUBDIR).join(ACTIVE_FILE);
    let meta = fs::symlink_metadata(&path).ok()?;
    if meta.file_type().is_symlink() || !meta.is_file() || meta.len() > MAX_ACTIVE_RECORD_BYTES {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // SAFETY: `geteuid` has no preconditions.
        let uid = unsafe { libc::geteuid() };
        if meta.uid() != uid || (meta.mode() & 0o077) != 0 {
            return None;
        }
    }
    let mut opts = OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NOFOLLOW);
    }
    let mut raw = Vec::new();
    opts.open(&path)
        .ok()?
        .take(MAX_ACTIVE_RECORD_BYTES + 1)
        .read_to_end(&mut raw)
        .ok()?;
    if raw.len() as u64 > MAX_ACTIVE_RECORD_BYTES {
        return None;
    }
    let mut record: ActiveThrottleRecord = serde_json::from_slice(&raw).ok()?;
    if record.version != LEASE_VERSION || now_ms > record.expires_at_ms {
        return None;
    }
    // The steward never issues a lease longer than `lease_ttl_ms` allows or
    // dated in the future; one that is was not written by it.
    if record.issued_at_ms > now_ms.saturating_add(60_000)
        || record.expires_at_ms.saturating_sub(record.issued_at_ms) > MAX_LEASE_TTL_MS
    {
        return None;
    }
    // Fail-safe liveness check: if the GitPulse process that wrote `active.json`
    // exited or crashed (`SIGKILL`), stop throttling immediately.
    if record.writer_pid != std::process::id() {
        let alive = crate::sysload::process_start_us(record.writer_pid)
            .is_some_and(|live_start| live_start == record.writer_start_us);
        if !alive {
            return None;
        }
    }
    let (forced_enabled, _) = env_override_of(raw_env);
    if forced_enabled == Some(false) {
        return None;
    }
    // Only a repository the steward found trusted and covered: a command
    // outside every repository, or in one it did not name, is not paced.
    let common_dir = canonical_common_dir(cwd)?;
    if !record.repositories.contains(&common_dir) {
        return None;
    }
    let settings = load_settings();
    let effective = effective_policy_for(&settings, Some(common_dir.as_str()), forced_enabled);
    if !effective.enabled || !effective.slow_critical {
        return None;
    }
    record.pace_ms = record.pace_ms.clamp(100, 1_500);
    Some(record)
}

pub(crate) fn active_throttle_for_cwd(cwd: &str) -> Option<ActiveThrottleRecord> {
    let dir = crate::tool_config::default_config_dir()?;
    active_throttle_for_cwd_with_settings(
        &dir,
        cwd,
        now_epoch_ms(),
        std::env::var(STEWARD_ENV).ok().as_deref(),
        crate::tool_config::manvi_steward_settings,
    )
}

/* ── Testable Tick Dependencies & Orchestration ──────────────────────────── */

pub(crate) struct CriticalCleanRequest<'a> {
    pub trusted_cleanable_repos: &'a [String],
    pub trusted_anchor_repo: Option<&'a str>,
    pub clean_shared_caches: bool,
    pub retention_days: u32,
    pub disk_critical: bool,
    pub load: &'a HostLoadStatus,
    pub manvi: &'a ManviStatus,
}

pub(crate) type CloudCleanEvaluator<'a> = &'a dyn Fn(&str, &str, i64) -> Result<String, String>;

/// Re-verifies immediately before each deletion (after `work.assist.complete`
/// returns) that steward cleanup policy and repository trust still permit the
/// target (`None` = host-wide shared cache or session scratchpad;
/// `Some(checkout_root)` = repository or linked worktree).
pub(crate) type PreMutationPolicyCheck<'a> = &'a dyn Fn(Option<&str>) -> Result<(), String>;

/// Unions active/open repositories (`active_repos`, preserved first in order)
/// with approved checkouts from `repository_trust` and any repositories under
/// Global Cleaner `cleaner_roots`, deduplicating by canonical Git root and
/// capping at `MAX_CLEANUP_DISCOVERED_REPOS`.
pub(crate) fn merge_cleanup_candidate_repos(
    active_repos: Vec<String>,
    approved_checkouts: Vec<PathBuf>,
    cleaner_roots: &[String],
    cleaner_exclusions: &[String],
) -> Vec<String> {
    fn push_root(ordered: &mut Vec<String>, seen_canon: &mut BTreeSet<PathBuf>, candidate: &Path) {
        if ordered.len() >= MAX_CLEANUP_DISCOVERED_REPOS {
            return;
        }
        let Some(git_root) = find_git_root(candidate) else {
            return;
        };
        let canon = crate::engine::process::canonicalize_plain(&git_root)
            .unwrap_or_else(|_| git_root.clone());
        if seen_canon.insert(canon.clone()) {
            ordered.push(canon.to_string_lossy().into_owned());
        }
    }

    let mut ordered = Vec::new();
    let mut seen_canon = BTreeSet::new();
    for repo in &active_repos {
        push_root(&mut ordered, &mut seen_canon, Path::new(repo));
    }
    for checkout in &approved_checkouts {
        push_root(&mut ordered, &mut seen_canon, checkout);
    }
    if ordered.len() < MAX_CLEANUP_DISCOVERED_REPOS && !cleaner_roots.is_empty() {
        if let (Ok(roots), Ok(excludes)) = (
            crate::storage::hygiene::discovery::roots(cleaner_roots, false),
            crate::storage::hygiene::discovery::roots(cleaner_exclusions, true),
        ) {
            let cancel = AtomicBool::new(false);
            let disc = crate::storage::hygiene::discovery::discover(&roots, &excludes, &cancel);
            for repo in disc.repos {
                push_root(&mut ordered, &mut seen_canon, &repo);
            }
        }
    }
    ordered
}

/// A failed steward model call. `credential_rejected` is Manvi's
/// `E_PROVIDER_AUTH` code from an Anthropic route, read as data, never from
/// the message text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModelCallError {
    pub message: String,
    pub credential_rejected: bool,
}

impl From<String> for ModelCallError {
    fn from(message: String) -> Self {
        Self {
            message,
            credential_rejected: false,
        }
    }
}

impl From<&str> for ModelCallError {
    fn from(message: &str) -> Self {
        message.to_string().into()
    }
}

impl std::fmt::Display for ModelCallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// Reads one failed `work.assist.complete` answer. Only an Anthropic route's
/// `E_PROVIDER_AUTH` marks the key rejected: a local server's refusal is not
/// about the Claude key.
fn model_call_error(provider: &str, failure: ManviError) -> ModelCallError {
    ModelCallError {
        credential_rejected: provider == "anthropic"
            && failure.code == crate::ai::MANVI_PROVIDER_AUTH,
        message: format!("{}: {}", failure.code, failure.message),
    }
}

pub(crate) trait StewardDeps {
    fn now_ms(&self) -> u64;
    fn config_dir(&self) -> Option<PathBuf>;
    fn settings(&self) -> ManviStewardSettings;
    fn env_var(&self) -> Option<String>;
    fn manvi_probe(&self, disk_remedy: bool) -> ManviProbe;
    fn host_load(&self) -> HostLoadStatus;
    fn disk_remedy_due(&self) -> bool;
    fn candidate_repos(&self, load: &HostLoadStatus, explicit_repo: Option<&str>) -> Vec<String>;
    fn cleanup_candidate_repos(
        &self,
        load: &HostLoadStatus,
        explicit_repo: Option<&str>,
    ) -> Vec<String> {
        self.candidate_repos(load, explicit_repo)
    }
    fn repo_trusted(&self, repo_root: &str) -> bool;
    fn repo_common_dir(&self, path: &str) -> Option<String>;
    fn manvi_complete(
        &self,
        selection: &ModelSelection,
        system: &str,
        user: &str,
        max_tokens: i64,
    ) -> Result<String, ModelCallError>;
    // One call carries the whole directive envelope the bus and mailbox need.
    #[allow(clippy::too_many_arguments)]
    fn deliver_repo_directive(
        &self,
        repo_root: &str,
        msg_id: &str,
        model: &str,
        mode: &str,
        tier: &str,
        binding: &str,
        directive: &str,
    ) -> Result<u32, String>;
    fn throttle_candidates(&self) -> Result<Vec<AgentThrottleCandidate>, String>;
    fn set_process_throttle(&self, pid: i32, start_us: u64, enable: bool) -> Result<bool, String>;
    fn write_active_lease(&self, dir: &Path, record: &ActiveThrottleRecord) -> Result<(), String>;
    fn clear_active_lease(&self, dir: &Path) -> Result<(), String>;
    fn clear_owned_active_lease(&self, dir: &Path) -> Result<(), String> {
        clear_active_throttle_if_owned_or_stale(dir, self.now_ms())
    }
    fn clean_critical_storage(&self, req: &CriticalCleanRequest<'_>) -> CleanPassOutcome;
}

struct ResetRunning<'a>(&'a AtomicBool);
impl Drop for ResetRunning<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

struct LiveDeps<'a> {
    workbench: &'a WorkbenchState,
    watched_roots: Vec<String>,
}

impl LiveDeps<'_> {
    fn call_manvi_assist_checked(
        &self,
        selection: &ModelSelection,
        system: &str,
        user: &str,
        max_tokens: i64,
        reject_truncated: bool,
    ) -> Result<String, ModelCallError> {
        match self.workbench.assist_served(selection) {
            Ok(true) => {}
            Ok(false) => return Err(crate::ai::OLD_MANVI_MESSAGE.into()),
            Err(err) => {
                return Err(
                    format!("Manvi worker unavailable ({}): {}", err.code, err.message).into(),
                )
            }
        }
        let route = self.workbench.assist_route(selection.clone());
        let reply = (route.complete)(json!({
            "system": system,
            "user": user,
            "max_output_tokens": max_tokens,
        }))
        .map_err(|failure| model_call_error(route.provider, failure))?;
        if reply["ok"] != true {
            return Err("Manvi returned a non-ok completion response".into());
        }
        let provider = reply["provider"].as_str().unwrap_or_default();
        if provider != route.provider {
            return Err(format!(
                "Manvi answered from `{provider}` instead of expected `{}`",
                route.provider
            )
            .into());
        }
        if reject_truncated && reply["truncated"] == true {
            return Err(
                "Manvi completion hit max_output_tokens before finishing structured output".into(),
            );
        }
        Ok(reply["text"].as_str().unwrap_or_default().to_string())
    }
}

impl StewardDeps for LiveDeps<'_> {
    fn now_ms(&self) -> u64 {
        now_epoch_ms()
    }

    fn config_dir(&self) -> Option<PathBuf> {
        crate::tool_config::default_config_dir()
    }

    fn settings(&self) -> ManviStewardSettings {
        crate::tool_config::manvi_steward_settings()
    }

    fn env_var(&self) -> Option<String> {
        std::env::var(STEWARD_ENV).ok()
    }

    fn manvi_probe(&self, disk_remedy: bool) -> ManviProbe {
        probe_live_manvi_on(runtime(), disk_remedy)
    }

    fn host_load(&self) -> HostLoadStatus {
        crate::sysload::status()
    }

    fn disk_remedy_due(&self) -> bool {
        crate::sysload::disk_remedy_due()
    }

    fn candidate_repos(&self, load: &HostLoadStatus, explicit_repo: Option<&str>) -> Vec<String> {
        let mut roots = BTreeSet::new();
        if let Some(repo) = explicit_repo {
            if let Some(root) = find_git_root(Path::new(repo)) {
                roots.insert(root.to_string_lossy().into_owned());
            }
        }
        for watched in &self.watched_roots {
            if let Some(root) = find_git_root(Path::new(watched)) {
                roots.insert(root.to_string_lossy().into_owned());
            }
        }
        if let Some(procs) = &load.procs {
            for sess in &procs.sessions {
                if let Some(cwd) = &sess.cwd {
                    if let Some(root) = find_git_root(Path::new(cwd)) {
                        roots.insert(root.to_string_lossy().into_owned());
                    }
                }
            }
        }
        // Also include checkouts named in per-repository steward overrides so a
        // repository with a custom policy can still be inspected or serve as a
        // trusted anchor when no session is currently inside it.
        for common in self.settings().repositories.keys() {
            let common_path = Path::new(common);
            let candidate = if common_path.file_name() == Some(std::ffi::OsStr::new(".git")) {
                common_path.parent()
            } else {
                Some(common_path)
            };
            if let Some(dir) = candidate {
                if let Some(root) = find_git_root(dir) {
                    roots.insert(root.to_string_lossy().into_owned());
                }
            }
        }
        roots.into_iter().collect()
    }

    fn cleanup_candidate_repos(
        &self,
        load: &HostLoadStatus,
        explicit_repo: Option<&str>,
    ) -> Vec<String> {
        let active = self.candidate_repos(load, explicit_repo);
        let approved = crate::repository_trust::approved_checkouts();
        let (roots, exclusions) = crate::storage::hygiene::global::service()
            .and_then(|c| c.state())
            .map(|st| (st.config.roots, st.config.exclusions))
            .unwrap_or_default();
        merge_cleanup_candidate_repos(active, approved, &roots, &exclusions)
    }

    fn repo_trusted(&self, repo_root: &str) -> bool {
        crate::repository_trust::require(Path::new(repo_root)).is_ok()
    }

    fn repo_common_dir(&self, path: &str) -> Option<String> {
        canonical_common_dir(path)
    }

    fn manvi_complete(
        &self,
        selection: &ModelSelection,
        system: &str,
        user: &str,
        max_tokens: i64,
    ) -> Result<String, ModelCallError> {
        let raw = self.call_manvi_assist_checked(selection, system, user, max_tokens, false)?;
        let text = sanitize_directive(&raw);
        if text.is_empty() {
            return Err("Manvi returned an empty steward directive".into());
        }
        Ok(text)
    }

    fn deliver_repo_directive(
        &self,
        repo_root: &str,
        msg_id: &str,
        model: &str,
        mode: &str,
        tier: &str,
        binding: &str,
        directive: &str,
    ) -> Result<u32, String> {
        let mailbox = Mailbox::at_default().map_err(|e| e.to_string())?;
        deliver_directive_to_repo(
            repo_root, &mailbox, msg_id, model, mode, tier, binding, directive,
        )
    }

    fn throttle_candidates(&self) -> Result<Vec<AgentThrottleCandidate>, String> {
        crate::sysload::procscan::agent_throttle_candidates()
    }

    fn set_process_throttle(&self, pid: i32, start_us: u64, enable: bool) -> Result<bool, String> {
        crate::sysload::relief::steward_throttle(pid, start_us, enable)
    }

    fn write_active_lease(&self, dir: &Path, record: &ActiveThrottleRecord) -> Result<(), String> {
        write_active_throttle(dir, record)
    }

    fn clear_active_lease(&self, dir: &Path) -> Result<(), String> {
        clear_active_throttle(dir)
    }

    fn clean_critical_storage(&self, req: &CriticalCleanRequest<'_>) -> CleanPassOutcome {
        let policy_check = |checkout_scope: Option<&str>| -> Result<(), String> {
            let latest = self.settings();
            let (forced, _) = env_override_of(self.env_var().as_deref());
            if let Some(checkout_root) = checkout_scope {
                if !self.repo_trusted(checkout_root) {
                    return Err("Repository trust was revoked before cleanup executed.".into());
                }
                let common = self.repo_common_dir(checkout_root);
                let eff = effective_policy_for(&latest, common.as_deref(), forced);
                if !eff.enabled || !eff.clean_critical {
                    return Err(
                        "Steward critical cleanup was disabled for this repository before execution."
                            .into(),
                    );
                }
            } else {
                let global_eff = effective_policy_for(&latest, None, forced);
                if !global_eff.enabled || !global_eff.clean_critical {
                    return Err(
                        "Global steward critical cleanup was disabled before execution.".into(),
                    );
                }
            }
            Ok(())
        };
        let cloud_selection = (req.manvi.mode == "cloud")
            .then_some(req.manvi.typed.as_ref())
            .flatten();
        if let Some(sel) = cloud_selection {
            let eval = cleanup_evaluator(&req.manvi.model, |system, user, max_tokens| {
                self.call_manvi_assist_checked(sel, system, user, max_tokens, true)
            });
            run_critical_hygiene_pass_impl(
                req.trusted_cleanable_repos,
                req.trusted_anchor_repo,
                req.clean_shared_caches,
                req.clean_shared_caches,
                req.retention_days,
                req.disk_critical,
                Some(req.load),
                None,
                Some(&eval),
                &crate::storage::hygiene::agent::LiveSessions::observe,
                &policy_check,
            )
        } else {
            run_critical_hygiene_pass_impl(
                req.trusted_cleanable_repos,
                req.trusted_anchor_repo,
                req.clean_shared_caches,
                req.clean_shared_caches,
                req.retention_days,
                req.disk_critical,
                Some(req.load),
                None,
                None,
                &crate::storage::hygiene::agent::LiveSessions::observe,
                &policy_check,
            )
        }
    }
}

/// Posts a system-steward directive to `<git-common-dir>/gitpulse/lanes/messages.json`
/// under [`MessageOrigin::SystemSteward`] and deposits matching
/// [`EnvelopeProvenance::SystemSteward`] envelopes into active Claude/Antigravity
/// mailboxes (keyed by `msg_id` so `gitpulse_agent_inbox` and `peer-inbox`
/// deduplicate and cross-ack cleanly). Never touches idle or exited sessions.
#[allow(clippy::too_many_arguments)]
pub(crate) fn deliver_directive_to_repo(
    repo_root: &str,
    mailbox: &Mailbox,
    msg_id: &str,
    model: &str,
    mode: &str,
    tier: &str,
    binding: &str,
    directive: &str,
) -> Result<u32, String> {
    let peers = agent_bus::list_reconciled_peers(repo_root).map_err(|e| e.message().to_string())?;
    let active_peers: Vec<_> = peers
        .into_iter()
        .filter(|peer| {
            peer.liveness == "live" && !matches!(peer.record.turn_state.as_str(), "idle" | "exited")
        })
        .collect();
    if active_peers.is_empty() {
        return Ok(0);
    }

    let bus_msg = agent_bus::post_peer_message(
        repo_root,
        MessageOrigin::SystemSteward,
        agent_bus::STEWARD_SENDER,
        None,
        Some(agent_bus::STEWARD_THREAD_KEY),
        agent_bus::STEWARD_MESSAGE_KIND,
        directive,
        None,
        Some(msg_id),
        0,
        1,
    )
    .map_err(|e| e.message().to_string())?;

    let now = SystemTime::now();
    let mut recipients = 0u32;
    let mut seen_sessions = BTreeSet::new();
    let mut seen_bus_peers = BTreeSet::new();
    for peer in active_peers {
        if matches!(peer.record.provider.as_str(), "claude" | "agy") {
            if let Some(sid) = peer
                .record
                .provider_session_id
                .as_deref()
                .filter(|id| crate::workbench::terminal_command::is_canonical_uuid(id))
            {
                let sid_lower = sid.to_ascii_lowercase();
                if !seen_sessions.insert(sid_lower.clone()) {
                    continue;
                }
                let prov = EnvelopeProvenance::SystemSteward {
                    msg_id: msg_id.to_string(),
                    model: model.to_string(),
                    mode: mode.to_string(),
                    tier: tier.to_string(),
                    binding: binding.to_string(),
                    repo_root: repo_root.to_string(),
                    bus_seq: bus_msg.seq,
                };
                if mailbox
                    .deposit_with_provenance(&sid_lower, msg_id, directive, prov, now)
                    .is_ok()
                {
                    recipients = recipients.saturating_add(1);
                }
                continue;
            }
        }
        if seen_bus_peers.insert(peer.record.peer_id) {
            recipients = recipients.saturating_add(1);
        }
    }
    crate::mcp::a2a::notify_in_process_waiters();
    Ok(recipients)
}

pub(crate) fn sanitize_directive(raw: &str) -> String {
    let collapsed: String = raw
        .chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .collect();
    let trimmed = collapsed.trim();
    if trimmed.chars().count() <= MAX_DIRECTIVE_CHARS {
        trimmed.to_string()
    } else {
        let mut cut: String = trimmed.chars().take(MAX_DIRECTIVE_CHARS - 1).collect();
        cut.push('…');
        cut
    }
}

fn format_gib(bytes: Option<u64>) -> String {
    match bytes {
        Some(b) => format!("{:.1} GiB", b as f64 / (1u64 << 30) as f64),
        None => "unavailable".into(),
    }
}

fn telemetry_digest(
    load: &HostLoadStatus,
    disk_remedy: bool,
    throttled: u32,
    reclaimed_bytes: u64,
) -> String {
    let verdict = &load.governor.verdict;
    let snap = load.snapshot.as_ref();
    let mem_used = snap
        .and_then(|s| s.host.memory.value())
        .map(|m| m.used_bytes);
    let mem_total = snap.and_then(|s| s.host.memsize.value());
    let swap_used = snap.and_then(|s| s.host.swap_used.value());
    let free_disk = snap.and_then(|s| s.host.free_disk.value());
    let load_1m = snap
        .and_then(|s| s.host.load.value())
        .map(|l| format!("{:.1}", l[0]))
        .unwrap_or_else(|| "unavailable".into());
    let cores = snap
        .and_then(|s| s.host.cores.value())
        .map(|c| c.logical.to_string())
        .unwrap_or_else(|| "unavailable".into());
    let sessions_total = load
        .procs
        .as_ref()
        .map(|p| p.sessions.len())
        .or_else(|| {
            snap.map(|s| {
                let claude = s
                    .claude
                    .value()
                    .map_or(0usize, |c| (c.launched + c.observed) as usize);
                let codex = s
                    .codex
                    .value()
                    .map_or(0usize, |c| (c.launched + c.observed) as usize);
                claude + codex
            })
        })
        .unwrap_or(0);
    format!(
        "tier: {}\nbinding: {} ({})\nadmit_launches: {}\ndisk_remedy_due: {}\n\
         memory_used: {} of {}\nswap_used: {}\nload_1m: {} across {} logical cores\n\
         free_disk: {}\nlive_agent_sessions: {}\n\
         actions_applied_by_gitpulse: throttled {} agent process(es), reclaimed {} of cached build output",
        tier_name(verdict.tier),
        binding_key(verdict.binding),
        verdict
            .binding
            .map_or("within limits", Binding::cause_phrase),
        verdict.admit,
        disk_remedy,
        format_gib(mem_used),
        format_gib(mem_total),
        format_gib(swap_used),
        load_1m,
        cores,
        format_gib(free_disk),
        sessions_total,
        throttled,
        format_gib(Some(reclaimed_bytes)),
    )
}

fn deterministic_directive(
    tier: Tier,
    binding: Option<Binding>,
    disk_remedy: bool,
    free_disk: Option<u64>,
    throttled: u32,
    reclaimed_bytes: u64,
) -> String {
    let cause = binding.map_or("host load", Binding::cause_phrase);
    let mut parts = vec![format!("Host load is {} ({cause}).", tier_name(tier))];
    if tier >= Tier::Severe {
        parts.push(
            "Pause heavy builds and test suites until pressure drops; if a check must run, cap concurrency (CARGO_BUILD_JOBS=2 -- --test-threads=2, GOMAXPROCS=4 go test -p 2)."
                .into(),
        );
    } else {
        parts.push(
            "Run at most one capped build or test command at a time (CARGO_BUILD_JOBS=2 -- --test-threads=2, GOMAXPROCS=4 go test -p 2)."
                .into(),
        );
    }
    if throttled > 0 {
        parts.push(format!(
            "GitPulse placed {throttled} active agent child process(es) into background scheduler/I/O throttle and enabled command pacing until critical load clears."
        ));
    }
    if disk_remedy || matches!(binding, Some(Binding::LowDisk | Binding::DiskFilling)) {
        parts.push(format!(
            "Free disk space is critically low ({}; GitPulse reclaimed {} from eligible caches). Remove throwaway targets under .gitpulse/scratch and avoid cargo llvm-cov or cold multi-worktree builds.",
            format_gib(free_disk),
            format_gib(Some(reclaimed_bytes)),
        ));
    }
    parts.join(" ")
}

/// Runs hygiene cleanup for critically low storage using ONLY the existing
/// `storage::hygiene` engines (`prepare`/`execute` for repository build folders
/// and shared caches + `agent::{scan_sessions, scan_repository,
/// scan_claude_targets, remove_judged}` for leftover Cargo scratch builds in
/// finished system-temp session scratchpads and trusted repositories). Never
/// enables or mutates saved Global Cleaner schedules, and skips overlapping
/// runs if the Global Cleaner is currently active.
#[cfg(all(test, unix))]
pub(crate) fn run_critical_hygiene_pass(
    trusted_cleanable_repos: &[String],
    clean_shared_caches: bool,
    retention_days: u32,
) -> CleanPassOutcome {
    run_critical_hygiene_pass_with(
        trusted_cleanable_repos,
        trusted_cleanable_repos.first().map(String::as_str),
        clean_shared_caches,
        retention_days,
        None,
    )
}

fn default_session_tmp_roots() -> Vec<PathBuf> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    let mut push = |candidate: PathBuf| {
        let resolved = fs::canonicalize(&candidate).unwrap_or(candidate);
        if seen.insert(resolved.clone()) {
            out.push(resolved);
        }
    };
    push(PathBuf::from("/tmp"));
    push(std::env::temp_dir());
    if let Some(custom) = std::env::var_os("CLAUDE_CODE_TMPDIR") {
        let path = PathBuf::from(custom);
        if path.is_absolute() {
            push(path);
        }
    }
    out
}

#[derive(Debug, Clone)]
enum HygieneCandidateAction {
    HygieneTarget {
        repo_root: String,
        target: String,
        display_target: String,
        retention_days: u32,
        is_linked_worktree: bool,
        worktree_branch: Option<String>,
        artifact_rel_path: Option<String>,
    },
    AgentScratch {
        candidate: crate::storage::hygiene::global::CleanerCandidate,
        target_label: String,
        is_session_scratchpad: bool,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct HygieneCandidate {
    pub id: String,
    pub category: &'static str,
    pub repo_or_scope: String,
    pub target_label: String,
    pub provider: String,
    pub bytes: u64,
    pub private_bytes: Option<u64>,
    pub age_secs: Option<u64>,
    pub recent_below_retention: bool,
    pub checkout_has_live_agent: bool,
    pub evidence: String,
    action: HygieneCandidateAction,
}

impl HygieneCandidate {
    fn reclaimable_bytes(&self) -> u64 {
        self.private_bytes.unwrap_or(self.bytes)
    }

    fn is_shared_cache(&self) -> bool {
        self.category == "shared_cache"
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub(crate) struct CloudCleanSelection {
    pub selected_ids: Vec<String>,
    #[serde(default)]
    pub rationale: String,
}

pub(crate) fn parse_cloud_clean_selection(raw: &str) -> Result<CloudCleanSelection, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("empty response".into());
    }
    if let Ok(parsed) = serde_json::from_str::<CloudCleanSelection>(trimmed) {
        return Ok(parsed);
    }
    // Check any markdown fenced code block (` ```json ... ``` `) even when
    // surrounded by introductory or trailing prose.
    let mut scan = trimmed;
    let mut fence_attempts = 0usize;
    while let Some(open) = scan.find("```") {
        fence_attempts += 1;
        if fence_attempts > 8 {
            break;
        }
        let after_open = &scan[open + 3..];
        let Some(nl) = after_open.find('\n') else {
            break;
        };
        let body_start = &after_open[nl + 1..];
        let Some(close) = body_start.find("```") else {
            break;
        };
        let fenced_body = body_start[..close].trim();
        if let Ok(parsed) = serde_json::from_str::<CloudCleanSelection>(fenced_body) {
            return Ok(parsed);
        }
        scan = &body_start[close + 3..];
    }
    // Stream-scan balanced JSON objects starting at each `{` (bounded to 64
    // attempts) so prose containing `{...}` before or after the selection object
    // never merges into an invalid slice.
    for (idx, _) in trimmed.match_indices('{').take(64) {
        let mut iter =
            serde_json::Deserializer::from_str(&trimmed[idx..]).into_iter::<CloudCleanSelection>();
        if let Some(Ok(parsed)) = iter.next() {
            return Ok(parsed);
        }
    }
    Err("response is not a JSON object with `selected_ids`".into())
}

fn build_cloud_clean_prompt(
    load: Option<&HostLoadStatus>,
    disk_critical: bool,
    retention_days: u32,
    candidates: &[HygieneCandidate],
    total_candidates: usize,
) -> String {
    let snap = load.and_then(|l| l.snapshot.as_ref());
    let verdict = load.map(|l| &l.governor.verdict);
    let free_disk = snap.and_then(|s| s.host.free_disk.value());
    let swap_used = snap.and_then(|s| s.host.swap_used.value());
    let mem_used = snap
        .and_then(|s| s.host.memory.value())
        .map(|m| m.used_bytes);
    let items: Vec<serde_json::Value> = candidates
        .iter()
        .map(|c| {
            json!({
                "id": c.id,
                "category": c.category,
                "scope": c.repo_or_scope,
                "target": c.target_label,
                "provider": c.provider,
                "bytes": c.bytes,
                "private_bytes": c.private_bytes,
                "human_size": format_gib(Some(c.reclaimable_bytes())),
                "age_hours": c.age_secs.map(|s| s / 3_600),
                "recent_below_retention": c.recent_below_retention,
                "checkout_has_live_agent": c.checkout_has_live_agent,
                "note": c.evidence,
            })
        })
        .collect();
    json!({
        "host": {
            "tier": verdict.map_or("severe", |v| tier_name(v.tier)),
            "binding": verdict.map_or("low_disk", |v| binding_key(v.binding)),
            "disk_critical": disk_critical,
            "free_disk_bytes": free_disk,
            "free_disk_human": format_gib(free_disk),
            "swap_used_bytes": swap_used,
            "memory_used_bytes": mem_used,
            "clean_retention_days": retention_days,
        },
        "inventory": {
            "shown": candidates.len(),
            "total": total_candidates,
            "truncated": total_candidates > candidates.len(),
        },
        "candidates": items,
    })
    .to_string()
}

fn live_agent_cwds_from(load: Option<&HostLoadStatus>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(procs) = load.and_then(|l| l.procs.as_ref()) {
        for sess in &procs.sessions {
            if let Some(cwd) = &sess.cwd {
                let p = Path::new(cwd);
                out.push(
                    crate::engine::process::canonicalize_plain(p)
                        .unwrap_or_else(|_| p.to_path_buf()),
                );
            }
        }
    }
    out
}

fn fresh_cleaner_exclusions(extra: &[PathBuf]) -> Vec<PathBuf> {
    let mut exclusions = extra.to_vec();
    if let Ok(cleaner) = crate::storage::hygiene::global::service() {
        if let Ok(state) = cleaner.state() {
            for excl in &state.config.exclusions {
                let resolved = crate::engine::process::canonicalize_plain(Path::new(excl))
                    .unwrap_or_else(|_| PathBuf::from(excl));
                if !exclusions.contains(&resolved) {
                    exclusions.push(resolved);
                }
            }
        }
    }
    exclusions
}

fn path_matches_exclusions(candidate: &Path, exclusions: &[PathBuf]) -> bool {
    let resolved = crate::engine::process::canonicalize_plain(candidate)
        .unwrap_or_else(|_| candidate.to_path_buf());
    exclusions.iter().any(|excl| resolved.starts_with(excl))
}

#[cfg(all(test, unix))]
pub(crate) fn run_critical_hygiene_pass_with(
    trusted_cleanable_repos: &[String],
    trusted_anchor_repo: Option<&str>,
    clean_shared_caches: bool,
    retention_days: u32,
    session_tmp_root: Option<&Path>,
) -> CleanPassOutcome {
    run_critical_hygiene_pass_full(
        trusted_cleanable_repos,
        trusted_anchor_repo,
        clean_shared_caches,
        retention_days,
        true,
        None,
        session_tmp_root,
        None,
    )
}

#[cfg(all(test, unix))]
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_critical_hygiene_pass_full(
    trusted_cleanable_repos: &[String],
    trusted_anchor_repo: Option<&str>,
    clean_shared_caches: bool,
    retention_days: u32,
    disk_critical: bool,
    load: Option<&HostLoadStatus>,
    session_tmp_root: Option<&Path>,
    cloud_evaluator: Option<CloudCleanEvaluator<'_>>,
) -> CleanPassOutcome {
    run_critical_hygiene_pass_impl(
        trusted_cleanable_repos,
        trusted_anchor_repo,
        clean_shared_caches,
        clean_shared_caches,
        retention_days,
        disk_critical,
        load,
        session_tmp_root,
        cloud_evaluator,
        &crate::storage::hygiene::agent::LiveSessions::observe,
        &|_| Ok(()),
    )
}

#[allow(clippy::too_many_arguments)]
fn run_critical_hygiene_pass_impl(
    trusted_cleanable_repos: &[String],
    trusted_anchor_repo: Option<&str>,
    clean_session_scratchpads: bool,
    clean_shared_caches: bool,
    retention_days: u32,
    disk_critical: bool,
    load: Option<&HostLoadStatus>,
    session_tmp_root: Option<&Path>,
    cloud_evaluator: Option<CloudCleanEvaluator<'_>>,
    observe_live: &dyn Fn() -> crate::storage::hygiene::agent::LiveSessions,
    policy_still_allows: PreMutationPolicyCheck<'_>,
) -> CleanPassOutcome {
    let mut out = CleanPassOutcome::default();
    let mut global_exclusions: Vec<PathBuf> = Vec::new();

    if let Ok(cleaner) = crate::storage::hygiene::global::service() {
        if let Ok(state) = cleaner.state() {
            if state.running {
                out.warnings.push(
                    "Global build cleaner is currently running; skipped overlapping steward cleanup pass."
                        .into(),
                );
                out.deferred_or_busy = true;
                return out;
            }
            for excl in &state.config.exclusions {
                if let Ok(canon) = crate::engine::process::canonicalize_plain(Path::new(excl)) {
                    global_exclusions.push(canon);
                } else {
                    global_exclusions.push(PathBuf::from(excl));
                }
            }
        }
    }

    let is_excluded = |candidate: &Path| path_matches_exclusions(candidate, &global_exclusions);

    let use_cloud = cloud_evaluator.is_some();
    let scratch_scan_retention = if use_cloud && disk_critical {
        0
    } else {
        retention_days
    };
    let now_sys = SystemTime::now();
    let now_secs = now_sys
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let live_cwds = live_agent_cwds_from(load);
    let own_removals = crate::storage::hygiene::agent::SelfRemovals::default();
    let mut seen_scratch_checkouts = BTreeSet::new();
    let mut seen_worktree_roots = BTreeSet::new();
    let mut candidates: Vec<HygieneCandidate> = Vec::new();
    let mut skipped_items: Vec<StewardCleanItem> = Vec::new();
    let stage1_start = Instant::now();
    let mut inventory_budget_hit = false;

    // Stage 1a: Collect pre-verified idle repository & linked-worktree build
    // directories plus in-repository/worktree agent scratch Cargo targets.
    for (repo_idx, repo_root) in trusted_cleanable_repos.iter().enumerate() {
        if repo_idx > 0 && stage1_start.elapsed() >= STAGE1_INVENTORY_BUDGET {
            inventory_budget_hit = true;
            out.warnings.push(format!(
                "Candidate inventory stopped at its {}s budget after examining {repo_idx} of {} trusted repositories.",
                STAGE1_INVENTORY_BUDGET.as_secs(),
                trusted_cleanable_repos.len()
            ));
            break;
        }
        collect_checkout_artifact_candidates(
            repo_root,
            false,
            None,
            retention_days,
            now_sys,
            now_secs,
            &live_cwds,
            &is_excluded,
            &mut candidates,
            &mut skipped_items,
            &mut out,
        );

        let mut scratch_checkouts = Vec::new();
        if let Ok(canon_repo) = crate::engine::process::canonicalize_plain(Path::new(repo_root)) {
            if seen_scratch_checkouts.insert(canon_repo.clone()) {
                scratch_checkouts.push(canon_repo);
            }
        }
        match crate::storage::hygiene::global::listed_worktrees(repo_root) {
            Ok(infos) => {
                for info in infos
                    .iter()
                    .filter(|i| !i.is_main && !i.is_bare && !i.is_prunable)
                {
                    let Ok(wt_path) =
                        crate::engine::process::canonicalize_plain(Path::new(&info.path))
                    else {
                        continue;
                    };
                    if is_excluded(&wt_path) || crate::repository_trust::require(&wt_path).is_err()
                    {
                        continue;
                    }
                    if seen_scratch_checkouts.insert(wt_path.clone()) {
                        scratch_checkouts.push(wt_path.clone());
                    }
                    // Also inspect standard Cargo `target` / `.turbo` caches inside
                    // unlocked, idle linked worktrees (`worktree:*`). `quiet_worktree`
                    // inside `collect_checkout_artifact_candidates` verifies zero
                    // using processes, clean status, and clear task leases.
                    if !info.is_locked && seen_worktree_roots.insert(wt_path.clone()) {
                        let wt_str = wt_path.to_string_lossy().into_owned();
                        collect_checkout_artifact_candidates(
                            &wt_str,
                            true,
                            info.branch.as_deref(),
                            retention_days,
                            now_sys,
                            now_secs,
                            &live_cwds,
                            &is_excluded,
                            &mut candidates,
                            &mut skipped_items,
                            &mut out,
                        );
                    }
                }
            }
            Err(err) => {
                if err.origin() == Some(IpcErrorKind::DeferredUnderLoad) {
                    out.deferred_or_busy = true;
                }
                if out.warnings.len() < 16 {
                    out.warnings.push(format!(
                        "{repo_root}: git worktree list unavailable ({}); using verified metadata checkouts",
                        err.message()
                    ));
                }
                if let Ok(pair) = crate::engine::worktree::admit_worktree_pair(repo_root, repo_root)
                {
                    for wt_path in pair.best_effort_registered_checkouts() {
                        if !is_excluded(&wt_path)
                            && crate::repository_trust::require(&wt_path).is_ok()
                            && seen_scratch_checkouts.insert(wt_path.clone())
                        {
                            scratch_checkouts.push(wt_path);
                        }
                    }
                }
            }
        }

        collect_repo_agent_scratch_candidates(
            &scratch_checkouts,
            scratch_scan_retention,
            retention_days,
            now_secs,
            use_cloud,
            &live_cwds,
            observe_live,
            &is_excluded,
            &mut candidates,
            &mut skipped_items,
            &mut out,
        );
    }

    // Stage 1b: Host-wide finished-session scratchpads and shared caches.
    if clean_session_scratchpads {
        let tmp_roots = match session_tmp_root {
            Some(explicit) => vec![explicit.to_path_buf()],
            None => default_session_tmp_roots(),
        };
        for tmp_root in &tmp_roots {
            collect_session_scratch_candidates(
                tmp_root,
                scratch_scan_retention,
                retention_days,
                now_secs,
                use_cloud,
                observe_live,
                &is_excluded,
                &mut candidates,
                &mut out,
            );
        }
    }

    if clean_shared_caches {
        let anchor = trusted_cleanable_repos
            .first()
            .map(String::as_str)
            .or(trusted_anchor_repo);
        if let Some(anchor_repo) = anchor {
            match crate::engine::process::with_disk_relief(crate::storage::hygiene::cache_inventory)
            {
                Ok(inv) => {
                    for entry in inv.entries {
                        if entry
                            .error
                            .as_deref()
                            .is_some_and(crate::engine::process::is_deferred_under_load)
                        {
                            out.deferred_or_busy = true;
                        }
                        // Profile backups are the only way back from a schema
                        // upgrade: a user-confirmed action removes them, never this run.
                        if entry.confirm_only {
                            continue;
                        }
                        let Some(bytes) = entry.bytes else {
                            continue;
                        };
                        let Some(action) = entry.action else {
                            continue;
                        };
                        if bytes == 0 || (!use_cloud && bytes < MIN_SHARED_CACHE_CLEAN_BYTES) {
                            continue;
                        }
                        let target = format!("cache:{}", entry.id);
                        candidates.push(HygieneCandidate {
                            id: String::new(),
                            category: "shared_cache",
                            repo_or_scope: "shared-cache".into(),
                            target_label: target.clone(),
                            provider: entry.label,
                            bytes,
                            private_bytes: None,
                            age_secs: None,
                            recent_below_retention: false,
                            checkout_has_live_agent: false,
                            evidence: format!("{action}. {}", entry.note),
                            action: HygieneCandidateAction::HygieneTarget {
                                repo_root: anchor_repo.to_string(),
                                target: target.clone(),
                                display_target: target,
                                retention_days,
                                is_linked_worktree: false,
                                worktree_branch: None,
                                artifact_rel_path: None,
                            },
                        });
                    }
                }
                Err(err) => {
                    if err.origin() == Some(IpcErrorKind::DeferredUnderLoad) {
                        out.deferred_or_busy = true;
                    }
                    out.warnings
                        .push(format!("Shared-cache inventory skipped: {}", err.message()));
                }
            }
        } else {
            out.deferred_or_busy = true;
            out.warnings.push(
                "Shared-cache pruning skipped: no trusted repository with cleanup switched on to run it under."
                    .into(),
            );
        }
    }

    // Sort largest reclaimable bytes first and assign deterministic opaque IDs (`c1`..).
    candidates.sort_by(|a, b| {
        b.reclaimable_bytes()
            .cmp(&a.reclaimable_bytes())
            .then_with(|| a.target_label.cmp(&b.target_label))
            .then_with(|| a.repo_or_scope.cmp(&b.repo_or_scope))
    });
    for (idx, cand) in candidates.iter_mut().enumerate() {
        cand.id = format!("c{}", idx + 1);
    }

    // Stage 2: Select candidates (via Manvi cloud when configured, with safe
    // age-gated fallback) and execute through the canonical TOCTOU-checked engines.
    let total_candidates = candidates.len();
    let offered_len = total_candidates.min(MAX_CLOUD_CANDIDATES);
    let offered = &candidates[..offered_len];
    if total_candidates > offered_len {
        out.warnings.push(format!(
            "Evaluated top {offered_len} of {total_candidates} idle cleanup candidates by reclaimable size; remaining candidates stay queued for a later check."
        ));
    }
    let static_indices = || -> Vec<usize> {
        offered
            .iter()
            .enumerate()
            .filter(|(_, c)| {
                !c.recent_below_retention
                    && (!c.is_shared_cache() || c.bytes >= MIN_SHARED_CACHE_CLEAN_BYTES)
            })
            .map(|(i, _)| i)
            .collect()
    };

    let (chosen_indices, allow_recent_scratch, cloud_selected, cloud_rationale) = match (
        cloud_evaluator,
        !offered.is_empty(),
    ) {
        (Some(eval), true) => {
            let prompt = build_cloud_clean_prompt(
                load,
                disk_critical,
                retention_days,
                offered,
                total_candidates,
            );
            match eval(
                STEWARD_CLEAN_SYSTEM_PROMPT,
                &prompt,
                MAX_CLEAN_SELECT_TOKENS,
            ) {
                Ok(raw_reply) => match parse_cloud_clean_selection(&raw_reply) {
                    Ok(sel) => {
                        let id_to_idx: HashMap<&str, usize> = offered
                            .iter()
                            .enumerate()
                            .map(|(idx, c)| (c.id.as_str(), idx))
                            .collect();
                        let mut seen_ids = BTreeSet::new();
                        let mut indices = Vec::new();
                        let mut unknown = 0usize;
                        for raw_id in &sel.selected_ids {
                            let clean_id = raw_id.trim();
                            if clean_id.is_empty() || !seen_ids.insert(clean_id.to_string()) {
                                continue;
                            }
                            if let Some(&idx) = id_to_idx.get(clean_id) {
                                indices.push(idx);
                            } else {
                                unknown += 1;
                            }
                        }
                        if unknown > 0 {
                            out.warnings.push(format!(
                                    "Ignored {unknown} unrecognized candidate id(s) in Manvi cloud cleanup selection."
                                ));
                        }
                        let clean_note = sanitize_directive(&sel.rationale);
                        if indices.is_empty() {
                            let detail = if clean_note.is_empty() {
                                "Preserved by Manvi cloud after evaluating candidate inventory."
                                    .to_string()
                            } else {
                                format!("Preserved by Manvi cloud: {clean_note}")
                            };
                            for cand in offered.iter().take(8) {
                                skipped_items.push(StewardCleanItem {
                                    repo_path: cand.repo_or_scope.clone(),
                                    target: cand.target_label.clone(),
                                    status: "skipped".into(),
                                    measured_bytes: cand.bytes,
                                    bytes_after: None,
                                    detail: detail.clone(),
                                });
                            }
                        }
                        (indices, disk_critical, true, clean_note)
                    }
                    Err(parse_err) => {
                        if disk_critical {
                            out.warnings.push(format!(
                                    "Manvi cloud cleanup selection couldn't be parsed ({parse_err}); fell back to age-gated cleanup (>= {retention_days} days)."
                                ));
                            (static_indices(), false, false, String::new())
                        } else {
                            out.warnings.push(format!(
                                    "Manvi cloud cleanup selection couldn't be parsed ({parse_err}); skipped cleanup because disk space is not critical."
                                ));
                            (Vec::new(), false, false, String::new())
                        }
                    }
                },
                Err(call_err) => {
                    if disk_critical {
                        out.warnings.push(format!(
                                "Manvi cloud cleanup evaluation failed ({call_err}); fell back to age-gated cleanup (>= {retention_days} days)."
                            ));
                        (static_indices(), false, false, String::new())
                    } else {
                        out.deferred_or_busy = true;
                        out.warnings.push(format!(
                                "Manvi cloud cleanup evaluation failed ({call_err}); skipped cleanup because disk space is not critical."
                            ));
                        (Vec::new(), false, false, String::new())
                    }
                }
            }
        }
        _ => (static_indices(), false, false, String::new()),
    };

    if chosen_indices.len() > MAX_EXECUTED_CLEAN_ITEMS {
        out.warnings.push(format!(
            "Executed first {MAX_EXECUTED_CLEAN_ITEMS} of {} selected cleanup candidates; remaining items stay queued for a later check.",
            chosen_indices.len()
        ));
    }

    let scratch_remove_retention = if allow_recent_scratch {
        0
    } else {
        retention_days
    };
    let mut scratch_reclaimed = 0u64;
    for idx in chosen_indices.into_iter().take(MAX_EXECUTED_CLEAN_ITEMS) {
        let cand = &offered[idx];
        match &cand.action {
            HygieneCandidateAction::HygieneTarget {
                repo_root,
                target,
                display_target,
                retention_days: target_retention,
                is_linked_worktree,
                worktree_branch,
                artifact_rel_path,
            } => {
                if !execute_single_hygiene_target(
                    repo_root,
                    target,
                    display_target,
                    *target_retention,
                    *is_linked_worktree,
                    worktree_branch.as_deref(),
                    artifact_rel_path.as_deref(),
                    cloud_selected,
                    &cloud_rationale,
                    &global_exclusions,
                    policy_still_allows,
                    &mut out,
                ) {
                    break;
                }
            }
            HygieneCandidateAction::AgentScratch {
                candidate,
                target_label,
                is_session_scratchpad,
            } => {
                if scratch_reclaimed >= MAX_STEWARD_AGENT_SCRATCH_BYTES {
                    continue;
                }
                if !execute_agent_scratch_candidate(
                    candidate,
                    target_label,
                    *is_session_scratchpad,
                    scratch_remove_retention,
                    cloud_selected,
                    &cloud_rationale,
                    observe_live,
                    &own_removals,
                    &global_exclusions,
                    policy_still_allows,
                    &mut scratch_reclaimed,
                    &mut out,
                ) {
                    break;
                }
            }
        }
    }

    if inventory_budget_hit && out.reclaimed_bytes == 0 {
        out.deferred_or_busy = true;
    }

    for skipped in skipped_items {
        if out.items.len() >= 32 {
            break;
        }
        out.items.push(skipped);
    }

    out
}

#[allow(clippy::too_many_arguments)]
fn collect_checkout_artifact_candidates(
    checkout_root: &str,
    is_linked_worktree: bool,
    worktree_branch: Option<&str>,
    retention_days: u32,
    now_sys: SystemTime,
    now_secs: u64,
    live_cwds: &[PathBuf],
    is_excluded: &dyn Fn(&Path) -> bool,
    candidates: &mut Vec<HygieneCandidate>,
    skipped_items: &mut Vec<StewardCleanItem>,
    out: &mut CleanPassOutcome,
) {
    if is_linked_worktree {
        if let Err(err) = crate::engine::process::with_disk_relief(|| {
            crate::storage::hygiene::global::quiet_worktree(checkout_root, worktree_branch)
        }) {
            if err.origin() == Some(IpcErrorKind::DeferredUnderLoad)
                || err.message().contains("could not be read")
            {
                out.deferred_or_busy = true;
            }
            return;
        }
    }
    let report = match crate::engine::process::with_disk_relief(|| {
        crate::storage::scan_storage(checkout_root)
    }) {
        Ok(r) => r,
        Err(err) => {
            if err.origin() == Some(IpcErrorKind::DeferredUnderLoad) {
                out.deferred_or_busy = true;
            }
            if !is_linked_worktree {
                out.warnings.push(format!(
                    "{checkout_root}: storage scan skipped ({})",
                    err.message()
                ));
            }
            return;
        }
    };
    if report.artifacts.iter().any(|a| a.checks_unexamined) {
        out.deferred_or_busy = true;
        if !is_linked_worktree {
            out.warnings.push(format!(
                "{checkout_root}: git ignore/tracked checks were deferred under load; artifacts left untouched"
            ));
        }
    }
    let canon_checkout = crate::engine::process::canonicalize_plain(Path::new(checkout_root))
        .unwrap_or_else(|_| PathBuf::from(checkout_root));
    let checkout_has_live_agent = live_cwds.iter().any(|cwd| cwd.starts_with(&canon_checkout));
    let cancel = AtomicBool::new(false);

    for artifact in report.artifacts {
        if !artifact.scan_complete
            || artifact.tracked_files > 0
            || artifact.unignored
            || artifact.checks_unexamined
            || artifact.bytes == 0
        {
            continue;
        }
        if is_linked_worktree {
            let leaf = Path::new(&artifact.path)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default();
            if !crate::engine::cow_clone::STANDARD_CACHE_DIRS.contains(&leaf) {
                continue;
            }
        }
        let full_artifact = Path::new(checkout_root).join(&artifact.path);
        if is_excluded(&full_artifact) {
            continue;
        }
        let Ok(provider) = crate::storage::hygiene::providers::local_provider(
            Path::new(checkout_root),
            &artifact.path,
        ) else {
            continue;
        };
        let display_target = if is_linked_worktree {
            format!("worktree:{}", artifact.path)
        } else {
            format!("local:{}", artifact.path)
        };
        if !is_linked_worktree {
            if let Err(err) = crate::engine::process::with_disk_relief(|| {
                crate::storage::hygiene::global::quiet_project(checkout_root, &artifact.path)
            }) {
                if err.origin() == Some(IpcErrorKind::DeferredUnderLoad) {
                    out.deferred_or_busy = true;
                }
                skipped_items.push(StewardCleanItem {
                    repo_path: checkout_root.to_string(),
                    target: display_target,
                    status: "skipped".into(),
                    measured_bytes: artifact.bytes,
                    bytes_after: None,
                    detail: err.message().to_string(),
                });
                continue;
            }
        }
        let snap = match crate::storage::hygiene::tree::snapshot(&full_artifact, true, &cancel) {
            Ok(s) => s,
            Err(why) => {
                skipped_items.push(StewardCleanItem {
                    repo_path: checkout_root.to_string(),
                    target: display_target,
                    status: "skipped".into(),
                    measured_bytes: artifact.bytes,
                    bytes_after: None,
                    detail: why,
                });
                continue;
            }
        };
        if let Err(why) = crate::storage::hygiene::tree::require_age(&snap, retention_days, now_sys)
        {
            skipped_items.push(StewardCleanItem {
                repo_path: checkout_root.to_string(),
                target: display_target,
                status: "skipped".into(),
                measured_bytes: artifact.bytes,
                bytes_after: None,
                detail: why,
            });
            continue;
        }
        if let Err(err) = crate::engine::process::with_disk_relief(|| {
            crate::storage::hygiene::open_files_under(&full_artifact)
        }) {
            if err.origin() == Some(IpcErrorKind::DeferredUnderLoad) {
                out.deferred_or_busy = true;
            }
            skipped_items.push(StewardCleanItem {
                repo_path: checkout_root.to_string(),
                target: display_target,
                status: "skipped".into(),
                measured_bytes: artifact.bytes,
                bytes_after: None,
                detail: err.message().to_string(),
            });
            continue;
        }
        let age_secs = u64::try_from(snap.newest / 1_000_000_000)
            .ok()
            .map(|newest| now_secs.saturating_sub(newest));
        let category = if is_linked_worktree {
            "worktree_cargo_target"
        } else if provider == "Rust / Cargo" {
            "cargo_target"
        } else {
            "repo_build_cache"
        };
        let rel_path = artifact.path.clone();
        candidates.push(HygieneCandidate {
            id: String::new(),
            category,
            repo_or_scope: checkout_root.to_string(),
            target_label: display_target.clone(),
            provider: provider.to_string(),
            bytes: artifact.bytes,
            private_bytes: artifact.private_bytes,
            age_secs,
            recent_below_retention: false,
            checkout_has_live_agent,
            evidence: "git-ignored, untracked, no open files, and clean tree/lease checks".into(),
            action: HygieneCandidateAction::HygieneTarget {
                repo_root: checkout_root.to_string(),
                target: format!("local:{rel_path}"),
                display_target,
                retention_days,
                is_linked_worktree,
                worktree_branch: worktree_branch.map(str::to_owned),
                artifact_rel_path: Some(rel_path),
            },
        });
    }
}

fn note_agent_scan_outcome(
    scan: &crate::storage::hygiene::agent::Scan,
    out: &mut CleanPassOutcome,
) {
    if !scan.complete || scan.unexamined > 0 {
        out.deferred_or_busy = true;
    }
    for issue in &scan.issues {
        if out.warnings.len() < 16 && !out.warnings.contains(issue) {
            out.warnings.push(issue.clone());
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn collect_session_scratch_candidates(
    session_tmp_root: &Path,
    scan_retention_days: u32,
    configured_retention_days: u32,
    now_secs: u64,
    measure_private: bool,
    observe_live: &dyn Fn() -> crate::storage::hygiene::agent::LiveSessions,
    is_excluded: &dyn Fn(&Path) -> bool,
    candidates: &mut Vec<HygieneCandidate>,
    out: &mut CleanPassOutcome,
) {
    let scan_live = observe_live();
    let cancel = AtomicBool::new(false);
    let in_use = |dir: &Path| crate::sysload::procscan::dir_use(dir);
    let scan_ctx = crate::storage::hygiene::agent::Context {
        now: now_secs,
        retention_days: scan_retention_days,
        live: &scan_live,
        use_of: &in_use,
        cancel: &cancel,
        deadline: Instant::now() + Duration::from_secs(30),
        target_budget: crate::storage::hygiene::agent::CARGO_TARGET_BUDGET,
        removals: None,
    };
    let uid = crate::storage::hygiene::agent::current_uid();
    let scan = crate::storage::hygiene::agent::scan_sessions(
        session_tmp_root,
        uid,
        &scan_ctx,
        measure_private,
    );
    note_agent_scan_outcome(&scan, out);
    for candidate in scan.candidates {
        if candidate.bytes == 0 || is_excluded(Path::new(&candidate.path)) {
            continue;
        }
        let target_label = Path::new(&candidate.path)
            .strip_prefix(&candidate.repo_path)
            .map(|rel| format!("session-scratch:{}", rel.to_string_lossy()))
            .unwrap_or_else(|_| format!("session-scratch:{}", candidate.path));
        let age_secs = scan
            .newest_secs
            .get(&candidate.path)
            .map(|&newest| now_secs.saturating_sub(newest));
        let recent_below_retention =
            age_secs.is_some_and(|s| (s / 86_400) < u64::from(configured_retention_days));
        candidates.push(HygieneCandidate {
            id: String::new(),
            category: "session_scratchpad",
            repo_or_scope: candidate.repo_path.clone(),
            target_label: target_label.clone(),
            provider: candidate.provider.clone(),
            bytes: candidate.bytes,
            private_bytes: candidate.private_bytes,
            age_secs,
            recent_below_retention,
            checkout_has_live_agent: false,
            evidence: candidate.evidence.join("; "),
            action: HygieneCandidateAction::AgentScratch {
                candidate,
                target_label,
                is_session_scratchpad: true,
            },
        });
    }
}

#[allow(clippy::too_many_arguments)]
fn collect_repo_agent_scratch_candidates(
    checkouts: &[PathBuf],
    scan_retention_days: u32,
    configured_retention_days: u32,
    now_secs: u64,
    measure_private: bool,
    live_cwds: &[PathBuf],
    observe_live: &dyn Fn() -> crate::storage::hygiene::agent::LiveSessions,
    is_excluded: &dyn Fn(&Path) -> bool,
    candidates: &mut Vec<HygieneCandidate>,
    skipped_items: &mut Vec<StewardCleanItem>,
    out: &mut CleanPassOutcome,
) {
    if checkouts.is_empty() {
        return;
    }
    let scan_live = observe_live();
    let cancel = AtomicBool::new(false);
    let in_use = |dir: &Path| crate::sysload::procscan::dir_use(dir);
    let scan_ctx = crate::storage::hygiene::agent::Context {
        now: now_secs,
        retention_days: scan_retention_days,
        live: &scan_live,
        use_of: &in_use,
        cancel: &cancel,
        deadline: Instant::now() + Duration::from_secs(30),
        target_budget: crate::storage::hygiene::agent::CARGO_TARGET_BUDGET,
        removals: None,
    };

    let mut raw_candidates = Vec::new();
    let mut newest_map = HashMap::new();
    for checkout in checkouts {
        let scan =
            crate::storage::hygiene::agent::scan_repository(checkout, &scan_ctx, measure_private);
        note_agent_scan_outcome(&scan, out);
        newest_map.extend(scan.newest_secs);
        raw_candidates.extend(scan.candidates);
    }
    let claude_scan =
        crate::storage::hygiene::agent::scan_claude_targets(checkouts, &scan_ctx, measure_private);
    note_agent_scan_outcome(&claude_scan, out);
    newest_map.extend(claude_scan.newest_secs);
    raw_candidates.extend(claude_scan.candidates);

    for candidate in raw_candidates {
        if candidate.bytes == 0 || is_excluded(Path::new(&candidate.path)) {
            continue;
        }
        let checkout_root = find_git_root(Path::new(&candidate.repo_path))
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| candidate.repo_path.clone());
        let target_label = Path::new(&candidate.path)
            .strip_prefix(&checkout_root)
            .or_else(|_| Path::new(&candidate.path).strip_prefix(&candidate.repo_path))
            .map(|rel| format!("scratch:{}", rel.to_string_lossy()))
            .unwrap_or_else(|_| format!("scratch:{}", candidate.path));
        if let Err(err) = crate::engine::process::with_disk_relief(|| {
            crate::storage::hygiene::global::leases_clear(&checkout_root, None)
        }) {
            skipped_items.push(StewardCleanItem {
                repo_path: checkout_root,
                target: target_label,
                status: "skipped".into(),
                measured_bytes: candidate.bytes,
                bytes_after: None,
                detail: err.message().to_string(),
            });
            continue;
        }
        let canon_checkout = crate::engine::process::canonicalize_plain(Path::new(&checkout_root))
            .unwrap_or_else(|_| PathBuf::from(&checkout_root));
        let checkout_has_live_agent = live_cwds.iter().any(|cwd| cwd.starts_with(&canon_checkout));
        let age_secs = newest_map
            .get(&candidate.path)
            .map(|&newest| now_secs.saturating_sub(newest));
        let recent_below_retention =
            age_secs.is_some_and(|s| (s / 86_400) < u64::from(configured_retention_days));
        candidates.push(HygieneCandidate {
            id: String::new(),
            category: "repo_agent_scratch",
            repo_or_scope: checkout_root,
            target_label: target_label.clone(),
            provider: candidate.provider.clone(),
            bytes: candidate.bytes,
            private_bytes: candidate.private_bytes,
            age_secs,
            recent_below_retention,
            checkout_has_live_agent,
            evidence: candidate.evidence.join("; "),
            action: HygieneCandidateAction::AgentScratch {
                candidate,
                target_label,
                is_session_scratchpad: false,
            },
        });
    }
}

/// Returns `false` only when the global cleaner's `mutation_lock` could not be
/// acquired (signalling the caller to stop attempting further mutations).
#[allow(clippy::too_many_arguments)]
fn execute_agent_scratch_candidate(
    candidate: &crate::storage::hygiene::global::CleanerCandidate,
    target_label: &str,
    is_session_scratchpad: bool,
    retention_days: u32,
    cloud_selected: bool,
    cloud_rationale: &str,
    observe_live: &dyn Fn() -> crate::storage::hygiene::agent::LiveSessions,
    own_removals: &crate::storage::hygiene::agent::SelfRemovals,
    initial_exclusions: &[PathBuf],
    policy_still_allows: PreMutationPolicyCheck<'_>,
    scratch_reclaimed: &mut u64,
    out: &mut CleanPassOutcome,
) -> bool {
    let checkout_root = if is_session_scratchpad {
        candidate.repo_path.clone()
    } else {
        find_git_root(Path::new(&candidate.repo_path))
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| candidate.repo_path.clone())
    };
    let _mutation = match crate::storage::hygiene::global::mutation_lock() {
        Ok(guard) => guard,
        Err(err) => {
            out.deferred_or_busy = true;
            out.warnings.push(err.message().to_string());
            return false;
        }
    };
    let policy_scope = if is_session_scratchpad {
        None
    } else {
        Some(checkout_root.as_str())
    };
    if let Err(why) = policy_still_allows(policy_scope) {
        out.items.push(StewardCleanItem {
            repo_path: checkout_root,
            target: target_label.to_string(),
            status: "skipped".into(),
            measured_bytes: candidate.bytes,
            bytes_after: None,
            detail: why,
        });
        return true;
    }
    let exclusions = fresh_cleaner_exclusions(initial_exclusions);
    if path_matches_exclusions(Path::new(&candidate.path), &exclusions) {
        out.items.push(StewardCleanItem {
            repo_path: checkout_root,
            target: target_label.to_string(),
            status: "skipped".into(),
            measured_bytes: candidate.bytes,
            bytes_after: None,
            detail: "Target is excluded by Global Cleaner settings.".into(),
        });
        return true;
    }
    if !is_session_scratchpad {
        if let Err(err) = crate::repository_trust::require(Path::new(&checkout_root)) {
            out.items.push(StewardCleanItem {
                repo_path: checkout_root,
                target: target_label.to_string(),
                status: "skipped".into(),
                measured_bytes: candidate.bytes,
                bytes_after: None,
                detail: err.message().to_string(),
            });
            return true;
        }
        if let Err(err) = crate::engine::process::with_disk_relief(|| {
            crate::storage::hygiene::global::leases_clear(&checkout_root, None)
        }) {
            out.items.push(StewardCleanItem {
                repo_path: checkout_root,
                target: target_label.to_string(),
                status: "skipped".into(),
                measured_bytes: candidate.bytes,
                bytes_after: None,
                detail: err.message().to_string(),
            });
            return true;
        }
        if let Err(err) = crate::harness::guard_file(&checkout_root, &candidate.path, "delete") {
            out.items.push(StewardCleanItem {
                repo_path: checkout_root,
                target: target_label.to_string(),
                status: "skipped".into(),
                measured_bytes: candidate.bytes,
                bytes_after: None,
                detail: err,
            });
            return true;
        }
    }
    // Re-observe live Claude sessions and wall-clock right before mutation so a
    // session resumed while scanning or waiting on Manvi cloud is preserved.
    let fresh_live = observe_live();
    let fresh_now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let cancel = AtomicBool::new(false);
    let in_use = |dir: &Path| crate::sysload::procscan::dir_use(dir);
    let remove_ctx = crate::storage::hygiene::agent::Context {
        now: fresh_now,
        retention_days,
        live: &fresh_live,
        use_of: &in_use,
        cancel: &cancel,
        deadline: Instant::now() + Duration::from_secs(45),
        target_budget: crate::storage::hygiene::agent::CARGO_TARGET_BUDGET,
        removals: Some(own_removals),
    };
    let remaining = MAX_STEWARD_AGENT_SCRATCH_BYTES.saturating_sub(*scratch_reclaimed);
    let prefix = if cloud_selected && !cloud_rationale.is_empty() {
        format!("Selected by Manvi cloud ({cloud_rationale}); removed")
    } else if cloud_selected {
        "Selected by Manvi cloud; removed".to_string()
    } else {
        "Removed".to_string()
    };
    let kind_phrase = if is_session_scratchpad {
        "finished-session Cargo scratchpad build"
    } else {
        "idle agent Cargo scratch build"
    };
    match crate::storage::hygiene::agent::remove_judged(candidate, &remove_ctx, remaining) {
        Ok(freed) => {
            *scratch_reclaimed = scratch_reclaimed.saturating_add(freed);
            out.reclaimed_bytes = out.reclaimed_bytes.saturating_add(freed);
            out.items.push(StewardCleanItem {
                repo_path: checkout_root,
                target: target_label.to_string(),
                status: "reclaimed".into(),
                measured_bytes: freed,
                bytes_after: Some(0),
                detail: format!(
                    "{prefix} {kind_phrase} ({}).",
                    candidate.evidence.join("; ")
                ),
            });
        }
        Err(crate::storage::hygiene::agent::Refusal::PartlyRemoved { why, bytes }) => {
            *scratch_reclaimed = scratch_reclaimed.saturating_add(bytes);
            out.reclaimed_bytes = out.reclaimed_bytes.saturating_add(bytes);
            out.items.push(StewardCleanItem {
                repo_path: checkout_root,
                target: target_label.to_string(),
                status: "partially_reclaimed".into(),
                measured_bytes: candidate.bytes,
                bytes_after: None,
                detail: format!("Stopped part way ({bytes} bytes removed): {why}"),
            });
        }
        Err(refusal) => {
            if matches!(
                refusal,
                crate::storage::hygiene::agent::Refusal::Unexamined(_)
            ) {
                out.deferred_or_busy = true;
            }
            out.items.push(StewardCleanItem {
                repo_path: checkout_root,
                target: target_label.to_string(),
                status: "skipped".into(),
                measured_bytes: candidate.bytes,
                bytes_after: None,
                detail: refusal.message(),
            });
        }
    }
    true
}

#[cfg(all(test, unix))]
fn clean_session_agent_scratchpads(
    session_tmp_root: &Path,
    retention_days: u32,
    observe_live: &dyn Fn() -> crate::storage::hygiene::agent::LiveSessions,
    own_removals: &crate::storage::hygiene::agent::SelfRemovals,
    is_excluded: &dyn Fn(&Path) -> bool,
    scratch_reclaimed: &mut u64,
    out: &mut CleanPassOutcome,
) {
    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let mut candidates = Vec::new();
    collect_session_scratch_candidates(
        session_tmp_root,
        retention_days,
        retention_days,
        now_secs,
        false,
        observe_live,
        is_excluded,
        &mut candidates,
        out,
    );
    for cand in candidates.into_iter().take(28) {
        if *scratch_reclaimed >= MAX_STEWARD_AGENT_SCRATCH_BYTES {
            break;
        }
        if let HygieneCandidateAction::AgentScratch {
            candidate,
            target_label,
            is_session_scratchpad,
        } = cand.action
        {
            if !execute_agent_scratch_candidate(
                &candidate,
                &target_label,
                is_session_scratchpad,
                retention_days,
                false,
                "",
                observe_live,
                own_removals,
                &[],
                &|_| Ok(()),
                scratch_reclaimed,
                out,
            ) {
                break;
            }
        }
    }
}

/// Executes one `storage::hygiene` target (`local:*`, `worktree:*`, or
/// `cache:*`) with fresh under-lock policy, exclusion, worktree/project-quiet,
/// and harness-gate checks, always cancelling `plan.id` on pre-execution failure
/// so `PLANS.pending` (`MAX_PLANS = 4`) never leaks. Returns `false` only when
/// the global mutation/execution lock is busy.
/// The under-lock policy recheck for one target. A shared cache answers to the
/// global switch AND to the repository it is pruned under, which must still be
/// trusted and still have cleanup on; any other target answers to its own
/// repository.
fn recheck_target_policy(
    repo_root: &str,
    target: &str,
    policy_still_allows: PreMutationPolicyCheck<'_>,
) -> Result<(), String> {
    if target.starts_with("cache:") {
        policy_still_allows(None)?;
    }
    policy_still_allows(Some(repo_root))
}

#[allow(clippy::too_many_arguments)]
fn execute_single_hygiene_target(
    repo_root: &str,
    target: &str,
    display_target: &str,
    retention_days: u32,
    is_linked_worktree: bool,
    worktree_branch: Option<&str>,
    artifact_rel_path: Option<&str>,
    cloud_selected: bool,
    cloud_rationale: &str,
    initial_exclusions: &[PathBuf],
    policy_still_allows: PreMutationPolicyCheck<'_>,
    out: &mut CleanPassOutcome,
) -> bool {
    if crate::storage::hygiene::requires_user_confirmation(target) {
        out.items.push(StewardCleanItem {
            repo_path: repo_root.to_string(),
            target: display_target.to_string(),
            status: "skipped".into(),
            measured_bytes: 0,
            bytes_after: None,
            detail: "Removal needs a user-confirmed action; the scheduled steward does not run it."
                .into(),
        });
        return true;
    }
    let plan = match crate::engine::process::with_disk_relief(|| {
        crate::storage::hygiene::prepare(repo_root, target, retention_days)
    }) {
        Ok(plan) => plan,
        Err(err) => {
            if err.origin() == Some(IpcErrorKind::DeferredUnderLoad) {
                out.deferred_or_busy = true;
            }
            out.items.push(StewardCleanItem {
                repo_path: repo_root.to_string(),
                target: display_target.to_string(),
                status: "skipped".into(),
                measured_bytes: 0,
                bytes_after: None,
                detail: err.message().to_string(),
            });
            return true;
        }
    };
    let measured = plan.bytes;
    let mut skipped_by_precheck = false;
    let outcome = crate::engine::process::with_disk_relief(|| {
        crate::storage::hygiene::execute(repo_root, &plan.id, |argv, path| {
            if let Err(why) = recheck_target_policy(repo_root, target, policy_still_allows) {
                skipped_by_precheck = true;
                return Err(crate::ipc_error::CommandError::from(why));
            }
            let exclusions = fresh_cleaner_exclusions(initial_exclusions);
            if path_matches_exclusions(Path::new(path), &exclusions) {
                skipped_by_precheck = true;
                return Err("Target is excluded by Global Cleaner settings.".into());
            }
            if is_linked_worktree {
                if let Err(err) =
                    crate::storage::hygiene::global::quiet_worktree(repo_root, worktree_branch)
                {
                    skipped_by_precheck = true;
                    return Err(err);
                }
            } else if let Some(rel) = artifact_rel_path {
                if let Err(err) = crate::storage::hygiene::global::quiet_project(repo_root, rel) {
                    skipped_by_precheck = true;
                    return Err(err);
                }
            }
            if let Some(argv) = argv {
                if let Err(err) = crate::harness::guard_command(
                    repo_root,
                    &argv.iter().map(String::as_str).collect::<Vec<_>>(),
                ) {
                    skipped_by_precheck = true;
                    return Err(err.into());
                }
            }
            if let Err(err) = crate::harness::guard_file(repo_root, path, "delete") {
                skipped_by_precheck = true;
                return Err(err.into());
            }
            Ok(())
        })
    });
    match outcome {
        Ok(((), result)) if result.success => {
            let saved = result
                .bytes_after
                .map(|after| result.bytes_before.saturating_sub(after))
                .unwrap_or(0);
            out.reclaimed_bytes = out.reclaimed_bytes.saturating_add(saved);
            let detail = if cloud_selected && !cloud_rationale.is_empty() {
                format!(
                    "Selected by Manvi cloud ({cloud_rationale}); {}",
                    result.message
                )
            } else if cloud_selected {
                format!("Selected by Manvi cloud; {}", result.message)
            } else {
                result.message
            };
            out.items.push(StewardCleanItem {
                repo_path: repo_root.to_string(),
                target: display_target.to_string(),
                status: "reclaimed".into(),
                measured_bytes: result.bytes_before,
                bytes_after: result.bytes_after,
                detail,
            });
            true
        }
        Ok(((), result)) => {
            out.items.push(StewardCleanItem {
                repo_path: repo_root.to_string(),
                target: display_target.to_string(),
                status: "failed".into(),
                measured_bytes: result.bytes_before,
                bytes_after: result.bytes_after,
                detail: result.message,
            });
            true
        }
        Err(err) => {
            let _ = crate::storage::hygiene::cancel(repo_root, &plan.id);
            let msg = err.message().to_string();
            let lock_busy = msg.contains("Another hygiene operation is running")
                || msg.contains("Cleaner is already running")
                || msg.contains("mutation.lock");
            if err.origin() == Some(IpcErrorKind::DeferredUnderLoad) || lock_busy {
                out.deferred_or_busy = true;
            }
            let changed_since_preview = msg.contains("Files changed")
                || msg.contains("Files are open")
                || msg.contains("Artifact path changed");
            let status = if skipped_by_precheck || changed_since_preview || lock_busy {
                "skipped"
            } else {
                "failed"
            };
            out.items.push(StewardCleanItem {
                repo_path: repo_root.to_string(),
                target: display_target.to_string(),
                status: status.into(),
                measured_bytes: measured,
                bytes_after: None,
                detail: msg,
            });
            !lock_busy
        }
    }
}

fn status_manvi_after_tick(rt: &StewardRuntime, record: &StewardTickRecord) -> ManviStatus {
    let cached = crate::supervisor::lock_or_recover(&rt.inner)
        .cached_manvi
        .clone();
    if let Some(prev) = cached {
        return prev;
    }
    if record.outcome == "manvi_deferred" {
        return ManviStatus {
            state: "deferred".into(),
            detail: record.summary.clone(),
            ..ManviStatus::default()
        };
    }
    ManviStatus::default()
}

fn run_live_tick(
    rt: &Arc<StewardRuntime>,
    trigger: &str,
    explicit_repo: Option<&str>,
    workbench: &WorkbenchState,
    watcher: Option<&WatcherState>,
) -> Result<(StewardTickRecord, ManviStewardStatus), String> {
    if rt.running_tick.swap(true, Ordering::SeqCst) {
        return Err("A Manvi steward check is already running.".into());
    }
    let _reset = ResetRunning(&rt.running_tick);
    let watched_roots = watcher.map(WatcherState::watched_roots).unwrap_or_default();
    let deps = LiveDeps {
        workbench,
        watched_roots,
    };
    let record = evaluate_tick_with(rt, trigger, explicit_repo, &deps);
    // Reuse the `manvi use` snapshot already populated by `evaluate_tick_with`
    // rather than spawning a second `manvi use --json` subprocess per tick.
    let manvi = status_manvi_after_tick(rt, &record);
    let settings = deps.settings();
    let updated = build_status(rt, settings, &manvi, explicit_repo);
    emit_status(rt, &updated);
    Ok((record, updated))
}

pub(crate) fn evaluate_tick_with(
    rt: &StewardRuntime,
    trigger: &str,
    explicit_repo: Option<&str>,
    deps: &dyn StewardDeps,
) -> StewardTickRecord {
    let now_ms = deps.now_ms();
    let id = format!(
        "steward-{now_ms}-{}",
        SEQUENCE.fetch_add(1, Ordering::Relaxed) + 1
    );
    let settings = deps.settings();
    let (forced_enabled, env_note) = env_override_of(deps.env_var().as_deref());
    let mut warnings = Vec::new();
    if let Some(note) = env_note {
        warnings.push(note);
    }

    let master_enabled = is_master_enabled(&settings, forced_enabled);

    // Check `!master_enabled` BEFORE spawning `manvi use` so a disabled steward
    // never spawns subprocesses even when invoked manually via `Run check`.
    if !master_enabled {
        let mut extra_warnings = warnings;
        let mut restored = 0u32;
        if let Some(dir) = deps.config_dir() {
            if let Err(err) = deps.clear_active_lease(&dir) {
                extra_warnings.push(format!("Could not clear steward lease: {err}"));
            }
        }
        let (prev_throttled, cached_manvi) = {
            let mut guard = crate::supervisor::lock_or_recover(&rt.inner);
            guard.critical_throttle_active = false;
            guard.last_critical_wake = None;
            (
                std::mem::take(&mut guard.throttled),
                guard.cached_manvi.clone(),
            )
        };
        let had_throttles = !prev_throttled.is_empty();
        for (pid, (start_us, _)) in prev_throttled {
            if matches!(deps.set_process_throttle(pid, start_us, false), Ok(true)) {
                restored = restored.saturating_add(1);
            }
        }
        if had_throttles {
            if let Some(dir) = deps.config_dir() {
                let _ = write_throttle_journal(&dir, &BTreeMap::new());
            }
        }
        let (manvi_mode, manvi_model) = match cached_manvi {
            Some(m) if m.state == "ready" && m.typed.is_some() && m.unsupported.is_empty() => {
                (m.mode, m.model)
            }
            Some(m) if m.state == "not_chosen" || m.state == "not_installed" => {
                ("unconfigured".to_string(), m.model)
            }
            Some(m) => ("unavailable".to_string(), m.model),
            None => ("unavailable".to_string(), String::new()),
        };
        let rec = StewardTickRecord {
            id,
            trigger: trigger.to_string(),
            ran_at_ms: now_ms,
            outcome: "disabled".into(),
            tier: "unknown".into(),
            binding: "none".into(),
            manvi_mode,
            manvi_model,
            model_generated: false,
            summary: "Manvi system steward is switched off.".into(),
            directive: String::new(),
            messaged_sessions: 0,
            messaged_repositories: 0,
            skipped_untrusted_repositories: 0,
            throttled_processes: 0,
            restored_processes: restored,
            reclaimed_bytes: 0,
            cleaned_items: Vec::new(),
            warnings: extra_warnings,
        };
        store_tick(rt, now_ms, rec.clone());
        return rec;
    }

    let disk_remedy = deps.disk_remedy_due();
    // A deferred or unreadable `manvi use` says nothing about whether Manvi is
    // configured: reuse the last answer, or record that it could not be
    // checked while existing throttles and the lease stay in place.
    let unchecked = match deps.manvi_probe(disk_remedy) {
        ManviProbe::Answered(status) => {
            crate::supervisor::lock_or_recover(&rt.inner).cached_manvi =
                Some(ManviStatus::clone(&status));
            Ok(*status)
        }
        ManviProbe::Deferred(why) => Err((
            "manvi_deferred",
            "`manvi use` was deferred under host load".to_string(),
            format!("Checking Manvi configuration was deferred under host load ({why})"),
        )),
        ManviProbe::Unreadable(why) => Err((
            "failed",
            format!("`manvi use` could not be read ({why})"),
            format!("Couldn't ask Manvi whether a model is configured ({why})"),
        )),
    };
    let manvi = match unchecked {
        Ok(status) => status,
        Err((outcome, reuse_reason, summary)) => {
            let cached = crate::supervisor::lock_or_recover(&rt.inner)
                .cached_manvi
                .clone();
            match cached {
                Some(prev) => {
                    warnings.push(format!(
                        "Reused cached Manvi model configuration because {reuse_reason}."
                    ));
                    prev
                }
                None => {
                    // Never revoke active throttles or misreport Manvi as unconfigured
                    // merely because `manvi use` could not be asked before a first read,
                    // and clear `last_critical_wake` so the next pulse retries.
                    crate::supervisor::lock_or_recover(&rt.inner).last_critical_wake = None;
                    let rec = StewardTickRecord {
                        id,
                        trigger: trigger.to_string(),
                        ran_at_ms: now_ms,
                        outcome: outcome.into(),
                        tier: "unknown".into(),
                        binding: "none".into(),
                        manvi_mode: "unavailable".into(),
                        manvi_model: String::new(),
                        model_generated: false,
                        summary: format!("{summary}; existing safeguards stay in place."),
                        directive: String::new(),
                        messaged_sessions: 0,
                        messaged_repositories: 0,
                        skipped_untrusted_repositories: 0,
                        throttled_processes: 0,
                        restored_processes: 0,
                        reclaimed_bytes: 0,
                        cleaned_items: Vec::new(),
                        warnings,
                    };
                    store_tick(rt, now_ms, rec.clone());
                    return rec;
                }
            }
        }
    };

    let manvi_ready = is_manvi_ready(&manvi);
    let manvi_mode_label = if manvi_ready {
        manvi.mode.clone()
    } else if manvi.state == "not_chosen" || manvi.state == "not_installed" {
        "unconfigured".to_string()
    } else {
        "unavailable".to_string()
    };

    // Helper that clears active throttles (and resets `last_critical_wake` so a
    // subsequent critical pulse is never swallowed by debounce) and records a
    // terminal tick. `force_clear_lease = false` preserves any valid unexpired
    // `active.json` owned by a different live GitPulse writer process.
    let release_and_record = |outcome: &str,
                              tier: &str,
                              binding: &str,
                              summary: String,
                              mut extra_warnings: Vec<String>,
                              force_clear_lease: bool|
     -> StewardTickRecord {
        let mut restored = 0u32;
        if let Some(dir) = deps.config_dir() {
            let res = if force_clear_lease {
                deps.clear_active_lease(&dir)
            } else {
                deps.clear_owned_active_lease(&dir)
            };
            if let Err(err) = res {
                extra_warnings.push(format!("Could not clear steward lease: {err}"));
            }
        }
        let prev_throttled = {
            let mut guard = crate::supervisor::lock_or_recover(&rt.inner);
            guard.critical_throttle_active = false;
            guard.last_critical_wake = None;
            std::mem::take(&mut guard.throttled)
        };
        let had_throttles = !prev_throttled.is_empty();
        for (pid, (start_us, _)) in prev_throttled {
            if matches!(deps.set_process_throttle(pid, start_us, false), Ok(true)) {
                restored = restored.saturating_add(1);
            }
        }
        if had_throttles {
            if let Some(dir) = deps.config_dir() {
                let _ = write_throttle_journal(&dir, &BTreeMap::new());
            }
        }
        let rec = StewardTickRecord {
            id: id.clone(),
            trigger: trigger.to_string(),
            ran_at_ms: now_ms,
            outcome: outcome.to_string(),
            tier: tier.to_string(),
            binding: binding.to_string(),
            manvi_mode: manvi_mode_label.clone(),
            manvi_model: manvi.model.clone(),
            model_generated: false,
            summary,
            directive: String::new(),
            messaged_sessions: 0,
            messaged_repositories: 0,
            skipped_untrusted_repositories: 0,
            throttled_processes: 0,
            restored_processes: restored,
            reclaimed_bytes: 0,
            cleaned_items: Vec::new(),
            warnings: extra_warnings,
        };
        store_tick(rt, now_ms, rec.clone());
        rec
    };

    if !manvi_ready {
        let why = if !manvi.unsupported.is_empty() {
            manvi.unsupported.clone()
        } else if !manvi.detail.is_empty() {
            manvi.detail.clone()
        } else {
            "Manvi has no configured cloud or local model.".into()
        };
        return release_and_record(
            "manvi_unconfigured",
            "unknown",
            "none",
            format!("Steward stood down because Manvi is not configured: {why}"),
            warnings,
            false,
        );
    }

    let load = deps.host_load();
    if load.role == crate::sysload::Role::Reader && trigger != "manual" {
        return release_and_record(
            "sensor_unavailable",
            "unknown",
            "none",
            "Another running GitPulse instance holds the host-load writer election and governs this machine."
                .into(),
            warnings,
            false,
        );
    }
    let sensor_enabled = load.sensor.enabled.unwrap_or(false);
    if !sensor_enabled || !load.sensor.state.governs() {
        let why = load.sensor.reason.clone().unwrap_or_else(|| {
            if !sensor_enabled {
                "host-load measurement is switched off".into()
            } else {
                format!("sensor state is {:?}", load.sensor.state)
            }
        });
        return release_and_record(
            "sensor_unavailable",
            "unknown",
            "none",
            format!("Host load could not be verified ({why}); nothing was assumed healthy."),
            warnings,
            false,
        );
    }

    if load.sensor.state == SensorState::StaleRecent {
        if let Some(age_ms) = load.sensor.age_ms {
            warnings.push(format!(
                "Host-load reading is {}s old (stale-recent).",
                age_ms / 1_000
            ));
        }
    }

    let verdict = &load.governor.verdict;
    let critical = is_critical_load(&load);
    let tier = if critical {
        verdict.tier.max(Tier::Severe)
    } else {
        verdict.tier
    };
    let binding = if load.pressure == PressureInput::Critical && verdict.tier < Tier::Severe {
        Some(Binding::MemoryPressure)
    } else {
        verdict.binding.or_else(|| {
            (load.pressure == PressureInput::Critical).then_some(Binding::MemoryPressure)
        })
    };
    let b_key = binding_key(binding);
    let t_name = tier_name(tier);

    // Healthy fast-path: restore any prior throttles, clear lease, no LLM call.
    if tier == Tier::Clear && !critical && !disk_remedy {
        return release_and_record(
            "healthy",
            t_name,
            b_key,
            "Host load and storage are clear; no intervention needed.".into(),
            warnings,
            load.role != crate::sysload::Role::Reader,
        );
    }

    // Partition candidate repositories by trust and resolved ("glocal") policy.
    // Messaging deduplicates by canonical `common_dir` so multiple linked worktrees
    // of one repository never spam duplicate bus/mailbox directives.
    let candidate_repos = deps.candidate_repos(&load, explicit_repo);
    let mut skipped_untrusted = 0u32;
    let mut message_targets: BTreeMap<String, String> = BTreeMap::new();
    let mut clean_repos = Vec::new();
    // Common directories of the trusted repositories whose policy slows
    // agents: the only places an agent process is throttled or a command paced.
    let mut slow_scope: BTreeSet<String> = BTreeSet::new();
    for repo_root in &candidate_repos {
        if !deps.repo_trusted(repo_root) {
            skipped_untrusted = skipped_untrusted.saturating_add(1);
            continue;
        }
        let common = deps.repo_common_dir(repo_root);
        let eff = effective_policy_for(&settings, common.as_deref(), forced_enabled);
        if !eff.enabled {
            continue;
        }
        if eff.slow_critical {
            if let Some(common) = &common {
                slow_scope.insert(common.clone());
            }
        }
        if eff.message_agents {
            let bus_key = common.unwrap_or_else(|| repo_root.clone());
            message_targets
                .entry(bus_key)
                .or_insert_with(|| repo_root.clone());
        }
        if eff.clean_critical {
            clean_repos.push(repo_root.clone());
        }
    }

    // 1. Critical slowdown: lease + OS background process throttling.
    let mut throttled_now = 0u32;
    let mut restored_now = 0u32;
    let any_slow_enabled = any_slow_policy_enabled(&settings, forced_enabled);
    if critical && any_slow_enabled && load.role != crate::sysload::Role::Reader {
        let candidates_res = deps.throttle_candidates();
        let (mut desired, episode_started, mut already_background) = {
            let guard = crate::supervisor::lock_or_recover(&rt.inner);
            (
                guard.throttled.clone(),
                !guard.critical_throttle_active && guard.throttled.is_empty(),
                guard.background_before_episode.clone(),
            )
        };
        if episode_started {
            already_background.clear();
        }

        match candidates_res {
            Ok(candidates) => {
                // Stop tracking entries that are no longer candidates. One that
                // exited or was recycled refuses the restore; one still running
                // (its session root ended, say) is put back to normal priority
                // rather than forgotten in the background.
                let live_pairs: BTreeSet<(i32, u64)> =
                    candidates.iter().map(|c| (c.pid, c.start_us)).collect();
                let dropped: Vec<(i32, u64)> = desired
                    .iter()
                    .filter(|(pid, (start_us, _))| !live_pairs.contains(&(**pid, *start_us)))
                    .map(|(pid, (start_us, _))| (*pid, *start_us))
                    .collect();
                for (pid, start_us) in dropped {
                    desired.remove(&pid);
                    if matches!(deps.set_process_throttle(pid, start_us, false), Ok(true)) {
                        restored_now = restored_now.saturating_add(1);
                    }
                }

                // One line per cause: the same refusal (Linux limits, say)
                // repeats for every process on every tick otherwise.
                let mut refused: BTreeMap<String, Vec<String>> = BTreeMap::new();
                let mut common_of: HashMap<String, Option<String>> = HashMap::new();
                for cand in candidates {
                    // The process's own repository, or, for one working
                    // outside every repository (a build script in the Cargo
                    // registry), the nearest ancestor's.
                    let common = cand
                        .session_cwd
                        .iter()
                        .chain(cand.lineage_cwds.iter())
                        .find_map(|cwd| {
                            common_of
                                .entry(cwd.clone())
                                .or_insert_with(|| deps.repo_common_dir(cwd))
                                .clone()
                        });
                    // Only work inside a trusted repository whose policy slows
                    // agents; anything else is left alone, or put back.
                    let in_scope = common.as_ref().is_some_and(|c| slow_scope.contains(c));
                    if !in_scope {
                        if let Some((prev_start, _)) = desired.remove(&cand.pid) {
                            if matches!(
                                deps.set_process_throttle(cand.pid, prev_start, false),
                                Ok(true)
                            ) {
                                restored_now = restored_now.saturating_add(1);
                            }
                        }
                        continue;
                    }
                    if desired
                        .get(&cand.pid)
                        .is_some_and(|(prev_start, _)| *prev_start == cand.start_us)
                    {
                        desired.insert(cand.pid, (cand.start_us, common));
                        continue;
                    }
                    match deps.set_process_throttle(cand.pid, cand.start_us, true) {
                        Ok(true) => {
                            desired.insert(cand.pid, (cand.start_us, common));
                        }
                        // Already in the background. Seen on the episode's first
                        // tick, it was put there by someone else and is left
                        // alone. Seen later, it is ours only when its
                        // backgrounding is attributable to this episode: an
                        // ancestor (within the scan's bound) is a process this
                        // episode throttled, so it inherited the priority.
                        // Any other (the person or another supervisor
                        // backgrounded it) is recorded and left alone, since
                        // restoring it would overwrite their choice.
                        Ok(false) => {
                            let key = (cand.pid, cand.start_us);
                            let inherited = !episode_started
                                && !already_background.contains(&key)
                                && cand.ancestor_pids.iter().any(|a| desired.contains_key(a));
                            if inherited {
                                desired.insert(cand.pid, (cand.start_us, common));
                            } else {
                                already_background.insert(key);
                            }
                        }
                        Err(err) => refused
                            .entry(err)
                            .or_default()
                            .push(format!("{} (pid {})", cand.name, cand.pid)),
                    }
                }
                for (err, procs) in refused {
                    let shown: Vec<&str> = procs.iter().take(3).map(String::as_str).collect();
                    let more = procs.len().saturating_sub(shown.len());
                    let tail = if more > 0 {
                        format!(" and {more} more")
                    } else {
                        String::new()
                    };
                    warnings.push(format!("Could not slow {}{tail}: {err}", shown.join(", ")));
                }
            }
            Err(err) => warnings.push(format!("Agent process throttle scan failed: {err}")),
        }

        // Re-check latest settings before publishing `active.json` or committing
        // `throttled` so a concurrent `save_settings` / `save_repo_override`
        // disable during an in-flight tick never leaves stale throttles behind.
        let latest_settings = deps.settings();
        let (latest_forced, _) = env_override_of(deps.env_var().as_deref());
        let still_any_slow = any_slow_policy_enabled(&latest_settings, latest_forced);
        let mut aborted_restore = Vec::new();
        desired.retain(|&pid, (start_us, common)| {
            let eff = effective_policy_for(&latest_settings, common.as_deref(), latest_forced);
            if still_any_slow && eff.enabled && eff.slow_critical {
                true
            } else {
                aborted_restore.push((pid, *start_us));
                false
            }
        });
        for (pid, start_us) in aborted_restore {
            if matches!(deps.set_process_throttle(pid, start_us, false), Ok(true)) {
                restored_now = restored_now.saturating_add(1);
            }
        }

        // Commit under the lock and check `closed` there. `shutdown` sets it
        // before it takes `throttled` to restore, so either it restores this
        // set or this tick sees it closed and restores its own throttles.
        let shutting_down = still_any_slow && {
            let mut guard = crate::supervisor::lock_or_recover(&rt.inner);
            let closed = rt.closed.load(Ordering::SeqCst);
            if !closed {
                guard.throttled = desired.clone();
                guard.background_before_episode = already_background;
                guard.critical_throttle_active = true;
            }
            closed
        };
        if shutting_down {
            for (pid, (start_us, _)) in std::mem::take(&mut desired) {
                if matches!(deps.set_process_throttle(pid, start_us, false), Ok(true)) {
                    restored_now = restored_now.saturating_add(1);
                }
            }
            warnings.push("GitPulse is shutting down, so this check undid its throttles.".into());
        } else if still_any_slow {
            if let Some(dir) = deps.config_dir() {
                if let Err(err) = write_throttle_journal(&dir, &desired) {
                    warnings.push(format!(
                        "Could not record throttled processes for restore after a crash: {err}"
                    ));
                }
            }
            let writer_pid = std::process::id();
            let writer_start = crate::sysload::process_start_us(writer_pid);
            if writer_start.is_none() {
                warnings.push(
                    "Agent commands are not paced or refused: GitPulse could not read its own process start time, which the command hook needs to trust the slowdown lease."
                        .into(),
                );
            }
            if let (Some(dir), Some(writer_start_us)) = (deps.config_dir(), writer_start) {
                let ttl_ms = lease_ttl_ms(latest_settings.interval_secs);
                let lease_reason = if verdict.tier >= Tier::Severe || !verdict.admit {
                    verdict.message.clone()
                } else {
                    format!(
                        "kernel {} pressure escalated before governor hysteresis settled",
                        binding.map_or("host", Binding::cause_phrase)
                    )
                };
                let lease = ActiveThrottleRecord {
                    version: LEASE_VERSION,
                    writer_pid,
                    writer_start_us,
                    issued_at_ms: now_ms,
                    expires_at_ms: now_ms.saturating_add(ttl_ms),
                    tier: t_name.to_string(),
                    binding: b_key.to_string(),
                    reason: lease_reason,
                    pace_ms: CRITICAL_PACE_MS,
                    repositories: slow_scope
                        .iter()
                        .take(MAX_LEASE_REPOSITORIES)
                        .cloned()
                        .collect(),
                };
                if let Err(err) = deps.write_active_lease(&dir, &lease) {
                    warnings.push(format!("Could not publish critical throttle lease: {err}"));
                }
            }
            throttled_now = u32::try_from(desired.len()).unwrap_or(u32::MAX);
        } else {
            if let Some(dir) = deps.config_dir() {
                let _ = deps.clear_active_lease(&dir);
                let _ = write_throttle_journal(&dir, &BTreeMap::new());
            }
            let mut guard = crate::supervisor::lock_or_recover(&rt.inner);
            guard.throttled.clear();
            guard.critical_throttle_active = false;
        }
    } else {
        if let Some(dir) = deps.config_dir() {
            if !any_slow_enabled || load.role != crate::sysload::Role::Reader {
                let _ = deps.clear_active_lease(&dir);
            } else {
                let _ = deps.clear_owned_active_lease(&dir);
            }
        }
        let prev = {
            let mut guard = crate::supervisor::lock_or_recover(&rt.inner);
            guard.critical_throttle_active = false;
            if !critical {
                guard.last_critical_wake = None;
            }
            std::mem::take(&mut guard.throttled)
        };
        let had_throttles = !prev.is_empty();
        for (pid, (start_us, _)) in prev {
            if matches!(deps.set_process_throttle(pid, start_us, false), Ok(true)) {
                restored_now = restored_now.saturating_add(1);
            }
        }
        if had_throttles {
            if let Some(dir) = deps.config_dir() {
                let _ = write_throttle_journal(&dir, &BTreeMap::new());
            }
        }
    }

    // 2. Critical storage cleanup via canonical `storage::hygiene`, only when
    //    disk is what is critical: deleting build caches frees no memory or
    //    CPU. With Manvi cloud configured, the model chooses among the
    //    pre-verified candidates (including recent idle scratch builds).
    //    Closed/idle trusted repositories (from `repository_trust` and Global
    //    Cleaner `roots`) are discovered ONLY when `should_clean` is true so
    //    ordinary Elevated messaging ticks never walk cold checkouts.
    let storage_critical = disk_remedy
        || (critical && matches!(binding, Some(Binding::LowDisk | Binding::DiskFilling)))
        || load
            .governor
            .disk_fill
            .as_ref()
            .is_some_and(|df| df.tier >= Tier::Severe);
    let should_clean = storage_critical;
    if should_clean {
        let mut seen_clean: BTreeSet<String> = candidate_repos.iter().cloned().collect();
        for extra_root in deps.cleanup_candidate_repos(&load, explicit_repo) {
            if !seen_clean.insert(extra_root.clone()) {
                continue;
            }
            if !deps.repo_trusted(&extra_root) {
                continue;
            }
            let common = deps.repo_common_dir(&extra_root);
            let eff = effective_policy_for(&settings, common.as_deref(), forced_enabled);
            if eff.enabled && eff.clean_critical && !clean_repos.contains(&extra_root) {
                clean_repos.push(extra_root);
            }
        }
    }
    let mut reclaimed_bytes = 0u64;
    let mut cleaned_items = Vec::new();
    let global_eff = effective_policy_for(&settings, None, forced_enabled);
    let clean_shared_caches = global_eff.enabled && global_eff.clean_critical;
    let any_clean_enabled = clean_shared_caches || !clean_repos.is_empty();
    if should_clean && any_clean_enabled {
        let cooldown_ok = trigger == "manual" || {
            let guard = crate::supervisor::lock_or_recover(&rt.inner);
            guard
                .last_clean_ms
                .is_none_or(|last| now_ms.saturating_sub(last) >= AUTO_CLEAN_COOLDOWN_MS)
        };
        if cooldown_ok {
            // Shared caches are pruned under one repository's trust and
            // command policy. It must be one with cleanup switched on: a
            // repository that opted out is never the one a prune runs under.
            let trusted_anchor = clean_repos.first().map(String::as_str);
            let clean_out = deps.clean_critical_storage(&CriticalCleanRequest {
                trusted_cleanable_repos: &clean_repos,
                trusted_anchor_repo: trusted_anchor,
                clean_shared_caches,
                retention_days: settings.clean_retention_days,
                disk_critical: storage_critical,
                load: &load,
                manvi: &manvi,
            });
            if clean_out.reclaimed_bytes > 0 || !clean_out.deferred_or_busy {
                let mut guard = crate::supervisor::lock_or_recover(&rt.inner);
                guard.last_clean_ms = Some(now_ms);
            }
            reclaimed_bytes = clean_out.reclaimed_bytes;
            cleaned_items = clean_out.items;
            warnings.extend(clean_out.warnings);
        } else {
            warnings.push(
                "Automatic cache cleanup skipped: ran within the 30-minute cooldown window.".into(),
            );
        }
    }

    // 3. Synthesize directive with Manvi (`work.assist.complete`), protecting
    //    severe RAM crises from local-model weight loading.
    let selection = manvi.typed.as_ref().expect("checked manvi_ready above");
    let severe_memory = load.pressure == PressureInput::Critical
        || (critical
            && matches!(
                binding,
                Some(
                    Binding::MemoryPressure
                        | Binding::CompressorThrash
                        | Binding::CompressorSegments
                        | Binding::Swap
                )
            ));
    let free_disk = load
        .snapshot
        .as_ref()
        .and_then(|s| s.host.free_disk.value());
    let (directive, model_generated) = if manvi.mode == "local" && severe_memory {
        warnings.push(
            "Skipped calling Manvi's local model server during severe memory pressure so model weights do not consume RAM; sent deterministic directive."
                .into(),
        );
        (
            deterministic_directive(
                tier,
                binding,
                disk_remedy,
                free_disk,
                throttled_now,
                reclaimed_bytes,
            ),
            false,
        )
    } else {
        let digest = telemetry_digest(&load, disk_remedy, throttled_now, reclaimed_bytes);
        let answer =
            deps.manvi_complete(selection, STEWARD_SYSTEM_PROMPT, &digest, MAX_OUTPUT_TOKENS);
        // Only a cloud call says anything about the Claude key: success clears
        // a rejection, Anthropic's refusal of the key sets it, and any other
        // failure (or a local model's answer) leaves it as it was.
        if manvi.mode == "cloud" {
            record_cloud_key_answer(
                &mut crate::supervisor::lock_or_recover(&rt.inner),
                &manvi.model,
                key_answer_of_call(&answer),
            );
        }
        match answer {
            Ok(text) => (text, true),
            Err(err) => {
                warnings.push(format!(
                    "Manvi completion did not produce a directive ({err}); sent deterministic directive."
                ));
                (
                    deterministic_directive(
                        tier,
                        binding,
                        disk_remedy,
                        free_disk,
                        throttled_now,
                        reclaimed_bytes,
                    ),
                    false,
                )
            }
        }
    };

    // 4. Message active agents across trusted repositories, respecting per-repo cooldowns.
    let mut messaged_sessions = 0u32;
    let mut messaged_repositories = 0u32;
    let cooldown_ms = settings
        .interval_secs
        .clamp(STEWARD_MIN_INTERVAL_SECS, STEWARD_MAX_INTERVAL_SECS)
        .saturating_mul(1_000);
    let cur_rank = tier_rank(tier);
    for (bus_key, repo_root) in &message_targets {
        let should_send = trigger == "manual" || {
            let guard = crate::supervisor::lock_or_recover(&rt.inner);
            match guard.last_messaged.get(bus_key) {
                None => true,
                Some((last_ms, prev_rank, prev_binding)) => {
                    // Send immediately when severity escalated or binding changed;
                    // otherwise respect the configured interval cooldown.
                    cur_rank > *prev_rank
                        || prev_binding != b_key
                        || now_ms.saturating_sub(*last_ms) >= cooldown_ms
                }
            }
        };
        if !should_send {
            continue;
        }
        let msg_id = format!(
            "steward-{now_ms}-{}",
            SEQUENCE.fetch_add(1, Ordering::Relaxed) + 1
        );
        match deps.deliver_repo_directive(
            repo_root,
            &msg_id,
            &manvi.model,
            &manvi.mode,
            t_name,
            b_key,
            &directive,
        ) {
            Ok(0) => {}
            Ok(count) => {
                messaged_repositories = messaged_repositories.saturating_add(1);
                messaged_sessions = messaged_sessions.saturating_add(count);
                let mut guard = crate::supervisor::lock_or_recover(&rt.inner);
                guard
                    .last_messaged
                    .insert(bus_key.clone(), (now_ms, cur_rank, b_key.to_string()));
            }
            Err(err) => warnings.push(format!("{repo_root}: could not message agents ({err})")),
        }
    }

    let summary = format!(
        "Host {} ({}); messaged {} session(s) across {} repository(ies), throttled {} process(es), reclaimed {}.",
        t_name,
        binding.map_or("load", Binding::cause_phrase),
        messaged_sessions,
        messaged_repositories,
        throttled_now,
        format_gib(Some(reclaimed_bytes)),
    );

    let record = StewardTickRecord {
        id,
        trigger: trigger.to_string(),
        ran_at_ms: now_ms,
        outcome: "acted".into(),
        tier: t_name.to_string(),
        binding: b_key.to_string(),
        manvi_mode: manvi.mode.clone(),
        manvi_model: manvi.model,
        model_generated,
        summary,
        directive,
        messaged_sessions,
        messaged_repositories,
        skipped_untrusted_repositories: skipped_untrusted,
        throttled_processes: throttled_now,
        restored_processes: restored_now,
        reclaimed_bytes,
        cleaned_items,
        warnings,
    };
    store_tick(rt, now_ms, record.clone());
    record
}

fn store_tick(rt: &StewardRuntime, now_ms: u64, record: StewardTickRecord) {
    let mut guard = crate::supervisor::lock_or_recover(&rt.inner);
    guard.last_check_ms = Some(now_ms);
    guard.history.insert(0, record);
    guard.history.truncate(MAX_HISTORY);
}

#[cfg(all(test, unix))]
mod live_key_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sysload::{
        derive_profile, AgentKind, Clock, GovernorReport, GovernorVerdict, HostSample, Level,
        Platform, ProfileChoice, Reading, Reason, Role, SensorReport, Shed, Snapshot, TickStats,
        WriterIdentity,
    };
    use crate::tool_config::RepoStewardOverride;
    use std::cell::RefCell;

    struct FakeDeps {
        now_ms: u64,
        config_dir: PathBuf,
        settings: RefCell<ManviStewardSettings>,
        env_var: Option<String>,
        manvi: ManviProbe,
        manvi_probes: RefCell<usize>,
        load: HostLoadStatus,
        disk_remedy: bool,
        repos: Vec<String>,
        cleanup_repos: Vec<String>,
        trusted: BTreeSet<String>,
        common_dirs: HashMap<String, String>,
        llm_reply: Result<String, ModelCallError>,
        llm_calls: RefCell<usize>,
        active_sessions_by_repo: HashMap<String, u32>,
        delivered: RefCell<Vec<(String, String)>>,
        candidates: Vec<AgentThrottleCandidate>,
        on_throttle_scan: Option<ThrottleScanHook>,
        throttle_ops: RefCell<Vec<(i32, bool)>>,
        /// Pids already at background priority: throttling one answers `Ok(false)`.
        already_background: RefCell<BTreeSet<i32>>,
        clean_result: CleanPassOutcome,
        clean_calls: RefCell<Vec<CleanCall>>,
    }

    /// Runs during the throttle scan, to change settings mid-tick.
    type ThrottleScanHook = Box<dyn Fn(&RefCell<ManviStewardSettings>)>;
    /// `(cleanable repos, shared-cache anchor, clean shared caches)` per clean call.
    type CleanCall = (Vec<String>, Option<String>, bool);

    fn make_load(
        state: SensorState,
        tier: Tier,
        binding: Option<Binding>,
        admit: bool,
    ) -> HostLoadStatus {
        let mut snap = Snapshot::empty(Clock::now(), WriterIdentity::default(), true);
        snap.host = HostSample {
            free_disk: Reading::Value(8 << 30),
            memsize: Reading::Value(64 << 30),
            swap_used: Reading::Value(12 << 30),
            ..snap.host
        };
        HostLoadStatus {
            platform: Platform::Governed,
            role: Role::Writer,
            sensor: SensorReport {
                state,
                age_ms: Some(400),
                level: Some(match tier {
                    Tier::Clear => Level::Normal,
                    Tier::Elevated => Level::Warn,
                    Tier::Severe => Level::Critical,
                }),
                enabled: Some(true),
                reason: None,
            },
            pressure: match tier {
                Tier::Clear => PressureInput::Normal,
                Tier::Elevated => PressureInput::Warn,
                Tier::Severe => PressureInput::Critical,
            },
            governor: GovernorReport {
                verdict: GovernorVerdict {
                    admit,
                    shed: Shed {
                        background: tier >= Tier::Elevated,
                        reactive: tier >= Tier::Severe,
                        cause: binding,
                    },
                    reason: if tier == Tier::Clear {
                        Reason::Healthy
                    } else {
                        Reason::Pressure
                    },
                    tier,
                    binding,
                    sensor: state,
                    age_ms: Some(400),
                    message: "simulated load".into(),
                    warnings: Vec::new(),
                },
                exec_latency: None,
                disk_fill: None,
                refusing_for_s: None,
                notice: false,
                largest_foreign: None,
                override_remaining_s: None,
                advisory: None,
            },
            profile: derive_profile(Reading::Value(64 << 30), Some(16), ProfileChoice::Auto),
            snapshot_path: None,
            writer: None,
            writer_is_self: true,
            snapshot: Some(snap),
            procs: None,
            tick: TickStats::default(),
            problems: Vec::new(),
            previous_boot: None,
        }
    }

    fn cloud_manvi() -> ManviStatus {
        ManviStatus {
            state: "ready".into(),
            mode: "cloud".into(),
            provider: "anthropic".into(),
            provider_from: "config".into(),
            model: "claude-sonnet-4-6".into(),
            model_from: "config".into(),
            selection: Some(json!({"provider": "anthropic", "model": "claude-sonnet-4-6"})),
            typed: Some(ModelSelection::Claude {
                model: "claude-sonnet-4-6".into(),
            }),
            ..ManviStatus::default()
        }
    }

    fn local_manvi() -> ManviStatus {
        ManviStatus {
            state: "ready".into(),
            mode: "local".into(),
            provider: "local".into(),
            provider_from: "config".into(),
            model: "qwen3:14b".into(),
            local_base_url: "http://127.0.0.1:11434/v1".into(),
            selection: Some(
                json!({"provider": "local", "base_url": "http://127.0.0.1:11434/v1", "model": "qwen3:14b"}),
            ),
            typed: Some(ModelSelection::Local {
                base_url: "http://127.0.0.1:11434/v1".into(),
                model: "qwen3:14b".into(),
            }),
            ..ManviStatus::default()
        }
    }

    impl StewardDeps for FakeDeps {
        fn now_ms(&self) -> u64 {
            self.now_ms
        }
        fn config_dir(&self) -> Option<PathBuf> {
            Some(self.config_dir.clone())
        }
        fn settings(&self) -> ManviStewardSettings {
            self.settings.borrow().clone()
        }
        fn env_var(&self) -> Option<String> {
            self.env_var.clone()
        }
        fn manvi_probe(&self, _disk_remedy: bool) -> ManviProbe {
            *self.manvi_probes.borrow_mut() += 1;
            self.manvi.clone()
        }
        fn host_load(&self) -> HostLoadStatus {
            self.load.clone()
        }
        fn disk_remedy_due(&self) -> bool {
            self.disk_remedy
        }
        fn candidate_repos(&self, _load: &HostLoadStatus, _explicit: Option<&str>) -> Vec<String> {
            self.repos.clone()
        }
        fn cleanup_candidate_repos(
            &self,
            _load: &HostLoadStatus,
            _explicit: Option<&str>,
        ) -> Vec<String> {
            if self.cleanup_repos.is_empty() {
                self.repos.clone()
            } else {
                self.cleanup_repos.clone()
            }
        }
        fn repo_trusted(&self, repo_root: &str) -> bool {
            self.trusted.contains(repo_root)
        }
        fn repo_common_dir(&self, path: &str) -> Option<String> {
            self.common_dirs.get(path).cloned()
        }
        fn manvi_complete(
            &self,
            _selection: &ModelSelection,
            _system: &str,
            _user: &str,
            _max_tokens: i64,
        ) -> Result<String, ModelCallError> {
            *self.llm_calls.borrow_mut() += 1;
            self.llm_reply.clone()
        }
        fn deliver_repo_directive(
            &self,
            repo_root: &str,
            _msg_id: &str,
            _model: &str,
            _mode: &str,
            _tier: &str,
            _binding: &str,
            directive: &str,
        ) -> Result<u32, String> {
            let count = self
                .active_sessions_by_repo
                .get(repo_root)
                .copied()
                .unwrap_or(2);
            if count > 0 {
                self.delivered
                    .borrow_mut()
                    .push((repo_root.to_string(), directive.to_string()));
            }
            Ok(count)
        }
        fn throttle_candidates(&self) -> Result<Vec<AgentThrottleCandidate>, String> {
            let out = self.candidates.clone();
            if let Some(cb) = &self.on_throttle_scan {
                cb(&self.settings);
            }
            Ok(out)
        }
        fn set_process_throttle(
            &self,
            pid: i32,
            _start_us: u64,
            enable: bool,
        ) -> Result<bool, String> {
            self.throttle_ops.borrow_mut().push((pid, enable));
            Ok(!(enable && self.already_background.borrow().contains(&pid)))
        }
        fn write_active_lease(
            &self,
            dir: &Path,
            record: &ActiveThrottleRecord,
        ) -> Result<(), String> {
            write_active_throttle(dir, record)
        }
        fn clear_active_lease(&self, dir: &Path) -> Result<(), String> {
            clear_active_throttle(dir)
        }
        fn clean_critical_storage(&self, req: &CriticalCleanRequest<'_>) -> CleanPassOutcome {
            self.clean_calls.borrow_mut().push((
                req.trusted_cleanable_repos.to_vec(),
                req.trusted_anchor_repo.map(str::to_owned),
                req.clean_shared_caches,
            ));
            self.clean_result.clone()
        }
    }

    fn base_deps(dir: &Path) -> FakeDeps {
        FakeDeps {
            now_ms: 1_800_000_000_000,
            config_dir: dir.to_path_buf(),
            // A person who switched cleaning and slowing on: the cases these
            // tests exercise. `out_of_the_box_...` covers the defaults.
            settings: RefCell::new(ManviStewardSettings {
                slow_critical: true,
                clean_critical: true,
                ..ManviStewardSettings::default()
            }),
            env_var: None,
            manvi: ManviProbe::Answered(Box::new(cloud_manvi())),
            manvi_probes: RefCell::new(0),
            load: make_load(SensorState::Fresh, Tier::Clear, None, true),
            disk_remedy: false,
            repos: vec!["/trusted/a".into(), "/untrusted/b".into()],
            cleanup_repos: Vec::new(),
            trusted: BTreeSet::from(["/trusted/a".to_string()]),
            common_dirs: HashMap::from([("/trusted/a".to_string(), "/trusted/a/.git".to_string())]),
            llm_reply: Ok("CPU load is elevated. Cap cargo builds to 2 jobs.".into()),
            llm_calls: RefCell::new(0),
            active_sessions_by_repo: HashMap::new(),
            delivered: RefCell::new(Vec::new()),
            candidates: Vec::new(),
            on_throttle_scan: None,
            throttle_ops: RefCell::new(Vec::new()),
            already_background: RefCell::new(BTreeSet::new()),
            clean_result: CleanPassOutcome::default(),
            clean_calls: RefCell::new(Vec::new()),
        }
    }

    #[test]
    fn stands_down_honestly_when_manvi_unconfigured_or_sensor_dead() {
        let tmp = tempfile::tempdir().unwrap();
        let rt = StewardRuntime::default();
        let mut deps = base_deps(tmp.path());
        deps.manvi = ManviProbe::Answered(Box::new(ManviStatus {
            state: "not_chosen".into(),
            detail: "Choose a model first.".into(),
            ..ManviStatus::default()
        }));
        let rec = evaluate_tick_with(&rt, "scheduled", None, &deps);
        assert_eq!(rec.outcome, "manvi_unconfigured");
        assert!(!rec.model_generated);
        assert_eq!(*deps.llm_calls.borrow(), 0);

        // Sensor dead must report sensor_unavailable, never healthy.
        deps.manvi = ManviProbe::Answered(Box::new(cloud_manvi()));
        deps.load = make_load(SensorState::Dead, Tier::Clear, None, true);
        let rec = evaluate_tick_with(&rt, "scheduled", None, &deps);
        assert_eq!(rec.outcome, "sensor_unavailable");
        assert_eq!(*deps.llm_calls.borrow(), 0);
    }

    #[test]
    fn deferred_manvi_probe_under_severe_load_reuses_cached_selection_or_preserves_active_throttles(
    ) {
        let tmp = tempfile::tempdir().unwrap();
        let rt = StewardRuntime::default();
        let mut deps = base_deps(tmp.path());

        // Cold deferral with no cached Manvi status reports `manvi_deferred` (never
        // `manvi_unconfigured`) and does NOT clear an active lease.
        write_active_throttle(
            tmp.path(),
            &ActiveThrottleRecord {
                version: LEASE_VERSION,
                writer_pid: std::process::id(),
                writer_start_us: 0,
                issued_at_ms: deps.now_ms,
                expires_at_ms: deps.now_ms + 120_000,
                tier: "severe".into(),
                binding: "memory_pressure".into(),
                reason: "kernel pressure".into(),
                pace_ms: 1_000,
                repositories: Vec::new(),
            },
        )
        .unwrap();
        deps.manvi = ManviProbe::Deferred("shed under pressure".into());
        let cold = evaluate_tick_with(&rt, "critical_wake", None, &deps);
        assert_eq!(cold.outcome, "manvi_deferred");
        assert!(
            tmp.path().join(STEWARD_SUBDIR).join(ACTIVE_FILE).is_file(),
            "cold deferred probe must not tear down an active lease"
        );

        // Once a calm tick caches `manvi use`, a subsequent severe-load deferral
        // reuses the cached model and still enforces critical slowdowns and directives!
        deps.manvi = ManviProbe::Answered(Box::new(cloud_manvi()));
        assert_eq!(
            evaluate_tick_with(&rt, "scheduled", None, &deps).outcome,
            "healthy"
        );

        deps.now_ms += 30_000;
        deps.manvi = ManviProbe::Deferred("shed under pressure".into());
        deps.load = make_load(
            SensorState::Fresh,
            Tier::Severe,
            Some(Binding::MemoryPressure),
            false,
        );
        deps.candidates = vec![AgentThrottleCandidate {
            pid: 4200,
            start_us: 77,
            name: "rustc".into(),
            session_pid: 4000,
            session_kind: AgentKind::Claude,
            session_cwd: Some("/trusted/a".into()),
            is_session_root: false,
            lineage_cwds: Vec::new(),
            ancestor_pids: Vec::new(),
        }];
        let acted = evaluate_tick_with(&rt, "critical_wake", None, &deps);
        assert_eq!(acted.outcome, "acted");
        assert_eq!(acted.throttled_processes, 1);
        assert_eq!(acted.messaged_repositories, 1);
        assert!(acted.warnings.iter().any(|w| w.contains("cached Manvi")));
    }

    /// The premise of the pressure relief (`sysload::relief`), pinned: with
    /// Manvi unconfigured, or its probe deferred before it ever answered, a
    /// severe memory-bound tick throttles nothing here. The slowing and
    /// freezing under that pressure is the relief's, which runs on the
    /// sampler's tick and asks neither.
    #[test]
    fn the_steward_alone_throttles_nothing_without_manvi_which_is_why_the_relief_does_not_ask_it() {
        let candidate = AgentThrottleCandidate {
            pid: 4200,
            start_us: 77,
            name: "rustc".into(),
            session_pid: 4000,
            session_kind: AgentKind::Claude,
            session_cwd: Some("/trusted/a".into()),
            is_session_root: false,
            lineage_cwds: Vec::new(),
            ancestor_pids: Vec::new(),
        };
        let severe = || {
            make_load(
                SensorState::Fresh,
                Tier::Severe,
                Some(Binding::MemoryPressure),
                false,
            )
        };

        let tmp = tempfile::tempdir().unwrap();
        let rt = StewardRuntime::default();
        let mut deps = base_deps(tmp.path());
        deps.candidates = vec![candidate.clone()];
        deps.load = severe();
        deps.manvi = ManviProbe::Answered(Box::new(ManviStatus {
            state: "not_chosen".into(),
            ..ManviStatus::default()
        }));
        let unconfigured = evaluate_tick_with(&rt, "critical_wake", None, &deps);
        assert_eq!(unconfigured.outcome, "manvi_unconfigured");
        assert_eq!(unconfigured.throttled_processes, 0);
        assert!(
            deps.throttle_ops.borrow().iter().all(|(_, enable)| !enable),
            "nothing was slowed: {:?}",
            deps.throttle_ops.borrow()
        );

        let tmp = tempfile::tempdir().unwrap();
        let rt = StewardRuntime::default();
        let mut deps = base_deps(tmp.path());
        deps.candidates = vec![candidate];
        deps.load = severe();
        deps.manvi = ManviProbe::Deferred("shed under pressure".into());
        let deferred = evaluate_tick_with(&rt, "critical_wake", None, &deps);
        assert_eq!(deferred.outcome, "manvi_deferred");
        assert_eq!(deferred.throttled_processes, 0);
        assert!(deps.throttle_ops.borrow().is_empty());
    }

    #[test]
    fn healthy_tick_makes_no_model_call_and_dedupes_worktrees_by_common_dir_and_skips_empty_repos()
    {
        let tmp = tempfile::tempdir().unwrap();
        let rt = StewardRuntime::default();
        let mut deps = base_deps(tmp.path());
        // Add a linked worktree sharing `/trusted/a/.git` plus an idle watched repo `/trusted/empty`.
        deps.repos = vec![
            "/trusted/a".into(),
            "/trusted/a/.gitpulse/worktrees/lane-1".into(),
            "/trusted/empty".into(),
            "/untrusted/b".into(),
        ];
        deps.trusted
            .insert("/trusted/a/.gitpulse/worktrees/lane-1".into());
        deps.trusted.insert("/trusted/empty".into());
        deps.common_dirs.insert(
            "/trusted/a/.gitpulse/worktrees/lane-1".into(),
            "/trusted/a/.git".into(),
        );
        deps.common_dirs
            .insert("/trusted/empty".into(), "/trusted/empty/.git".into());
        deps.active_sessions_by_repo
            .insert("/trusted/empty".into(), 0);

        let clear = evaluate_tick_with(&rt, "scheduled", None, &deps);
        assert_eq!(clear.outcome, "healthy");
        assert_eq!(*deps.llm_calls.borrow(), 0);

        deps.load = make_load(
            SensorState::Fresh,
            Tier::Elevated,
            Some(Binding::CpuLoad),
            true,
        );
        let acted = evaluate_tick_with(&rt, "scheduled", None, &deps);
        assert_eq!(acted.outcome, "acted");
        assert!(acted.model_generated);
        // Two worktrees sharing `/trusted/a/.git` deliver ONCE, and `/trusted/empty`
        // with 0 active sessions is neither counted nor locked into cooldown.
        assert_eq!(acted.messaged_repositories, 1);
        assert_eq!(acted.messaged_sessions, 2);
        assert_eq!(deps.delivered.borrow().len(), 1);
        assert_eq!(acted.skipped_untrusted_repositories, 1);
        assert_eq!(*deps.llm_calls.borrow(), 1);

        // Immediate repeat at the same tier/binding within interval_secs does not spam `/trusted/a`,
        // while `/trusted/empty` delivers immediately once an active session starts in it!
        deps.now_ms += 10_000;
        deps.active_sessions_by_repo
            .insert("/trusted/empty".into(), 1);
        let repeat = evaluate_tick_with(&rt, "scheduled", None, &deps);
        assert_eq!(repeat.messaged_repositories, 1);
        assert_eq!(repeat.messaged_sessions, 1);

        // Escalation to Severe breaks through `/trusted/a`'s cooldown immediately.
        deps.now_ms += 5_000;
        deps.load = make_load(
            SensorState::Fresh,
            Tier::Severe,
            Some(Binding::CpuLoad),
            false,
        );
        let escalated = evaluate_tick_with(&rt, "critical_wake", None, &deps);
        assert_eq!(escalated.messaged_repositories, 2);
    }

    #[test]
    fn critical_memory_pressure_skips_local_model_throttles_agents_and_restores_on_recovery() {
        let tmp = tempfile::tempdir().unwrap();
        let rt = StewardRuntime::default();
        let mut deps = base_deps(tmp.path());
        deps.manvi = ManviProbe::Answered(Box::new(local_manvi()));
        deps.load = make_load(
            SensorState::Fresh,
            Tier::Severe,
            Some(Binding::MemoryPressure),
            false,
        );
        deps.candidates = vec![
            AgentThrottleCandidate {
                pid: 4100,
                start_us: 99,
                name: "rustc".into(),
                session_pid: 4000,
                session_kind: AgentKind::Claude,
                session_cwd: Some("/trusted/a".into()),
                is_session_root: false,
                lineage_cwds: Vec::new(),
                ancestor_pids: Vec::new(),
            },
            AgentThrottleCandidate {
                pid: 5100,
                start_us: 88,
                name: "cargo".into(),
                session_pid: 5000,
                session_kind: AgentKind::Codex,
                session_cwd: Some("/opted_out/c".into()),
                is_session_root: false,
                lineage_cwds: Vec::new(),
                ancestor_pids: Vec::new(),
            },
        ];
        deps.common_dirs
            .insert("/opted_out/c".into(), "/opted_out/c/.git".into());
        deps.settings.borrow_mut().repositories.insert(
            "/opted_out/c/.git".into(),
            RepoStewardOverride {
                slow_critical: Some(false),
                ..RepoStewardOverride::default()
            },
        );

        let critical = evaluate_tick_with(&rt, "critical_wake", None, &deps);
        assert_eq!(critical.outcome, "acted");
        assert_eq!(*deps.llm_calls.borrow(), 0);
        assert!(!critical.model_generated);
        assert!(critical
            .warnings
            .iter()
            .any(|w| w.contains("local model server")));
        assert_eq!(critical.throttled_processes, 1);
        assert_eq!(*deps.throttle_ops.borrow(), vec![(4100, true)]);
        let _ = published_lease(tmp.path(), &critical);

        // Recovery to Clear restores pid 4100 and deletes active.json!
        deps.now_ms += 60_000;
        deps.load = make_load(SensorState::Fresh, Tier::Clear, None, true);
        let recovered = evaluate_tick_with(&rt, "scheduled", None, &deps);
        assert_eq!(recovered.outcome, "healthy");
        assert_eq!(recovered.restored_processes, 1);
        assert_eq!(
            *deps.throttle_ops.borrow(),
            vec![(4100, true), (4100, false)]
        );
        assert!(!tmp.path().join(STEWARD_SUBDIR).join(ACTIVE_FILE).exists());
    }

    #[test]
    fn concurrent_settings_disable_during_in_flight_tick_never_leaves_stale_lease_or_throttled_pids(
    ) {
        let tmp = tempfile::tempdir().unwrap();
        let rt = StewardRuntime::default();
        let mut deps = base_deps(tmp.path());
        deps.load = make_load(
            SensorState::Fresh,
            Tier::Severe,
            Some(Binding::MemoryPressure),
            false,
        );
        deps.candidates = vec![AgentThrottleCandidate {
            pid: 6100,
            start_us: 55,
            name: "cargo".into(),
            session_pid: 6000,
            session_kind: AgentKind::Claude,
            session_cwd: Some("/trusted/a".into()),
            is_session_root: false,
            lineage_cwds: Vec::new(),
            ancestor_pids: Vec::new(),
        }];
        // Simulate the user switching `slow_critical = false` mid-tick while the
        // candidate scan is in flight, before `active.json` is written.
        deps.on_throttle_scan = Some(Box::new(|settings| {
            settings.borrow_mut().slow_critical = false;
        }));

        let rec = evaluate_tick_with(&rt, "critical_wake", None, &deps);
        assert_eq!(rec.throttled_processes, 0);
        assert_eq!(rec.restored_processes, 1);
        assert_eq!(
            *deps.throttle_ops.borrow(),
            vec![(6100, true), (6100, false)]
        );
        assert!(
            !tmp.path().join(STEWARD_SUBDIR).join(ACTIVE_FILE).exists(),
            "in-flight tick must not publish active.json after slow_critical was disabled"
        );
        assert!(!crate::supervisor::lock_or_recover(&rt.inner).critical_throttle_active);
    }

    #[test]
    fn global_disabled_with_non_slow_repo_override_never_arms_false_positive_lease_and_env_force_on_works(
    ) {
        let tmp = tempfile::tempdir().unwrap();
        let rt = StewardRuntime::default();
        let mut deps = base_deps(tmp.path());
        deps.load = make_load(
            SensorState::Fresh,
            Tier::Severe,
            Some(Binding::CpuLoad),
            false,
        );
        {
            let mut s = deps.settings.borrow_mut();
            s.enabled = false;
            s.slow_critical = true;
            s.clean_critical = true;
            s.repositories.insert(
                "/trusted/a/.git".into(),
                RepoStewardOverride {
                    enabled: Some(true),
                    message_agents: Some(true),
                    slow_critical: Some(false),
                    clean_critical: Some(false),
                },
            );
        }

        let rec = evaluate_tick_with(&rt, "critical_wake", None, &deps);
        assert_eq!(rec.outcome, "acted");
        assert_eq!(rec.throttled_processes, 0);
        assert!(!tmp.path().join(STEWARD_SUBDIR).join(ACTIVE_FILE).exists());
        assert!(!crate::supervisor::lock_or_recover(&rt.inner).critical_throttle_active);

        // Verify `active_throttle_for_cwd_with_env` honors `GITPULSE_MANVI_STEWARD=1`
        // even when `tools.json` has `enabled: false` (and `slow_critical: true`).
        let repo_holder = crate::test_support::git_repo();
        let repo_dir = repo_holder.path().to_path_buf();
        let disabled_cfg = serde_json::json!({
            "version": 1,
            "manvi_steward": {
                "enabled": false,
                "interval_secs": 180,
                "message_agents": true,
                "slow_critical": true,
                "clean_critical": true,
                "clean_retention_days": 7,
                "repositories": {}
            }
        });
        fs::write(
            tmp.path().join("tools.json"),
            serde_json::to_vec(&disabled_cfg).unwrap(),
        )
        .unwrap();
        write_active_throttle(
            tmp.path(),
            &ActiveThrottleRecord {
                version: LEASE_VERSION,
                writer_pid: std::process::id(),
                writer_start_us: 0,
                issued_at_ms: deps.now_ms,
                expires_at_ms: deps.now_ms + 60_000,
                tier: "severe".into(),
                binding: "cpu_load".into(),
                reason: "high load".into(),
                pace_ms: 1_000,
                repositories: vec![canonical_common_dir(repo_dir.to_str().unwrap()).unwrap()],
            },
        )
        .unwrap();
        let cwd_str = repo_dir.to_str().unwrap();
        assert!(active_throttle_for_cwd_with_env(tmp.path(), cwd_str, deps.now_ms, None).is_none());
        assert!(
            active_throttle_for_cwd_with_env(tmp.path(), cwd_str, deps.now_ms, Some("1")).is_some()
        );
        assert!(
            active_throttle_for_cwd_with_env(tmp.path(), cwd_str, deps.now_ms, Some("0")).is_none()
        );
    }

    #[test]
    fn critical_low_storage_runs_hygiene_cleanup_with_cooldown_and_manual_bypass_and_retries_when_busy(
    ) {
        let tmp = tempfile::tempdir().unwrap();
        let rt = StewardRuntime::default();
        let mut deps = base_deps(tmp.path());
        deps.load = make_load(
            SensorState::Fresh,
            Tier::Severe,
            Some(Binding::LowDisk),
            false,
        );
        deps.disk_remedy = true;

        // If the cleaner lock is busy (`deferred_or_busy = true` and 0 bytes reclaimed),
        // the 30m cooldown is NOT stamped so the next scheduled tick retries!
        deps.clean_result = CleanPassOutcome {
            reclaimed_bytes: 0,
            items: Vec::new(),
            warnings: vec!["cleaner busy".into()],
            deferred_or_busy: true,
        };
        let busy_tick = evaluate_tick_with(&rt, "scheduled", None, &deps);
        assert_eq!(busy_tick.reclaimed_bytes, 0);
        assert_eq!(deps.clean_calls.borrow().len(), 1);

        deps.now_ms += 180_000;
        deps.clean_result = CleanPassOutcome {
            reclaimed_bytes: 4 << 30,
            items: vec![StewardCleanItem {
                repo_path: "/trusted/a".into(),
                target: "local:target".into(),
                status: "reclaimed".into(),
                measured_bytes: 4 << 30,
                bytes_after: Some(0),
                detail: "Removed idle Cargo target".into(),
            }],
            warnings: Vec::new(),
            deferred_or_busy: false,
        };
        let first = evaluate_tick_with(&rt, "scheduled", None, &deps);
        assert_eq!(first.reclaimed_bytes, 4 << 30);
        assert_eq!(deps.clean_calls.borrow().len(), 2);

        // Next scheduled tick 3 minutes later skips automatic cleanup due to 30m cooldown.
        deps.now_ms += 180_000;
        let second = evaluate_tick_with(&rt, "scheduled", None, &deps);
        assert_eq!(second.reclaimed_bytes, 0);
        assert_eq!(deps.clean_calls.borrow().len(), 2);

        // Manual "Run check" bypasses the automatic cooldown.
        deps.now_ms += 5_000;
        let manual = evaluate_tick_with(&rt, "manual", None, &deps);
        assert_eq!(manual.reclaimed_bytes, 4 << 30);
        assert_eq!(deps.clean_calls.borrow().len(), 3);
    }

    #[cfg(unix)]
    #[test]
    fn real_critical_hygiene_pass_reclaims_ignored_target_and_agent_scratch_under_severe_disk_governor(
    ) {
        let _serial = crate::storage::hygiene::tests::serial();
        use crate::procguard::LockedSpawn;
        use crate::sysload::{
            inject_governor_with_history, GovernorInput, Level, Reading, Readings,
        };

        let tmp = tempfile::tempdir().unwrap();
        let repo = crate::engine::process::canonicalize_plain(tmp.path()).unwrap();
        assert!(std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&repo)
            .status_locked()
            .unwrap()
            .success());
        fs::write(repo.join(".gitignore"), "/target/\n/.gitpulse/\n").unwrap();
        fs::write(
            repo.join("Cargo.toml"),
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        assert!(std::process::Command::new("git")
            .args([
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "add",
                ".gitignore",
                "Cargo.toml",
            ])
            .current_dir(&repo)
            .status_locked()
            .unwrap()
            .success());
        assert!(std::process::Command::new("git")
            .args([
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-q",
                "-m",
                "init"
            ])
            .current_dir(&repo)
            .status_locked()
            .unwrap()
            .success());

        let repo_str = repo.to_string_lossy().into_owned();
        let preview = crate::repository_trust::inspect(&repo_str).unwrap();
        crate::repository_trust::grant(&repo_str, &preview.identity, false).unwrap();

        // 1. Create a Cargo `target/` directory at the repository root and age it 10 days.
        let cargo_target = repo.join("target");
        fs::create_dir_all(cargo_target.join("debug")).unwrap();
        fs::write(cargo_target.join("debug/.cargo-lock"), "").unwrap();
        fs::write(
            cargo_target.join("CACHEDIR.TAG"),
            "Signature: 8a477f597d28d172789f06886806bc55\n",
        )
        .unwrap();
        fs::write(cargo_target.join(".rustc_info.json"), "{}").unwrap();
        fs::write(cargo_target.join("debug/libdemo.rlib"), vec![b'a'; 8192]).unwrap();

        // 2. Create an agent scratch Cargo target under `.gitpulse/scratch/lane-1/target` and age it 10 days.
        let scratch_target = repo.join(".gitpulse/scratch/lane-1/target");
        fs::create_dir_all(scratch_target.join("debug")).unwrap();
        fs::write(scratch_target.join("debug/.cargo-lock"), "").unwrap();
        fs::write(
            scratch_target.join("CACHEDIR.TAG"),
            "Signature: 8a477f597d28d172789f06886806bc55\n",
        )
        .unwrap();
        fs::write(scratch_target.join(".rustc_info.json"), "{}").unwrap();
        fs::write(
            scratch_target.join("debug/libscratch.rlib"),
            vec![b'b'; 4096],
        )
        .unwrap();

        let ten_days_ago = SystemTime::now() - Duration::from_secs(10 * 86_400);
        for p in [
            cargo_target.join("debug/.cargo-lock"),
            cargo_target.join("debug/libdemo.rlib"),
            cargo_target.join("debug"),
            cargo_target.join("CACHEDIR.TAG"),
            cargo_target.join(".rustc_info.json"),
            cargo_target.clone(),
            scratch_target.join("debug/.cargo-lock"),
            scratch_target.join("debug/libscratch.rlib"),
            scratch_target.join("debug"),
            scratch_target.join("CACHEDIR.TAG"),
            scratch_target.join(".rustc_info.json"),
            scratch_target.clone(),
            repo.join(".gitpulse/scratch/lane-1"),
            repo.join(".gitpulse/scratch"),
            repo.join(".gitpulse"),
        ] {
            crate::test_support::set_modified(&p, ten_days_ago).unwrap();
        }

        // Inject a REAL Severe LowDisk + Critical governor so `SpawnGate` sheds every
        // reactive spawn that lacks `with_disk_relief`!
        let severe_disk = GovernorInput {
            readings: Readings {
                free_disk: Reading::Value(4 << 30),
                ..Readings::calm()
            },
            ..GovernorInput::test_pressure(Level::Critical)
        };
        let _gov = inject_governor_with_history(std::iter::repeat_n(severe_disk, 10));
        assert!(crate::sysload::disk_remedy_due());

        let out = run_critical_hygiene_pass(std::slice::from_ref(&repo_str), false, 7);
        assert!(
            out.reclaimed_bytes >= 12_000,
            "expected >= 12 KiB reclaimed under Severe LowDisk governor, got {out:?}"
        );
        assert!(
            !cargo_target.exists(),
            "ignored idle target/ was removed: {out:?}"
        );
        assert!(
            !scratch_target.exists(),
            "ignored idle .gitpulse/scratch/.../target was removed: {out:?}"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn real_child_process_background_throttle_applies_and_restores_on_macos() {
        use crate::procguard::LockedSpawn;
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("5")
            .spawn_locked()
            .unwrap();
        let pid_u32 = child.id();
        let pid = i32::try_from(pid_u32).unwrap();
        let start_us = crate::sysload::process_start_us(pid_u32).expect("child start_us");

        // Wrong start_us must fail closed and touch nothing.
        assert!(
            crate::sysload::procscan::set_background_throttle(pid, start_us + 1, true).is_err()
        );

        // Apply Darwin background throttle, verify kernel task priority drops to
        // MAXPRI_THROTTLE (<= 4), verify idempotence, then restore back above throttle.
        let initial_pri = crate::sysload::procscan::darwin_task_priority(pid).unwrap();
        assert!(
            initial_pri > 4,
            "expected default priority > 4, got {initial_pri}"
        );
        let changed =
            crate::sysload::procscan::set_background_throttle(pid, start_us, true).unwrap();
        assert!(changed, "fresh sleep child starts unthrottled");
        let throttled_pri = crate::sysload::procscan::darwin_task_priority(pid).unwrap();
        assert!(
            throttled_pri <= 4,
            "expected MAXPRI_THROTTLE <= 4, got {throttled_pri}"
        );
        let again = crate::sysload::procscan::set_background_throttle(pid, start_us, true).unwrap();
        assert!(!again, "already throttled returns Ok(false)");
        let restored =
            crate::sysload::procscan::set_background_throttle(pid, start_us, false).unwrap();
        assert!(restored, "restoring revokes PRIO_DARWIN_BG");
        let restored_pri = crate::sysload::procscan::darwin_task_priority(pid).unwrap();
        assert_eq!(restored_pri, initial_pri, "priority restored to original");
        let restore_again =
            crate::sysload::procscan::set_background_throttle(pid, start_us, false).unwrap();
        assert!(!restore_again, "already restored returns Ok(false)");

        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn unmocked_deliver_directive_to_repo_posts_system_steward_origin_aligns_mcp_inbox_and_blocks_agent_spoofing(
    ) {
        use crate::agent_mailbox::Delivery;
        use crate::engine::agent_bus::{
            PeerUpsert, STEWARD_MESSAGE_KIND, STEWARD_SENDER, STEWARD_THREAD_KEY,
        };
        use crate::mcp::a2a::{handle_agent_inbox, handle_agent_register, handle_agent_send};
        use crate::mcp::{AgentInboxArgs, AgentRegisterArgs, AgentSendArgs};
        use crate::procguard::LockedSpawn;

        let tmp = tempfile::tempdir().unwrap();
        let repo = crate::engine::process::canonicalize_plain(tmp.path()).unwrap();
        let repo_str = repo.to_string_lossy().into_owned();
        for args in [
            vec!["init", "-q", "-b", "main"],
            vec!["config", "user.name", "Steward Test"],
            vec!["config", "user.email", "steward@example.com"],
            vec!["commit", "-q", "--allow-empty", "-m", "init"],
        ] {
            assert!(std::process::Command::new("git")
                .args(&args)
                .current_dir(&repo)
                .status_locked()
                .unwrap()
                .success());
        }
        crate::test_support::trust_repo(&repo);

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0u128, |d| d.as_nanos());
        let active_claude_sid = format!(
            "11111111-{:04x}-4333-8444-{:012x}",
            (std::process::id() & 0xffff),
            (nonce & 0xffff_ffff_ffff)
        );
        let active_agy_sid = format!(
            "22222222-{:04x}-4333-8444-{:012x}",
            (std::process::id() & 0xffff),
            ((nonce.wrapping_add(1)) & 0xffff_ffff_ffff)
        );
        let idle_claude_sid = format!(
            "33333333-{:04x}-4333-8444-{:012x}",
            (std::process::id() & 0xffff),
            ((nonce.wrapping_add(2)) & 0xffff_ffff_ffff)
        );
        let my_pid = std::process::id();
        let my_start = crate::workbench::process_birth::started_unix_nanos(my_pid);

        // 1. Active Claude session (`working`) -> receives mailbox + bus directive.
        agent_bus::upsert_peer(
            &repo_str,
            PeerUpsert {
                provider: "claude".into(),
                surface: "terminal".into(),
                provider_session_id: Some(active_claude_sid.clone()),
                worktree_path: repo_str.clone(),
                branch: Some("main".into()),
                role: Some("coder".into()),
                turn_state: Some("working".into()),
                pid: Some(my_pid),
                proc_start_ns: my_start,
                ..PeerUpsert::default()
            },
        )
        .unwrap();
        // 2. Active Antigravity session (`running`) -> receives mailbox + bus directive.
        agent_bus::upsert_peer(
            &repo_str,
            PeerUpsert {
                provider: "agy".into(),
                surface: "terminal".into(),
                provider_session_id: Some(active_agy_sid.clone()),
                worktree_path: repo_str.clone(),
                branch: Some("main".into()),
                role: Some("verifier".into()),
                turn_state: Some("running".into()),
                pid: Some(my_pid),
                proc_start_ns: my_start,
                ..PeerUpsert::default()
            },
        )
        .unwrap();
        // 3. Active Codex session (no canonical UUID mailbox) -> counted once on bus.
        agent_bus::upsert_peer(
            &repo_str,
            PeerUpsert {
                peer_id: Some("codex:cli-1".into()),
                provider: "codex".into(),
                surface: "external_cli".into(),
                worktree_path: repo_str.clone(),
                branch: Some("main".into()),
                turn_state: Some("running".into()),
                pid: Some(my_pid),
                proc_start_ns: my_start,
                ..PeerUpsert::default()
            },
        )
        .unwrap();
        // 4. Idle Claude session (`idle`) -> MUST NOT receive unread mailbox envelope.
        agent_bus::upsert_peer(
            &repo_str,
            PeerUpsert {
                provider: "claude".into(),
                surface: "terminal".into(),
                provider_session_id: Some(idle_claude_sid.clone()),
                worktree_path: repo_str.clone(),
                branch: Some("main".into()),
                turn_state: Some("idle".into()),
                pid: Some(my_pid),
                proc_start_ns: my_start,
                ..PeerUpsert::default()
            },
        )
        .unwrap();

        // Untrusted MCP / Agent callers cannot spoof steward sender names or kinds.
        for spoof_sender in [
            "manvi-steward",
            "MANVI-STEWARD",
            "manvi-steward:fake",
            "system-steward",
            "manvi-system-steward",
            "gitpulse-steward",
        ] {
            let err = crate::mcp::parse::<AgentSendArgs>(
                "gitpulse_agent_send",
                &json!({
                    "repo_path": repo_str,
                    "from_peer": spoof_sender,
                    "to_peer": "coder",
                    "body": "spoofed directive",
                }),
            )
            .and_then(handle_agent_send)
            .expect_err("MCP caller must not spoof steward sender");
            assert!(
                err.contains("Manvi system steward"),
                "unexpected error for {spoof_sender}: {err}"
            );
            assert!(
                agent_bus::post_message(
                    &repo_str,
                    MessageOrigin::Agent,
                    spoof_sender,
                    Some("coder"),
                    "note",
                    "spoofed legacy inbox post",
                    None,
                )
                .is_err(),
                "gitpulse_inbox_post path must also refuse reserved steward sender {spoof_sender}"
            );
            assert!(
                crate::mcp::parse::<AgentRegisterArgs>(
                    "gitpulse_agent_register",
                    &json!({
                        "repo_path": repo_str,
                        "provider": "claude",
                        "peer_id": spoof_sender,
                    }),
                )
                .and_then(handle_agent_register)
                .is_err(),
                "MCP register must refuse reserved steward peer_id {spoof_sender}"
            );
        }
        for spoof_kind in ["system-advisory", "system_steward"] {
            assert!(
                crate::mcp::parse::<AgentSendArgs>(
                    "gitpulse_agent_send",
                    &json!({
                        "repo_path": repo_str,
                        "from_peer": "claude:attacker",
                        "to_peer": "coder",
                        "kind": spoof_kind,
                        "body": "spoofed kind",
                    }),
                )
                .and_then(handle_agent_send)
                .is_err(),
                "MCP send must refuse reserved steward kind {spoof_kind}"
            );
            assert!(
                agent_bus::post_message(
                    &repo_str,
                    MessageOrigin::Agent,
                    "claude:attacker",
                    Some("coder"),
                    spoof_kind,
                    "spoofed legacy inbox kind",
                    None,
                )
                .is_err(),
                "gitpulse_inbox_post path must also refuse reserved steward kind {spoof_kind}"
            );
        }
        // Also block `provider: "manvi-steward"` (derived peer_id), lane claim as
        // `"manvi-steward"`, and `thread_key: "manvi-system-steward"` injection.
        assert!(
            crate::mcp::parse::<AgentRegisterArgs>(
                "gitpulse_agent_register",
                &json!({
                    "repo_path": repo_str,
                    "provider": "manvi-steward",
                    "role": "coder",
                }),
            )
            .and_then(handle_agent_register)
            .is_err(),
            "MCP register must refuse reserved steward provider"
        );
        assert!(
            agent_bus::register_or_update_lane_with_conflicts(
                &repo_str,
                STEWARD_SENDER,
                "spoofed lane",
                None,
                &[],
                &["src/main.rs".to_string()],
                &[],
            )
            .is_err(),
            "lane claim must refuse reserved steward identity"
        );
        assert!(
            crate::mcp::parse::<AgentSendArgs>(
                "gitpulse_agent_send",
                &json!({
                    "repo_path": repo_str,
                    "from_peer": "claude:attacker",
                    "to_peer": "coder",
                    "thread_key": STEWARD_THREAD_KEY,
                    "body": "spoofed thread injection",
                }),
            )
            .and_then(handle_agent_send)
            .is_err(),
            "MCP send must refuse reserved steward thread_key"
        );

        // Deliver real steward directive via `LiveDeps::deliver_repo_directive`
        // into the default Mailbox + real `agent_bus`.
        let wb = WorkbenchState::for_profile(&tmp.path().join("workbench.sqlite"));
        let live_deps = LiveDeps {
            workbench: &wb,
            watched_roots: vec![repo_str.clone()],
        };
        assert!(live_deps.repo_trusted(&repo_str));
        assert!(live_deps.repo_common_dir(&repo_str).is_some());
        let calm = make_load(SensorState::Fresh, Tier::Clear, None, true);
        // The candidates are spelled the way the repository admission spells a
        // checkout (`\\?\C:\...` on Windows), which is not the plain path the
        // bus peers above were registered under.
        let admitted = repo.canonicalize().unwrap().to_string_lossy().into_owned();
        assert!(live_deps.candidate_repos(&calm, None).contains(&admitted));

        let mb = Mailbox::at_default().expect("default mailbox");
        let msg_id = format!(
            "steward-live-{}-{:x}",
            std::process::id(),
            (nonce & 0xffff_ffff)
        );
        let recipients = live_deps
            .deliver_repo_directive(
                &repo_str,
                &msg_id,
                "claude-sonnet-4-6",
                "cloud",
                "severe",
                "memory_pressure",
                "Host memory pressure is severe. Cap cargo builds to CARGO_BUILD_JOBS=2.",
            )
            .expect("LiveDeps::deliver_repo_directive succeeds");
        assert_eq!(
            recipients, 3,
            "2 active mailbox peers + 1 active codex bus peer"
        );
        assert_eq!(mb.status(&active_claude_sid, &msg_id), Delivery::Pending);
        assert_eq!(mb.status(&active_agy_sid, &msg_id), Delivery::Pending);
        assert_eq!(
            mb.status(&idle_claude_sid, &msg_id),
            Delivery::Absent,
            "idle session must not be woken or stuffed"
        );

        // Read via `gitpulse_agent_inbox` with `include_broadcasts: false` (mailbox path)
        // for `coder`, with `include_broadcasts: false` (bus-only path, no UUID mailbox)
        // for `codex:cli-1`, and with `include_broadcasts: true` (bus + mailbox dedupe path)
        // for `verifier`: wire metadata must be 100% identical across all three!
        let from_codex_bus = crate::mcp::parse::<AgentInboxArgs>(
            "gitpulse_agent_inbox",
            &json!({
                "repo_path": repo_str,
                "for_peer": "codex:cli-1",
                "include_broadcasts": false,
                "ack": true,
            }),
        )
        .and_then(handle_agent_inbox)
        .expect("codex bus-only peer must receive steward directive with default include_broadcasts=false");
        let codex_msgs = from_codex_bus["messages"].as_array().unwrap();
        let codex_msg = codex_msgs.iter().find(|m| m["msg_id"] == msg_id).expect(
            "codex peer must not be blind to SystemSteward advisory when include_broadcasts=false",
        );
        assert_eq!(codex_msg["from_agent"], STEWARD_SENDER);
        assert_eq!(codex_msg["kind"], STEWARD_MESSAGE_KIND);
        assert_eq!(codex_msg["thread_key"], STEWARD_THREAD_KEY);
        assert_eq!(codex_msg["hop"], 0);
        assert_eq!(codex_msg["max_hops"], 1);

        let from_mailbox = crate::mcp::parse::<AgentInboxArgs>(
            "gitpulse_agent_inbox",
            &json!({
                "repo_path": repo_str,
                "for_peer": "coder",
                "include_broadcasts": false,
                "ack": true,
            }),
        )
        .and_then(handle_agent_inbox)
        .expect("inbox via mailbox path");
        let mb_msgs = from_mailbox["messages"].as_array().unwrap();
        let mb_msg = mb_msgs
            .iter()
            .find(|m| m["msg_id"] == msg_id)
            .expect("steward directive delivered via mailbox");
        assert_eq!(mb_msg["from_agent"], STEWARD_SENDER);
        assert_eq!(mb_msg["kind"], STEWARD_MESSAGE_KIND);
        assert_eq!(mb_msg["thread_key"], STEWARD_THREAD_KEY);
        assert_eq!(mb_msg["hop"], 0);
        assert_eq!(mb_msg["max_hops"], 1);
        assert_eq!(mb.status(&active_claude_sid, &msg_id), Delivery::Claimed);

        let from_bus = crate::mcp::parse::<AgentInboxArgs>(
            "gitpulse_agent_inbox",
            &json!({
                "repo_path": repo_str,
                "for_peer": "verifier",
                "include_broadcasts": true,
                "thread_key": STEWARD_THREAD_KEY,
                "ack": true,
            }),
        )
        .and_then(handle_agent_inbox)
        .expect("inbox via bus+mailbox deduped path");
        let bus_msgs = from_bus["messages"].as_array().unwrap();
        assert_eq!(
            bus_msgs.len(),
            1,
            "bus + mailbox must deduplicate by msg_id"
        );
        assert_eq!(bus_msgs[0]["msg_id"], msg_id);
        assert_eq!(bus_msgs[0]["from_agent"], STEWARD_SENDER);
        assert_eq!(bus_msgs[0]["kind"], STEWARD_MESSAGE_KIND);
        assert_eq!(bus_msgs[0]["thread_key"], STEWARD_THREAD_KEY);
        assert_eq!(bus_msgs[0]["hop"], 0);
        assert_eq!(bus_msgs[0]["max_hops"], 1);
        assert_eq!(
            mb.status(&active_agy_sid, &msg_id),
            Delivery::Claimed,
            "include_broadcasts=true ack must cross-claim mailbox envelope by msg_id"
        );
        for sid in [&active_claude_sid, &active_agy_sid, &idle_claude_sid] {
            if let Ok(dir) = mb.session_path(sid) {
                let _ = fs::remove_dir_all(dir);
            }
        }
    }

    #[test]
    fn critical_pulse_pre_hysteresis_acts_immediately_and_healthy_tick_never_swallows_next_critical_wake(
    ) {
        let tmp = tempfile::tempdir().unwrap();
        let rt = StewardRuntime::default();
        let mut deps = base_deps(tmp.path());
        deps.manvi = ManviProbe::Answered(Box::new(local_manvi()));
        deps.candidates = vec![AgentThrottleCandidate {
            pid: 7100,
            start_us: 12,
            name: "rustc".into(),
            session_pid: 7000,
            session_kind: AgentKind::Claude,
            session_cwd: Some("/trusted/a".into()),
            is_session_root: false,
            lineage_cwds: Vec::new(),
            ancestor_pids: Vec::new(),
        }];

        // Case 1: Pulse wakes on `PressureInput::Critical` while `verdict.tier` is
        // `Tier::Elevated` with `Binding::CpuLoad` (`admit: true`) before governor
        // `ENTER_SAMPLES` hysteresis flips to `Severe` / `MemoryPressure`.
        // Must skip local LLM (so model weights aren't loaded into critical RAM)!
        let mut pre_hysteresis_load = make_load(
            SensorState::Fresh,
            Tier::Elevated,
            Some(Binding::CpuLoad),
            true,
        );
        pre_hysteresis_load.pressure = PressureInput::Critical;
        let pre_hysteresis_pulse = crate::sysload::pulse_of(&pre_hysteresis_load);
        notify_pulse_on(&rt, &pre_hysteresis_pulse);
        assert_eq!(
            crate::supervisor::lock_or_recover(&rt.inner).wake_requested,
            Some("critical_wake")
        );

        deps.load = pre_hysteresis_load;

        let first = evaluate_tick_with(&rt, "critical_wake", None, &deps);
        assert_eq!(
            first.outcome, "acted",
            "first critical kernel pressure sample must act instead of recording healthy"
        );
        assert_eq!(first.tier, "severe");
        assert_eq!(first.binding, "memory_pressure");
        assert_eq!(
            *deps.llm_calls.borrow(),
            0,
            "local model must be skipped on kernel critical memory pressure even if prior verdict binding was CpuLoad"
        );
        assert_eq!(first.throttled_processes, 1);
        assert!(crate::supervisor::lock_or_recover(&rt.inner).critical_throttle_active);

        // Case 2: Non-critical `Tier::Elevated` tick clears `critical_throttle_active`
        // AND resets `last_critical_wake`, so a real `Severe` pulse 2s later (inside
        // the 15s debounce window) wakes immediately instead of being swallowed!
        deps.now_ms += 2_000;
        deps.load = make_load(
            SensorState::Fresh,
            Tier::Elevated,
            Some(Binding::CpuLoad),
            true,
        );
        let elevated = evaluate_tick_with(&rt, "scheduled", None, &deps);
        assert_eq!(elevated.outcome, "acted");
        assert!(
            crate::supervisor::lock_or_recover(&rt.inner)
                .last_critical_wake
                .is_none(),
            "non-critical Elevated tick must clear last_critical_wake so a Severe transition 2s later wakes immediately"
        );

        let mut severe_load =
            make_load(SensorState::Fresh, Tier::Severe, Some(Binding::Swap), true);
        severe_load.pressure = PressureInput::Critical;
        let severe_pulse = crate::sysload::pulse_of(&severe_load);
        notify_pulse_on(&rt, &severe_pulse);
        assert_eq!(
            crate::supervisor::lock_or_recover(&rt.inner).wake_requested,
            Some("critical_wake"),
            "Severe transition 2s after non-critical Elevated tick must NOT be swallowed by 15s debounce"
        );
    }

    #[test]
    fn disabled_steward_never_spawns_manvi_use_or_polls_and_build_status_tracks_repo_override_schedule(
    ) {
        let tmp = tempfile::tempdir().unwrap();
        let rt = StewardRuntime::default();
        let deps = base_deps(tmp.path());
        deps.settings.borrow_mut().enabled = false;

        // 1. Background scheduler parks indefinitely (`Sleep(None)`) with zero 15s polling while disabled.
        let (decision, seen_epoch) =
            decide_worker_step_with_epoch(&rt, &deps.settings.borrow(), None, deps.now_ms);
        assert_eq!(decision, WorkerDecision::Sleep(None));
        assert!(crate::supervisor::lock_or_recover(&rt.inner)
            .history
            .is_empty());

        // Atomic wakeup check: if `settings_epoch` bumps across the lock boundary before
        // `wait_for_worker_wakeup(..., None, seen_epoch)` enters `wait_while`, it returns
        // immediately rather than losing the wakeup and parking forever.
        {
            let mut g = crate::supervisor::lock_or_recover(&rt.inner);
            g.settings_epoch = g.settings_epoch.wrapping_add(1);
        }
        let started = Instant::now();
        wait_for_worker_wakeup(&rt, None, seen_epoch);
        assert!(
            started.elapsed() < crate::test_support::coverage_relaxed(Duration::from_millis(200)),
            "wait_for_worker_wakeup must observe pre-wait settings_epoch bump without parking"
        );

        // 2. Manual/direct `evaluate_tick_with` while disabled makes ZERO `manvi use` probes.
        let rec = evaluate_tick_with(&rt, "manual", None, &deps);
        assert_eq!(rec.outcome, "disabled");
        assert_eq!(
            *deps.manvi_probes.borrow(),
            0,
            "disabled steward must not spawn `manvi use`"
        );

        // 3. When global `enabled == false` and a repository override has `enabled == Some(true)`,
        //    both `decide_worker_step` and machine-wide `build_status_with_env(..., repo_path = None, ...)`
        //    report an active schedule (`next_check_ms = Some(...)`).
        {
            let mut s = deps.settings.borrow_mut();
            s.repositories.insert(
                "/trusted/a/.git".into(),
                RepoStewardOverride {
                    enabled: Some(true),
                    ..RepoStewardOverride::default()
                },
            );
        }
        let (with_repo_override, _) = decide_worker_step_with_epoch(
            &rt,
            &deps.settings.borrow(),
            None,
            deps.now_ms + 300_000,
        );
        assert_eq!(with_repo_override, WorkerDecision::Run("scheduled"));

        let machine_status = build_status_with_env(
            &rt,
            deps.settings.borrow().clone(),
            &cloud_manvi(),
            None,
            None,
        );
        assert!(
            machine_status.next_check_ms.is_some(),
            "machine-wide status must report next_check_ms when a repository override enables the steward"
        );
    }

    #[cfg(unix)]
    #[test]
    fn shared_cache_anchor_fallback_and_finished_session_scratchpad_cleanup() {
        let _serial = crate::storage::hygiene::tests::serial();
        let tmp = tempfile::tempdir().unwrap();
        let rt = StewardRuntime::default();
        let mut deps = base_deps(tmp.path());
        deps.load = make_load(
            SensorState::Fresh,
            Tier::Severe,
            Some(Binding::LowDisk),
            false,
        );
        deps.disk_remedy = true;
        // Global `clean_critical` is ON (`clean_shared_caches == true`), while open
        // trusted repository `/trusted/a` opted OUT of repo-local `clean_critical`.
        deps.settings.borrow_mut().repositories.insert(
            "/trusted/a/.git".into(),
            RepoStewardOverride {
                clean_critical: Some(false),
                ..RepoStewardOverride::default()
            },
        );
        let rec = evaluate_tick_with(&rt, "manual", None, &deps);
        assert_eq!(rec.outcome, "acted");
        let calls = deps.clean_calls.borrow();
        assert_eq!(calls.len(), 1);
        assert!(
            calls[0].0.is_empty(),
            "opted-out repo must not be in trusted_cleanable_repos"
        );
        assert_eq!(
            calls[0].1, None,
            "a repository that opted out is never the one a shared-cache prune runs under"
        );
        assert!(calls[0].2, "clean_shared_caches must be true");
        drop(calls);

        // With another trusted repository that keeps cleanup on, that one anchors.
        deps.now_ms += AUTO_CLEAN_COOLDOWN_MS;
        deps.repos.push("/trusted/c".into());
        deps.trusted.insert("/trusted/c".into());
        deps.common_dirs
            .insert("/trusted/c".into(), "/trusted/c/.git".into());
        evaluate_tick_with(&rt, "critical_wake", None, &deps);
        let calls = deps.clean_calls.borrow();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[1].0, vec!["/trusted/c".to_string()]);
        assert_eq!(calls[1].1.as_deref(), Some("/trusted/c"));
        drop(calls);

        // Now verify `clean_session_agent_scratchpads` (`SCOPE_SESSION`):
        // 1. Reclaims a finished session's aged Cargo target under `<tmp>/claude-<uid>/<project>/<session>/scratchpad`
        // 2. Preserves a running session's scratchpad
        // 3. Re-observes `LiveSessions` right before `remove_judged` (TOCTOU guard) so a session
        //    that resumes between `scan_sessions` and `remove_judged` is NOT deleted!
        let sess_tmp = tempfile::tempdir().unwrap();
        let canon_tmp = crate::engine::process::canonicalize_plain(sess_tmp.path()).unwrap();
        let uid = crate::storage::hygiene::agent::current_uid();
        let finished_sid = "session-finished-1111aaaa";
        let live_sid = "session-running-2222bbbb";
        let resumed_sid = "session-resumed-3333cccc";

        let make_session_target = |sid: &str| -> PathBuf {
            let session_dir = canon_tmp.join(format!("claude-{uid}/proj-a/{sid}"));
            let scratchpad = session_dir.join("scratchpad");
            let target = scratchpad.join("target");
            fs::create_dir_all(target.join("debug")).unwrap();
            fs::write(target.join("debug/.cargo-lock"), "").unwrap();
            fs::write(
                target.join("CACHEDIR.TAG"),
                "Signature: 8a477f597d28d172789f06886806bc55\n",
            )
            .unwrap();
            fs::write(target.join(".rustc_info.json"), "{}").unwrap();
            fs::write(target.join("debug/libpad.rlib"), vec![b'z'; 6144]).unwrap();

            let old = SystemTime::now() - Duration::from_secs(12 * 86_400);
            for p in [
                target.join("debug/.cargo-lock"),
                target.join("debug/libpad.rlib"),
                target.join("debug"),
                target.join("CACHEDIR.TAG"),
                target.join(".rustc_info.json"),
                target.clone(),
                scratchpad,
                session_dir,
            ] {
                crate::test_support::set_modified(&p, old).unwrap();
            }
            target
        };

        let finished_target = make_session_target(finished_sid);
        let running_target = make_session_target(live_sid);
        let resumed_target = make_session_target(resumed_sid);

        let observe_calls = std::cell::Cell::new(0usize);
        let observe_live = || {
            let call = observe_calls.get();
            observe_calls.set(call + 1);
            let mut ids = BTreeSet::from([live_sid.to_string()]);
            if call >= 1 {
                // Simulate `resumed_sid` resuming after `scan_sessions` (call 0)
                // before `remove_judged` re-checks `observe_live()`!
                ids.insert(resumed_sid.to_string());
            }
            crate::storage::hygiene::agent::LiveSessions {
                ids,
                unidentified: 0,
                incomplete: None,
            }
        };
        let own_removals = crate::storage::hygiene::agent::SelfRemovals::default();
        let mut scratch_reclaimed = 0u64;
        let mut out = CleanPassOutcome::default();
        clean_session_agent_scratchpads(
            &canon_tmp,
            7,
            &observe_live,
            &own_removals,
            &|_| false,
            &mut scratch_reclaimed,
            &mut out,
        );

        assert!(
            !finished_target.exists(),
            "finished session's Cargo scratchpad target must be reclaimed: {out:?}"
        );
        assert!(
            running_target.exists(),
            "running session's Cargo scratchpad target must be preserved"
        );
        assert!(
            resumed_target.exists(),
            "TOCTOU guard: session resumed between scan_sessions and remove_judged must be preserved!"
        );
        assert!(
            out.reclaimed_bytes >= 6_000,
            "expected >= 6 KiB reclaimed from SCOPE_SESSION, got {out:?}"
        );
        assert!(out
            .items
            .iter()
            .any(|i| i.target.starts_with("session-scratch:") && i.status == "reclaimed"));
    }

    #[test]
    fn foreign_live_writer_lease_survives_reader_standdown_and_non_throttled_critical_debounce_resets(
    ) {
        let tmp = tempfile::tempdir().unwrap();
        let rt = StewardRuntime::default();
        let mut deps = base_deps(tmp.path());

        // A distinct live OS process represents the elected primary GitPulse
        // writer; its start time is read the way the lease reader reads it.
        use crate::procguard::LockedSpawn;
        struct ChildKillGuard(std::process::Child);
        impl Drop for ChildKillGuard {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        #[cfg(unix)]
        let mut sleeper = std::process::Command::new("/bin/sleep");
        #[cfg(unix)]
        sleeper.arg("30");
        #[cfg(windows)]
        let mut sleeper = std::process::Command::new("ping");
        #[cfg(windows)]
        sleeper.args(["-n", "30", "127.0.0.1"]);
        let mut guard_child = ChildKillGuard(sleeper.spawn_locked().expect("spawn sleeper"));
        let foreign_pid = guard_child.0.id();
        let foreign_start_us =
            crate::sysload::process_start_us(foreign_pid).expect("live child start_us");
        assert_ne!(foreign_pid, std::process::id());
        // A live foreign writer's lease is kept.
        let kept = true;

        let foreign_record = ActiveThrottleRecord {
            version: LEASE_VERSION,
            writer_pid: foreign_pid,
            writer_start_us: foreign_start_us,
            issued_at_ms: deps.now_ms,
            expires_at_ms: deps.now_ms + 300_000,
            tier: "severe".into(),
            binding: "swap".into(),
            reason: "primary writer swap pressure".into(),
            pace_ms: CRITICAL_PACE_MS,
            repositories: Vec::new(),
        };
        let active_file = tmp.path().join(STEWARD_SUBDIR).join(ACTIVE_FILE);
        write_active_throttle(tmp.path(), &foreign_record).expect("write foreign active.json");
        assert!(active_file.exists());

        // 1. Secondary instance shutdown / passive cleanup must NOT clobber a live foreign writer's lease.
        clear_active_throttle_if_owned_or_stale(tmp.path(), deps.now_ms)
            .expect("passive clear succeeds");
        assert_eq!(
            active_file.exists(),
            kept,
            "live foreign writer's active.json must survive passive/shutdown clear"
        );

        // 2. A secondary `Role::Reader` instance under `Tier::Severe` must not request
        //    duplicate `critical_wake` sweeps, double-message buses, or delete the primary
        //    writer's `active.json` (on either scheduled or manual ticks).
        deps.load = make_load(SensorState::Fresh, Tier::Severe, Some(Binding::Swap), false);
        deps.load.role = Role::Reader;
        deps.load.writer_is_self = false;

        let reader_pulse = crate::sysload::pulse_of(&deps.load);
        notify_pulse_on(&rt, &reader_pulse);
        assert_eq!(
            crate::supervisor::lock_or_recover(&rt.inner).wake_requested,
            None,
            "Role::Reader must not schedule duplicate critical_wake sweeps"
        );

        let sched_rec = evaluate_tick_with(&rt, "scheduled", None, &deps);
        assert_eq!(sched_rec.outcome, "sensor_unavailable");
        assert_eq!(*deps.llm_calls.borrow(), 0);
        assert!(deps.delivered.borrow().is_empty());
        assert_eq!(
            active_file.exists(),
            kept,
            "Role::Reader scheduled stand-down must keep primary writer's active.json"
        );

        let manual_rec = evaluate_tick_with(&rt, "manual", None, &deps);
        assert_eq!(manual_rec.outcome, "acted");
        assert_eq!(
            active_file.exists(),
            kept,
            "Role::Reader manual tick must not clobber primary writer's active.json"
        );

        // Once the foreign writer exits, passive cleanup reclaims the stale lease file.
        let _ = guard_child.0.kill();
        let _ = guard_child.0.wait();
        clear_active_throttle_if_owned_or_stale(tmp.path(), deps.now_ms)
            .expect("stale clear succeeds");
        assert!(
            !active_file.exists(),
            "dead foreign writer's active.json must be removed"
        );

        // 3. When `slow_critical == false` (`critical_throttle_active == false`), a healthy
        //    `!critical` pulse must still reset `last_critical_wake` so an immediate re-entry
        //    into `Tier::Severe` inside 15s is NOT swallowed by `CRITICAL_WAKE_DEBOUNCE`.
        let rt2 = StewardRuntime::default();
        let severe = make_load(SensorState::Fresh, Tier::Severe, Some(Binding::Swap), false);
        let clear = make_load(SensorState::Fresh, Tier::Clear, None, true);
        let severe_pulse = crate::sysload::pulse_of(&severe);
        let clear_pulse = crate::sysload::pulse_of(&clear);

        notify_pulse_on(&rt2, &severe_pulse);
        assert_eq!(
            crate::supervisor::lock_or_recover(&rt2.inner).wake_requested,
            Some("critical_wake")
        );
        {
            let mut g = crate::supervisor::lock_or_recover(&rt2.inner);
            // Worker consumed the wakeup with `slow_critical == false` (`critical_throttle_active == false`).
            g.wake_requested = None;
            g.critical_throttle_active = false;
        }
        notify_pulse_on(&rt2, &clear_pulse);
        assert_eq!(
            crate::supervisor::lock_or_recover(&rt2.inner).wake_requested,
            None
        );
        notify_pulse_on(&rt2, &severe_pulse);
        assert_eq!(
            crate::supervisor::lock_or_recover(&rt2.inner).wake_requested,
            Some("critical_wake"),
            "subsequent Severe transition within 15s after healthy recovery must not be swallowed"
        );

        // 4. `default_session_tmp_roots` includes canonical temp dir without duplicates.
        let roots = default_session_tmp_roots();
        assert!(!roots.is_empty());
        let unique: BTreeSet<PathBuf> = roots.iter().cloned().collect();
        assert_eq!(
            roots.len(),
            unique.len(),
            "session tmp roots must be deduplicated"
        );
    }

    #[cfg(unix)]
    #[test]
    fn default_cloud_manvi_promotes_only_with_valid_anthropic_account_or_keychain_key() {
        use std::os::unix::fs::PermissionsExt;

        const DEFAULT_CLOUD_JSON: &str = r#"{"mode":"cloud","provider":"anthropic","provider_from":"default","user_config":"/home/u/.config/manvi/config.toml","shadowed_by":[],"model":"claude-sonnet-5-5","model_from":"default","local":{"base_url":"http://127.0.0.1:8000/v1","base_url_from":"default","model":"","model_from":"default"},"cloud":{"model":"claude-sonnet-5-5","model_from":"default"},"warnings":[]}"#;
        const VALID_KEY: &str = "sk-ant-api03-StewardTestKey_0123456789abcdef";

        let raw_unchosen = manvi_use::interpret(manvi_use::UseRun {
            success: true,
            stdout: DEFAULT_CLOUD_JSON.as_bytes().to_vec(),
            stderr: String::new(),
        });
        assert_eq!(
            raw_unchosen.state, "not_chosen",
            "diff-sending callers still see default cloud as not_chosen"
        );
        assert!(manvi_use::is_default_cloud(&raw_unchosen));

        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let bin = home.join(".local/bin");
        let sec_dir = tmp.path().join("sec");
        fs::create_dir_all(&bin).unwrap();
        fs::create_dir_all(&sec_dir).unwrap();

        // Items are kept per service, as the real Keychain keeps them: `item`
        // is the current service and `item-legacy` the pre-rename one. A fake
        // that ignored the service would let `save`'s cleanup of the old item
        // delete the key it had just written.
        let sec_item = sec_dir.join("item");
        let sec_legacy = sec_dir.join("item-legacy");
        let sec_script = format!(
            "#!/bin/sh\npick() {{\n  case \"$1\" in\n    dev.vbcr.gitpulse*|com.gitpulse.desktop*) item='{legacy}' ;;\n    *) item='{item}' ;;\n  esac\n}}\ncase \"$1\" in\n  -i)\n    read -r line\n    key=${{line##* -w }}\n    svc=${{line#* -s }}; svc=${{svc%% *}}\n    pick \"$svc\"\n    printf '%s' \"$key\" > \"$item\"\n    exit 0 ;;\n  find-generic-password)\n    pick \"$3\"\n    [ -f \"$item\" ] || exit 44\n    cat \"$item\"; echo; exit 0 ;;\n  delete-generic-password)\n    pick \"$3\"\n    rm -f \"$item\"; exit 0 ;;\nesac\nexit 2\n",
            item = sec_item.display(),
            legacy = sec_legacy.display(),
        );
        let sec_bin = sec_dir.join("security");
        fs::write(&sec_bin, sec_script).unwrap();
        fs::set_permissions(&sec_bin, fs::Permissions::from_mode(0o755)).unwrap();
        let keychain = crate::harness::claude_key::Keychain::at(sec_bin);

        let probe_cred = || {
            detect_steward_claude_credential_with(
                || {
                    crate::harness::anthropic_account::try_detect_with(
                        Some(&home),
                        Some(bin.as_os_str()),
                    )
                },
                || keychain.present(),
            )
        };

        // 1. Neither ~/.anthropic-accounts nor Keychain key -> remains `not_chosen`.
        let absent = resolve_steward_manvi_status(raw_unchosen.clone(), probe_cred).unwrap();
        assert_eq!(absent.state, "not_chosen");
        assert!(absent.typed.is_none());

        // 2. Valid Keychain Claude key (with no ~/.anthropic-accounts) -> promotes to `ready`!
        keychain.save(VALID_KEY).unwrap();
        let via_key = resolve_steward_manvi_status(raw_unchosen.clone(), probe_cred).unwrap();
        assert_eq!(via_key.state, "ready");
        assert_eq!(via_key.mode, "cloud");
        assert_eq!(
            via_key.typed,
            Some(ModelSelection::Claude {
                model: "claude-sonnet-5-5".into(),
            })
        );
        assert!(
            via_key.detail.contains("Keychain key"),
            "the detail does not say the Keychain key was used"
        );

        // 3. Broken ~/.anthropic-accounts MUST NOT silently fall back to Keychain key,
        //    and MUST surface `state == "failed"` (not `"not_chosen"`).
        fs::write(home.join(".anthropic-accounts"), "main wrkspc_01\n").unwrap();
        let broken = resolve_steward_manvi_status(raw_unchosen.clone(), probe_cred).unwrap();
        assert_eq!(
            broken.state, "failed",
            "broken ~/.anthropic-accounts must report failed and never fall back to Keychain key"
        );
        assert!(broken.typed.is_none());
        assert!(
            broken.detail.contains(".anthropic-accounts"),
            "the detail does not point at ~/.anthropic-accounts"
        );

        // 3b. Corrupted Keychain item (when ~/.anthropic-accounts is absent) also reports `failed`.
        fs::remove_file(home.join(".anthropic-accounts")).unwrap();
        fs::write(&sec_item, "not-a-valid-key").unwrap();
        let bad_kc = resolve_steward_manvi_status(raw_unchosen.clone(), probe_cred).unwrap();
        assert_eq!(bad_kc.state, "failed");
        assert!(bad_kc.typed.is_none());
        keychain.save(VALID_KEY).unwrap();

        // 3c. Cached Keychain verification avoids re-running `present()` (`-w`) on repeat ticks,
        //     while still catching external Keychain item deletion via `item_exists()`.
        let mut verified = false;
        let present_calls = std::cell::Cell::new(0usize);
        let first_cred = detect_steward_claude_credential_cached(
            &mut verified,
            || Ok(crate::harness::anthropic_account::Detection::Absent),
            || keychain.item_exists(),
            || {
                present_calls.set(present_calls.get() + 1);
                keychain.present()
            },
        )
        .unwrap();
        assert_eq!(first_cred, StewardClaudeCredential::KeychainKey);
        assert!(verified);
        assert_eq!(present_calls.get(), 1);

        let second_cred = detect_steward_claude_credential_cached(
            &mut verified,
            || Ok(crate::harness::anthropic_account::Detection::Absent),
            || keychain.item_exists(),
            || panic!("repeat background tick must not call keychain.present() (-w)"),
        )
        .unwrap();
        assert_eq!(second_cred, StewardClaudeCredential::KeychainKey);

        keychain.delete().unwrap();
        let after_delete = detect_steward_claude_credential_cached(
            &mut verified,
            || Ok(crate::harness::anthropic_account::Detection::Absent),
            || keychain.item_exists(),
            || panic!("absent item must not call keychain.present() (-w)"),
        )
        .unwrap();
        assert_eq!(after_delete, StewardClaudeCredential::Absent);
        assert!(!verified);

        fs::write(home.join(".anthropic-accounts"), "main wrkspc_01\n").unwrap();

        // 4. Valid ~/.anthropic-accounts launcher -> promotes to `ready` with account name!
        let acct_bin = bin.join("anthropic-account");
        fs::write(
            &acct_bin,
            "#!/bin/sh\nprintf '{\"account\":\"main\",\"source\":\"default\",\"workspace\":\"wrkspc_01\"}'\n",
        )
        .unwrap();
        fs::set_permissions(&acct_bin, fs::Permissions::from_mode(0o755)).unwrap();
        let via_acct = resolve_steward_manvi_status(raw_unchosen.clone(), probe_cred).unwrap();
        assert_eq!(via_acct.state, "ready");
        assert_eq!(
            via_acct.typed,
            Some(ModelSelection::Claude {
                model: "claude-sonnet-5-5".into(),
            })
        );
        assert!(
            via_acct.detail.contains("account `main`"),
            "the detail does not name the chosen account entry"
        );

        // 5. DeferredUnderLoad from credential check bubbles up so `probe_live_manvi` preserves cached state.
        let deferred = resolve_steward_manvi_status(raw_unchosen, || {
            Err(crate::ipc_error::CommandError::typed(
                IpcErrorKind::DeferredUnderLoad,
                "shed under load",
            ))
        })
        .unwrap_err();
        assert_eq!(deferred.origin(), Some(IpcErrorKind::DeferredUnderLoad));

        // 6. Explicit `manvi use local` or `manvi use cloud` never invokes `read_cred`.
        let explicit_local = resolve_steward_manvi_status(local_manvi(), || {
            panic!("explicit manvi choice must not spawn credential probes")
        })
        .unwrap();
        assert_eq!(explicit_local.state, "ready");
        assert_eq!(explicit_local.mode, "local");
    }

    #[cfg(unix)]
    #[test]
    fn manvi_cloud_evaluates_candidate_inventory_reclaims_selected_recent_scratch_and_worktree_targets_and_preserves_unselected(
    ) {
        let _serial = crate::storage::hygiene::tests::serial();
        use crate::procguard::LockedSpawn;
        use crate::sysload::{
            inject_governor_with_history, GovernorInput, Level, Reading, Readings,
        };

        let tmp = tempfile::tempdir().unwrap();
        let base = crate::engine::process::canonicalize_plain(tmp.path()).unwrap();
        let repo = base.join("repo");
        let wt = base.join("wt-idle");
        let sess_tmp = base.join("sys-tmp");
        fs::create_dir_all(&repo).unwrap();
        fs::create_dir_all(&sess_tmp).unwrap();

        for args in [
            vec!["init", "-q", "-b", "main"],
            vec!["config", "user.name", "Steward Test"],
            vec!["config", "user.email", "steward@example.com"],
        ] {
            assert!(std::process::Command::new("git")
                .args(&args)
                .current_dir(&repo)
                .status_locked()
                .unwrap()
                .success());
        }
        fs::write(
            repo.join(".gitignore"),
            "/target/\n/.gitpulse/\n/.claude/\n",
        )
        .unwrap();
        fs::write(
            repo.join("Cargo.toml"),
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        assert!(std::process::Command::new("git")
            .args(["add", ".gitignore", "Cargo.toml"])
            .current_dir(&repo)
            .status_locked()
            .unwrap()
            .success());
        assert!(std::process::Command::new("git")
            .args(["commit", "-q", "-m", "init"])
            .current_dir(&repo)
            .status_locked()
            .unwrap()
            .success());
        assert!(std::process::Command::new("git")
            .args([
                "worktree",
                "add",
                "-q",
                "-b",
                "wt-branch",
                wt.to_str().unwrap(),
                "HEAD",
            ])
            .current_dir(&repo)
            .status_locked()
            .unwrap()
            .success());

        crate::test_support::trust_repo(&repo);
        crate::test_support::trust_repo(&wt);

        let write_cargo_dir = |target: &Path, payload_len: usize, age: Duration| {
            fs::create_dir_all(target.join("debug")).unwrap();
            fs::write(target.join("debug/.cargo-lock"), "").unwrap();
            fs::write(
                target.join("CACHEDIR.TAG"),
                "Signature: 8a477f597d28d172789f06886806bc55\n",
            )
            .unwrap();
            fs::write(target.join(".rustc_info.json"), "{}").unwrap();
            let rlib = target.join("debug/libout.rlib");
            fs::write(&rlib, vec![b'x'; payload_len]).unwrap();
            let stamped = SystemTime::now() - age;
            for p in [
                target.join("debug/.cargo-lock"),
                rlib,
                target.join("debug"),
                target.join("CACHEDIR.TAG"),
                target.join(".rustc_info.json"),
                target.to_path_buf(),
            ] {
                crate::test_support::set_modified(&p, stamped).unwrap();
            }
            if let Some(parent) = target.parent() {
                let _ = crate::test_support::set_modified(parent, stamped);
            }
        };

        // 1. Recent (1 hour old, < 7d retention!) repo scratch build (20 KiB).
        let recent_repo_scratch = repo.join(".gitpulse/scratch/lane-recent/target");
        write_cargo_dir(&recent_repo_scratch, 20_480, Duration::from_secs(3_600));

        // 2. Recent (30 min old, < 7d retention!) finished-session scratchpad build (16 KiB).
        //    Only inspectable if NO running Claude sessions are unmatched on this host; if the
        //    developer's live machine currently has an unidentified Claude session, we verify
        //    the repo + worktree candidates without requiring host-global session state.
        let uid = crate::storage::hygiene::agent::current_uid();
        let sess_dir = sess_tmp.join(format!("claude-{uid}/proj-x/session-recent-dead-9999"));
        let recent_sess_target = sess_dir.join("scratchpad/target");
        write_cargo_dir(&recent_sess_target, 16_384, Duration::from_secs(1_800));
        crate::test_support::set_modified(
            &sess_dir,
            SystemTime::now() - Duration::from_secs(1_800),
        )
        .unwrap();

        // 3. Aged (10d old) idle linked-worktree Cargo `target` (12 KiB).
        let wt_target = wt.join("target");
        write_cargo_dir(&wt_target, 12_288, Duration::from_secs(10 * 86_400));

        // 4. Aged (10d old) main-checkout Cargo `target` (8 KiB) — Manvi cloud chooses to KEEP it!
        let main_target = repo.join("target");
        write_cargo_dir(&main_target, 8_192, Duration::from_secs(10 * 86_400));

        // 5. Aged (10d old) `.claude/keep-target` (4 KiB) — Manvi cloud chooses to KEEP it!
        let claude_keep = repo.join(".claude/keep-target");
        write_cargo_dir(&claude_keep, 4_096, Duration::from_secs(10 * 86_400));

        let severe_disk = GovernorInput {
            readings: Readings {
                free_disk: Reading::Value(3 << 30),
                ..Readings::calm()
            },
            ..GovernorInput::test_pressure(Level::Critical)
        };
        let _gov = inject_governor_with_history(std::iter::repeat_n(severe_disk, 10));
        assert!(crate::sysload::disk_remedy_due());

        let repo_str = repo.to_string_lossy().into_owned();
        let load = make_load(
            SensorState::Fresh,
            Tier::Severe,
            Some(Binding::LowDisk),
            false,
        );
        let seen_prompts: RefCell<Vec<String>> = RefCell::new(Vec::new());

        let eval = |_sys: &str, user: &str, _max_tokens: i64| -> Result<String, String> {
            seen_prompts.borrow_mut().push(user.to_string());
            let parsed: serde_json::Value = serde_json::from_str(user).unwrap();
            assert_eq!(parsed["host"]["disk_critical"], true);
            let cands = parsed["candidates"].as_array().unwrap();
            // Select ONLY the recent repo scratch build, the idle worktree target, and
            // (if present) the recent session scratchpad — plus a hallucinated id `c999`
            // and path `/etc/passwd` to prove unknown IDs are ignored safely!
            let mut pick: Vec<String> = cands
                .iter()
                .filter(|c| {
                    matches!(
                        c["category"].as_str(),
                        Some("repo_agent_scratch" | "worktree_cargo_target" | "session_scratchpad")
                    ) && c["target"].as_str() != Some("scratch:.claude/keep-target")
                })
                .map(|c| c["id"].as_str().unwrap().to_string())
                .collect();
            pick.push("c999".into());
            pick.push("/etc/passwd".into());
            Ok(format!(
                "```json\n{}\n```",
                json!({
                    "selected_ids": pick,
                    "rationale": "Reclaim recent scratch builds and idle worktree target while preserving main checkout cache."
                })
            ))
        };

        let out = run_critical_hygiene_pass_impl(
            std::slice::from_ref(&repo_str),
            Some(&repo_str),
            true,
            false,
            7,
            true,
            Some(&load),
            Some(&sess_tmp),
            Some(&eval),
            &crate::storage::hygiene::agent::LiveSessions::default,
            &|_| Ok(()),
        );

        assert_eq!(
            seen_prompts.borrow().len(),
            1,
            "Manvi cloud evaluated inventory once"
        );
        let prompt_json: serde_json::Value =
            serde_json::from_str(&seen_prompts.borrow()[0]).unwrap();
        assert_eq!(prompt_json["inventory"]["truncated"], false);
        assert_eq!(
            prompt_json["inventory"]["shown"],
            prompt_json["inventory"]["total"]
        );
        let prompt_cands = prompt_json["candidates"].as_array().unwrap();
        let recent_entry = prompt_cands
            .iter()
            .find(|c| c["target"] == "scratch:.gitpulse/scratch/lane-recent/target")
            .expect(
                "1-hour-old scratch build must be offered when disk_critical=true in cloud mode",
            );
        assert_eq!(recent_entry["recent_below_retention"], true);
        assert!(
            recent_entry["bytes"].is_u64(),
            "cloud inventory must measure bytes on scratch targets: {recent_entry:?}"
        );
        // Clone-private bytes come from APFS (`storage::clone_size`, macOS
        // only). Elsewhere the inventory must say they were not measured, not
        // offer a number it never read.
        if cfg!(target_os = "macos") {
            assert!(
                recent_entry["private_bytes"].is_u64(),
                "cloud inventory must measure private_bytes on scratch targets: {recent_entry:?}"
            );
        } else {
            assert!(
                recent_entry["private_bytes"].is_null(),
                "private_bytes is only measured on APFS, so it must be absent here: {recent_entry:?}"
            );
        }
        let recent_sess_entry = prompt_cands
            .iter()
            .find(|c| {
                c["category"] == "session_scratchpad"
                    && c["target"]
                        .as_str()
                        .is_some_and(|t| t.contains("session-recent-dead-9999"))
            })
            .expect("30-min-old finished-session scratchpad must be offered when disk_critical=true in cloud mode");
        assert_eq!(recent_sess_entry["recent_below_retention"], true);
        assert!(
            prompt_cands
                .iter()
                .any(|c| c["category"] == "worktree_cargo_target"
                    && c["target"] == "worktree:target"),
            "idle linked-worktree target must be offered in inventory: {prompt_cands:?}"
        );

        assert!(
            !recent_repo_scratch.exists(),
            "recent 1h-old .gitpulse/scratch target selected by Manvi cloud must be reclaimed: {out:?}"
        );
        assert!(
            !recent_sess_target.exists(),
            "recent 30m-old finished-session scratchpad selected by Manvi cloud must be reclaimed: {out:?}"
        );
        assert!(
            !wt_target.exists(),
            "idle linked-worktree target selected by Manvi cloud must be reclaimed: {out:?}"
        );
        assert!(
            main_target.exists(),
            "unselected main-checkout target must be preserved!"
        );
        assert!(
            claude_keep.exists(),
            "unselected .claude/keep-target must be preserved!"
        );
        assert!(
            out.reclaimed_bytes >= 32_000,
            "expected >= 32 KiB reclaimed, got {out:?}"
        );
        assert!(
            out.warnings
                .iter()
                .any(|w| w.contains("unrecognized candidate id")),
            "hallucinated candidate IDs must be flagged and ignored: {:?}",
            out.warnings
        );
    }

    #[cfg(unix)]
    #[test]
    fn manvi_cloud_failure_or_non_disk_pressure_never_deletes_recent_scratch_without_cloud_selection(
    ) {
        let _serial = crate::storage::hygiene::tests::serial();
        use crate::procguard::LockedSpawn;
        use crate::sysload::{
            inject_governor_with_history, GovernorInput, Level, Reading, Readings,
        };

        let tmp = tempfile::tempdir().unwrap();
        let repo = crate::engine::process::canonicalize_plain(tmp.path()).unwrap();
        for args in [
            vec!["init", "-q", "-b", "main"],
            vec!["config", "user.name", "Steward Test"],
            vec!["config", "user.email", "steward@example.com"],
        ] {
            assert!(std::process::Command::new("git")
                .args(&args)
                .current_dir(&repo)
                .status_locked()
                .unwrap()
                .success());
        }
        fs::write(repo.join(".gitignore"), "/.gitpulse/\n").unwrap();
        assert!(std::process::Command::new("git")
            .args(["add", ".gitignore"])
            .current_dir(&repo)
            .status_locked()
            .unwrap()
            .success());
        assert!(std::process::Command::new("git")
            .args(["commit", "-q", "-m", "init"])
            .current_dir(&repo)
            .status_locked()
            .unwrap()
            .success());
        crate::test_support::trust_repo(&repo);

        let make_scratch = |name: &str, age: Duration| -> PathBuf {
            let target = repo.join(format!(".gitpulse/scratch/{name}/target"));
            fs::create_dir_all(target.join("debug")).unwrap();
            fs::write(target.join("debug/.cargo-lock"), "").unwrap();
            fs::write(
                target.join("CACHEDIR.TAG"),
                "Signature: 8a477f597d28d172789f06886806bc55\n",
            )
            .unwrap();
            fs::write(target.join(".rustc_info.json"), "{}").unwrap();
            let rlib = target.join("debug/libx.rlib");
            fs::write(&rlib, vec![b'k'; 4_096]).unwrap();
            let stamped = SystemTime::now() - age;
            for p in [
                target.join("debug/.cargo-lock"),
                rlib,
                target.join("debug"),
                target.join("CACHEDIR.TAG"),
                target.join(".rustc_info.json"),
                target.clone(),
                repo.join(format!(".gitpulse/scratch/{name}")),
            ] {
                crate::test_support::set_modified(&p, stamped).unwrap();
            }
            target
        };

        let recent = make_scratch("recent-2h", Duration::from_secs(7_200));
        let aged = make_scratch("aged-10d", Duration::from_secs(10 * 86_400));

        let severe_disk = GovernorInput {
            readings: Readings {
                free_disk: Reading::Value(3 << 30),
                ..Readings::calm()
            },
            ..GovernorInput::test_pressure(Level::Critical)
        };
        let _gov = inject_governor_with_history(std::iter::repeat_n(severe_disk, 10));

        let repo_str = repo.to_string_lossy().into_owned();
        let load = make_load(
            SensorState::Fresh,
            Tier::Severe,
            Some(Binding::LowDisk),
            false,
        );

        // Case 1: Manvi cloud returns valid `selected_ids: []` -> keeps EVERYTHING (even aged!).
        let keep_all = |_: &str, _: &str, _: i64| -> Result<String, String> {
            Ok(r#"{"selected_ids":[],"rationale":"Hold caches while build settles."}"#.into())
        };
        let kept_out = run_critical_hygiene_pass_full(
            std::slice::from_ref(&repo_str),
            Some(&repo_str),
            false,
            7,
            true,
            Some(&load),
            None,
            Some(&keep_all),
        );
        assert_eq!(kept_out.reclaimed_bytes, 0);
        assert!(recent.exists() && aged.exists());
        assert!(kept_out
            .items
            .iter()
            .any(|i| i.status == "skipped" && i.detail.contains("Preserved by Manvi cloud")));

        // Case 2: Non-disk critical (`disk_critical = false`) NEVER offers `< 7d` recent scratch
        //         AND on LLM error deletes NOTHING when disk is not critical.
        let non_disk_err = |_: &str, user: &str, _: i64| -> Result<String, String> {
            let parsed: serde_json::Value = serde_json::from_str(user).unwrap();
            let cands = parsed["candidates"].as_array().unwrap();
            assert!(
                cands.iter().all(|c| c["recent_below_retention"] == false),
                "disk_critical=false must not include recent <7d scratch builds: {cands:?}"
            );
            Err("upstream timeout".into())
        };
        let non_disk_out = run_critical_hygiene_pass_full(
            std::slice::from_ref(&repo_str),
            Some(&repo_str),
            false,
            7,
            false,
            Some(&load),
            None,
            Some(&non_disk_err),
        );
        assert_eq!(non_disk_out.reclaimed_bytes, 0);
        assert!(non_disk_out.deferred_or_busy);
        assert!(recent.exists() && aged.exists());

        // Case 3: Disk IS critical (`disk_critical = true`) and Manvi cloud returns malformed JSON:
        //         falls back to >= 7d static policy -> reclaims `aged` (10d) and PRESERVES `recent` (2h)!
        let bad_json =
            |_: &str, _: &str, _: i64| -> Result<String, String> { Ok("sorry, not json".into()) };
        let fallback_out = run_critical_hygiene_pass_full(
            std::slice::from_ref(&repo_str),
            Some(&repo_str),
            false,
            7,
            true,
            Some(&load),
            None,
            Some(&bad_json),
        );
        assert!(
            recent.exists(),
            "recent <7d scratch build must survive when Manvi cloud reply is unparseable"
        );
        assert!(
            !aged.exists(),
            "aged >=7d scratch build must still be reclaimed on critical-disk fallback: {fallback_out:?}"
        );
        assert!(fallback_out.reclaimed_bytes >= 4_000);

        // Case 4: Critical swap is not a disk problem: even with Manvi cloud,
        //         deleting build caches frees no memory, so nothing is cleaned.
        let rt = StewardRuntime::default();
        let mut deps = base_deps(tmp.path());
        deps.load = make_load(SensorState::Fresh, Tier::Severe, Some(Binding::Swap), false);
        deps.disk_remedy = false;
        let swap_tick = evaluate_tick_with(&rt, "critical_wake", None, &deps);
        assert_eq!(swap_tick.outcome, "acted");
        assert_eq!(
            deps.clean_calls.borrow().len(),
            0,
            "cleanup waits for disk to be what is critical, cloud model or not"
        );

        // 2 minutes later, disk itself becomes critical (`disk_remedy = true`): cleaned at once.
        deps.now_ms += 120_000;
        deps.load = make_load(
            SensorState::Fresh,
            Tier::Severe,
            Some(Binding::LowDisk),
            false,
        );
        deps.disk_remedy = true;
        let disk_tick = evaluate_tick_with(&rt, "critical_wake", None, &deps);
        assert_eq!(disk_tick.outcome, "acted");
        assert_eq!(
            deps.clean_calls.borrow().len(),
            1,
            "critical disk is cleaned on the tick it is seen"
        );
    }

    #[test]
    fn parse_cloud_clean_selection_handles_braced_prose_fenced_blocks_and_rejects_truncation() {
        // 1. Prose containing `{...}` BEFORE and AFTER the JSON object (previously broke
        //    `find('{')..=rfind('}')` and triggered indiscriminate static fallback!).
        let braced_prose = r#"Given {"disk_critical": true} and host {low_disk}, reclaim only c1:
{"selected_ids":["c1","c3"],"rationale":"Keep main {target} cache warm."}
Trailing note {done}."#;
        let parsed = parse_cloud_clean_selection(braced_prose).expect("must extract valid JSON");
        assert_eq!(parsed.selected_ids, vec!["c1", "c3"]);
        assert_eq!(parsed.rationale, "Keep main {target} cache warm.");

        // 2. Markdown code fence preceded and followed by prose.
        let prose_wrapped_fence = "Here is my selection:\n```json\n{\"selected_ids\":[\"c2\"],\"rationale\":\"Remove orphaned scratchpad.\"}\n```\nMain target preserved.";
        let fenced =
            parse_cloud_clean_selection(prose_wrapped_fence).expect("must parse fenced block");
        assert_eq!(fenced.selected_ids, vec!["c2"]);

        // 3. Truncated / incomplete JSON must be rejected.
        assert!(parse_cloud_clean_selection(r#"{"selected_ids":["c1","c2"#).is_err());
        assert!(parse_cloud_clean_selection(r#"{"other_field":123}"#).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn pre_mutation_rechecks_worktree_dirty_policy_disable_and_discovers_closed_trusted_repos() {
        let _serial = crate::storage::hygiene::tests::serial();
        use crate::procguard::LockedSpawn;
        use crate::sysload::{
            inject_governor_with_history, GovernorInput, Level, Reading, Readings,
        };

        let tmp = tempfile::tempdir().unwrap();
        let base = crate::engine::process::canonicalize_plain(tmp.path()).unwrap();
        let repo = base.join("repo");
        let wt = base.join("wt-race");
        let closed_root = base.join("workspace/closed-repo");
        fs::create_dir_all(&repo).unwrap();
        fs::create_dir_all(&closed_root).unwrap();

        for dir in [&repo, &closed_root] {
            for args in [
                vec!["init", "-q", "-b", "main"],
                vec!["config", "user.name", "Steward Test"],
                vec!["config", "user.email", "steward@example.com"],
            ] {
                assert!(std::process::Command::new("git")
                    .args(&args)
                    .current_dir(dir)
                    .status_locked()
                    .unwrap()
                    .success());
            }
            fs::write(dir.join(".gitignore"), "/target/\n/.gitpulse/\n").unwrap();
            fs::write(
                dir.join("Cargo.toml"),
                "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
            )
            .unwrap();
            assert!(std::process::Command::new("git")
                .args(["add", ".gitignore", "Cargo.toml"])
                .current_dir(dir)
                .status_locked()
                .unwrap()
                .success());
            assert!(std::process::Command::new("git")
                .args(["commit", "-q", "-m", "init"])
                .current_dir(dir)
                .status_locked()
                .unwrap()
                .success());
        }
        assert!(std::process::Command::new("git")
            .args([
                "worktree",
                "add",
                "-q",
                "-b",
                "wt-race-branch",
                wt.to_str().unwrap(),
                "HEAD",
            ])
            .current_dir(&repo)
            .status_locked()
            .unwrap()
            .success());

        crate::test_support::trust_repo(&repo);
        crate::test_support::trust_repo(&wt);
        crate::test_support::trust_repo(&closed_root);

        let write_aged_cargo = |target: &Path| {
            fs::create_dir_all(target.join("debug")).unwrap();
            fs::write(target.join("debug/.cargo-lock"), "").unwrap();
            fs::write(
                target.join("CACHEDIR.TAG"),
                "Signature: 8a477f597d28d172789f06886806bc55\n",
            )
            .unwrap();
            fs::write(target.join(".rustc_info.json"), "{}").unwrap();
            let rlib = target.join("debug/libout.rlib");
            fs::write(&rlib, vec![b'a'; 8_192]).unwrap();
            let stamped = SystemTime::now() - Duration::from_secs(10 * 86_400);
            for p in [
                target.join("debug/.cargo-lock"),
                rlib,
                target.join("debug"),
                target.join("CACHEDIR.TAG"),
                target.join(".rustc_info.json"),
                target.to_path_buf(),
            ] {
                crate::test_support::set_modified(&p, stamped).unwrap();
            }
            if let Some(parent) = target.parent() {
                let _ = crate::test_support::set_modified(parent, stamped);
            }
        };

        let wt_target = wt.join("target");
        let main_target = repo.join("target");
        let repo_scratch = repo.join(".gitpulse/scratch/race/target");
        write_aged_cargo(&wt_target);
        write_aged_cargo(&main_target);
        write_aged_cargo(&repo_scratch);

        let severe_disk = GovernorInput {
            readings: Readings {
                free_disk: Reading::Value(3 << 30),
                ..Readings::calm()
            },
            ..GovernorInput::test_pressure(Level::Critical)
        };
        let _gov = inject_governor_with_history(std::iter::repeat_n(severe_disk, 10));

        let repo_str = repo.to_string_lossy().into_owned();
        let load = make_load(
            SensorState::Fresh,
            Tier::Severe,
            Some(Binding::LowDisk),
            false,
        );

        // Part A: TOCTOU race — `wt` is clean during Stage 1, then dirties a source file
        //         while Manvi cloud is evaluating! `wt_target` MUST be skipped and preserved.
        let wt_for_eval = wt.clone();
        let dirty_wt_eval = move |_: &str, user: &str, _: i64| -> Result<String, String> {
            let parsed: serde_json::Value = serde_json::from_str(user).unwrap();
            let wt_id = parsed["candidates"]
                .as_array()
                .unwrap()
                .iter()
                .find(|c| c["target"] == "worktree:target")
                .expect("worktree:target offered during Stage 1")["id"]
                .as_str()
                .unwrap()
                .to_string();
            // Simulate developer/agent editing a file in `wt` while Manvi cloud is thinking!
            fs::write(wt_for_eval.join("new_untracked.rs"), "fn wip() {}\n").unwrap();
            Ok(json!({
                "selected_ids": [wt_id],
                "rationale": "Attempt to reclaim worktree target."
            })
            .to_string())
        };

        let race_out = run_critical_hygiene_pass_impl(
            std::slice::from_ref(&repo_str),
            Some(&repo_str),
            false,
            false,
            7,
            true,
            Some(&load),
            None,
            Some(&dirty_wt_eval),
            &crate::storage::hygiene::agent::LiveSessions::default,
            &|_| Ok(()),
        );
        assert_eq!(race_out.reclaimed_bytes, 0);
        assert!(
            wt_target.exists(),
            "worktree dirtied while Manvi cloud was answering MUST NOT have its target deleted: {race_out:?}"
        );
        assert!(
            race_out
                .items
                .iter()
                .any(|i| i.target == "worktree:target" && i.status == "skipped"),
            "dirtied worktree target must record status=skipped (not failed): {race_out:?}"
        );

        // Part B: Concurrent policy disable while Manvi cloud is answering skips BOTH
        //         `local:target` and `scratch:*`, preserves both on disk, and cancels
        //         every `HygienePlan` so `MAX_PLANS = 4` never leaks!
        let disabled_flag = std::cell::Cell::new(false);
        let select_all_eval = |_: &str, user: &str, _: i64| -> Result<String, String> {
            let parsed: serde_json::Value = serde_json::from_str(user).unwrap();
            let ids: Vec<String> = parsed["candidates"]
                .as_array()
                .unwrap()
                .iter()
                .map(|c| c["id"].as_str().unwrap().to_string())
                .collect();
            disabled_flag.set(true);
            Ok(json!({
                "selected_ids": ids,
                "rationale": "Select all before user disables steward."
            })
            .to_string())
        };
        let policy_gate = |_: Option<&str>| -> Result<(), String> {
            if disabled_flag.get() {
                Err("Steward critical cleanup was disabled before execution.".into())
            } else {
                Ok(())
            }
        };
        for _ in 0..5 {
            disabled_flag.set(false);
            let disabled_out = run_critical_hygiene_pass_impl(
                std::slice::from_ref(&repo_str),
                Some(&repo_str),
                false,
                false,
                7,
                true,
                Some(&load),
                None,
                Some(&select_all_eval),
                &crate::storage::hygiene::agent::LiveSessions::default,
                &policy_gate,
            );
            assert_eq!(disabled_out.reclaimed_bytes, 0);
            assert!(main_target.exists() && repo_scratch.exists());
        }
        // Prove zero `HygienePlan` slots leaked across those 5 aborted runs (`MAX_PLANS = 4`
        // means all 4 slots must still be free!):
        let mut verify_plans = Vec::new();
        for _ in 0..4 {
            let p = crate::engine::process::with_disk_relief(|| {
                crate::storage::hygiene::prepare(&repo_str, "local:target", 7)
            })
            .expect("no leaked plans in PLANS.pending");
            verify_plans.push(p.id);
        }
        for pid in verify_plans {
            crate::storage::hygiene::cancel(&repo_str, &pid).unwrap();
        }

        // Part C: Closed/idle trusted repository discovery (`merge_cleanup_candidate_repos` +
        //         `evaluate_tick_with` with zero open tabs / zero live agent sessions).
        let workspace_dir = base.join("workspace").to_string_lossy().into_owned();
        let closed_str = closed_root.to_string_lossy().into_owned();
        let merged = merge_cleanup_candidate_repos(
            Vec::new(),
            vec![repo.clone()],
            std::slice::from_ref(&workspace_dir),
            &[],
        );
        assert!(
            merged.contains(&repo_str) && merged.contains(&closed_str),
            "merge_cleanup_candidate_repos must discover approved checkouts and cleaner roots: {merged:?}"
        );

        let rt = StewardRuntime::default();
        let mut deps = base_deps(tmp.path());
        // Simulate NO open tabs and NO running agent sessions (`repos` is empty!), while
        // closed trusted repository `/closed/rust-app` is discovered for cleanup.
        deps.repos = Vec::new();
        deps.cleanup_repos = vec!["/closed/rust-app".into()];
        deps.trusted.insert("/closed/rust-app".into());
        deps.common_dirs
            .insert("/closed/rust-app".into(), "/closed/rust-app/.git".into());

        // On an Elevated tick (`should_clean == false`), cold closed repos are NOT cleaned or messaged.
        deps.load = make_load(
            SensorState::Fresh,
            Tier::Elevated,
            Some(Binding::CpuLoad),
            true,
        );
        let elev_rec = evaluate_tick_with(&rt, "scheduled", None, &deps);
        assert_eq!(elev_rec.outcome, "acted");
        assert!(deps.clean_calls.borrow().is_empty());
        assert!(deps.delivered.borrow().is_empty());

        // On critical storage (`disk_remedy = true`), `/closed/rust-app` IS cleaned AND anchors shared caches!
        deps.now_ms += 300_000;
        deps.load = make_load(
            SensorState::Fresh,
            Tier::Severe,
            Some(Binding::LowDisk),
            false,
        );
        deps.disk_remedy = true;
        let crit_rec = evaluate_tick_with(&rt, "critical_wake", None, &deps);
        assert_eq!(crit_rec.outcome, "acted");
        let calls = deps.clean_calls.borrow();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, vec!["/closed/rust-app".to_string()]);
        assert_eq!(calls[0].1.as_deref(), Some("/closed/rust-app"));
        assert!(calls[0].2);
    }

    fn agent_worker(pid: i32) -> AgentThrottleCandidate {
        AgentThrottleCandidate {
            pid,
            start_us: 7,
            name: "cargo".into(),
            session_pid: 4000,
            session_kind: AgentKind::Claude,
            session_cwd: Some("/trusted/a".into()),
            is_session_root: false,
            lineage_cwds: Vec::new(),
            ancestor_pids: Vec::new(),
        }
    }

    fn critical_memory() -> HostLoadStatus {
        make_load(
            SensorState::Fresh,
            Tier::Severe,
            Some(Binding::MemoryPressure),
            false,
        )
    }

    fn journal_entries(dir: &Path) -> Option<Vec<(i32, u64)>> {
        let raw = fs::read(dir.join(STEWARD_SUBDIR).join(THROTTLE_JOURNAL_FILE)).ok()?;
        Some(
            serde_json::from_slice::<ThrottleJournal>(&raw)
                .unwrap()
                .entries,
        )
    }

    #[test]
    fn inherited_throttles_are_tracked_and_none_are_left_in_the_background() {
        let tmp = tempfile::tempdir().unwrap();
        let rt = StewardRuntime::default();
        let mut deps = base_deps(tmp.path());
        deps.load = critical_memory();
        // 4200 was already in the background before the episode: not ours.
        deps.candidates = vec![agent_worker(4100), agent_worker(4200)];
        deps.already_background.borrow_mut().insert(4200);

        let first = evaluate_tick_with(&rt, "critical_wake", None, &deps);
        assert_eq!(first.throttled_processes, 1);
        assert_eq!(journal_entries(tmp.path()), Some(vec![(4100, 7)]));

        // A child of the throttled 4100 starts and inherits background priority.
        deps.candidates.push(AgentThrottleCandidate {
            ancestor_pids: vec![4100, 4000],
            ..agent_worker(4300)
        });
        deps.already_background.borrow_mut().insert(4300);
        deps.now_ms += 60_000;
        let second = evaluate_tick_with(&rt, "scheduled", None, &deps);
        assert_eq!(second.throttled_processes, 2);
        assert_eq!(
            journal_entries(tmp.path()),
            Some(vec![(4100, 7), (4300, 7)])
        );

        // 4100's session ended but 4100 still runs: it is restored, not forgotten.
        deps.candidates.retain(|c| c.pid != 4100);
        deps.now_ms += 60_000;
        let third = evaluate_tick_with(&rt, "scheduled", None, &deps);
        assert_eq!(third.throttled_processes, 1);
        assert!(deps.throttle_ops.borrow().contains(&(4100, false)));

        // Recovery restores the inherited child and never the one someone else
        // backgrounded, and the journal goes with the throttles.
        deps.load = make_load(SensorState::Fresh, Tier::Clear, None, true);
        deps.now_ms += 60_000;
        evaluate_tick_with(&rt, "scheduled", None, &deps);
        let ops = deps.throttle_ops.borrow();
        assert!(ops.contains(&(4300, false)), "{ops:?}");
        assert!(!ops.contains(&(4200, false)), "{ops:?}");
        assert_eq!(journal_entries(tmp.path()), None);
    }

    /// Runs a throttle episode where 4100 is throttled on the first tick and
    /// 4300 (already in the background) first shows up on a later tick under
    /// `ancestors`, then recovers. Returns the throttle ops.
    fn later_tick_background_candidate(ancestors: Vec<i32>) -> Vec<(i32, bool)> {
        let tmp = tempfile::tempdir().unwrap();
        let rt = StewardRuntime::default();
        let mut deps = base_deps(tmp.path());
        deps.load = critical_memory();
        deps.candidates = vec![agent_worker(4100)];
        evaluate_tick_with(&rt, "critical_wake", None, &deps);

        deps.candidates.push(AgentThrottleCandidate {
            ancestor_pids: ancestors,
            ..agent_worker(4300)
        });
        deps.already_background.borrow_mut().insert(4300);
        deps.now_ms += 60_000;
        evaluate_tick_with(&rt, "scheduled", None, &deps);

        deps.load = make_load(SensorState::Fresh, Tier::Clear, None, true);
        deps.now_ms += 60_000;
        evaluate_tick_with(&rt, "scheduled", None, &deps);
        let ops = deps.throttle_ops.borrow().clone();
        ops
    }

    #[test]
    fn a_later_background_candidate_under_a_throttled_parent_is_restored() {
        let ops = later_tick_background_candidate(vec![4400, 4100, 4000]);
        assert!(ops.contains(&(4300, false)), "{ops:?}");
        assert!(ops.contains(&(4100, false)), "{ops:?}");
    }

    #[test]
    fn a_later_background_candidate_with_an_unrelated_parent_is_left_alone() {
        // Backgrounded by the person or another supervisor: its parent was
        // never throttled by this episode, so recovery must not reset it.
        let ops = later_tick_background_candidate(vec![4900, 4000]);
        assert!(!ops.contains(&(4300, false)), "{ops:?}");
        assert!(ops.contains(&(4100, false)), "{ops:?}");
    }

    #[test]
    fn an_unreadable_manvi_check_keeps_the_slowdown_instead_of_standing_down() {
        let tmp = tempfile::tempdir().unwrap();
        let rt = StewardRuntime::default();
        let mut deps = base_deps(tmp.path());
        deps.load = critical_memory();
        deps.candidates = vec![agent_worker(4100)];
        deps.manvi = ManviProbe::Unreadable("timed out after 10s".into());

        // Nothing cached yet: recorded as a failed check, nothing released.
        crate::supervisor::lock_or_recover(&rt.inner)
            .throttled
            .insert(4100, (7, None));
        crate::supervisor::lock_or_recover(&rt.inner).critical_throttle_active = true;
        let unchecked = evaluate_tick_with(&rt, "scheduled", None, &deps);
        assert_eq!(unchecked.outcome, "failed");
        assert!(
            unchecked.summary.contains("Couldn't ask Manvi"),
            "the tick summary does not say Manvi could not be asked"
        );
        assert!(deps.throttle_ops.borrow().is_empty());
        assert!(crate::supervisor::lock_or_recover(&rt.inner)
            .throttled
            .contains_key(&4100));

        // With an earlier answer cached, the check reuses it and keeps acting.
        crate::supervisor::lock_or_recover(&rt.inner).cached_manvi = Some(cloud_manvi());
        deps.now_ms += 60_000;
        let reused = evaluate_tick_with(&rt, "scheduled", None, &deps);
        assert_eq!(reused.outcome, "acted");
        assert!(reused
            .warnings
            .iter()
            .any(|w| w.contains("could not be read")));
        assert!(crate::supervisor::lock_or_recover(&rt.inner).critical_throttle_active);
    }

    #[test]
    fn a_tick_that_finishes_after_shutdown_undoes_its_own_throttles() {
        let tmp = tempfile::tempdir().unwrap();
        let rt = StewardRuntime::default();
        let mut deps = base_deps(tmp.path());
        deps.load = critical_memory();
        deps.candidates = vec![agent_worker(4100)];
        rt.closed.store(true, Ordering::SeqCst);
        let rec = evaluate_tick_with(&rt, "critical_wake", None, &deps);
        assert_eq!(
            *deps.throttle_ops.borrow(),
            vec![(4100, true), (4100, false)]
        );
        assert!(crate::supervisor::lock_or_recover(&rt.inner)
            .throttled
            .is_empty());
        assert!(!tmp.path().join(STEWARD_SUBDIR).join(ACTIVE_FILE).exists());
        assert!(rec.warnings.iter().any(|w| w.contains("shutting down")));
    }

    #[test]
    fn the_lease_is_renewed_before_it_can_lapse_at_any_interval() {
        assert_eq!(lease_ttl_ms(60), 300_000);
        assert_eq!(lease_ttl_ms(180), 540_000);
        assert_eq!(lease_ttl_ms(3_600), 900_000);
        for interval in [60, 180, 300, 900, 3_600] {
            assert!(
                renew_interval_secs(interval) * 1_000 * 2 < lease_ttl_ms(interval),
                "interval {interval}s: two renewals fit in the lease"
            );
        }

        let rt = StewardRuntime::default();
        let settings = ManviStewardSettings {
            interval_secs: 3_600,
            slow_critical: true,
            ..ManviStewardSettings::default()
        };
        let now = 1_800_000_000_000;
        crate::supervisor::lock_or_recover(&rt.inner).last_check_ms = Some(now - 301_000);
        let (calm, _) = decide_worker_step_with_epoch(&rt, &settings, None, now);
        assert!(matches!(calm, WorkerDecision::Sleep(_)), "{calm:?}");
        crate::supervisor::lock_or_recover(&rt.inner).critical_throttle_active = true;
        let (held, _) = decide_worker_step_with_epoch(&rt, &settings, None, now);
        assert_eq!(held, WorkerDecision::Run("scheduled"));

        // The lease a tick publishes at the longest interval lasts the cap.
        let tmp = tempfile::tempdir().unwrap();
        let rt = StewardRuntime::default();
        let mut deps = base_deps(tmp.path());
        *deps.settings.borrow_mut() = settings;
        deps.load = critical_memory();
        deps.candidates = vec![agent_worker(4100)];
        let rec = evaluate_tick_with(&rt, "critical_wake", None, &deps);
        if let Some(lease) = published_lease(tmp.path(), &rec) {
            assert_eq!(lease.expires_at_ms - lease.issued_at_ms, 900_000);
        }
    }

    #[test]
    fn a_lease_longer_than_the_steward_issues_or_dated_ahead_is_not_believed() {
        let tmp = tempfile::tempdir().unwrap();
        let now = 1_800_000_000_000;
        let repo = crate::test_support::git_repo();
        let repo_str = repo.path().to_str().unwrap();
        let covered = canonical_common_dir(repo_str).unwrap();
        let lease = |issued: u64, expires: u64| ActiveThrottleRecord {
            version: LEASE_VERSION,
            writer_pid: std::process::id(),
            writer_start_us: 1,
            issued_at_ms: issued,
            expires_at_ms: expires,
            tier: "severe".into(),
            binding: "memory_pressure".into(),
            reason: "test".into(),
            pace_ms: CRITICAL_PACE_MS,
            repositories: vec![covered.clone()],
        };
        let read = |record: ActiveThrottleRecord| {
            write_active_throttle(tmp.path(), &record).unwrap();
            active_throttle_for_cwd_with_settings(tmp.path(), repo_str, now, None, slowing_on)
        };
        assert!(read(lease(now, now + 900_000)).is_some());
        assert!(
            read(ActiveThrottleRecord {
                version: 1,
                ..lease(now, now + 900_000)
            })
            .is_none(),
            "a version-1 lease predates the repository list it must carry"
        );
        assert!(
            read(lease(now, now + 3_600_000)).is_none(),
            "forged hour-long lease"
        );
        assert!(
            read(lease(now + 120_000, now + 600_000)).is_none(),
            "issued in the future"
        );
    }

    /// Where the lease goes when this platform can read its own process start
    /// time: the file a tick publishes. A platform that reads none (neither
    /// Unix nor Windows) publishes no lease and says so in its warnings, and
    /// the command hook has nothing to trust.
    fn published_lease(dir: &Path, rec: &StewardTickRecord) -> Option<ActiveThrottleRecord> {
        let file = dir.join(STEWARD_SUBDIR).join(ACTIVE_FILE);
        if cfg!(any(unix, windows)) {
            let raw = fs::read(&file).unwrap();
            return Some(serde_json::from_slice(&raw).unwrap());
        }
        assert!(!file.exists(), "a lease was published with no start time");
        assert!(
            rec.warnings
                .iter()
                .any(|w| w.contains("could not read its own process start time")),
            "{:?}",
            rec.warnings
        );
        None
    }

    fn slowing_on() -> ManviStewardSettings {
        ManviStewardSettings {
            slow_critical: true,
            ..ManviStewardSettings::default()
        }
    }

    #[test]
    fn a_lease_paces_only_inside_the_trusted_repositories_the_steward_named() {
        let tmp = tempfile::tempdir().unwrap();
        let now = 1_800_000_000_000;
        let (named_repo, other_repo) = (
            crate::test_support::git_repo(),
            crate::test_support::git_repo(),
        );
        let (named, other) = (
            named_repo.path().to_path_buf(),
            other_repo.path().to_path_buf(),
        );
        fs::create_dir_all(named.join("src")).unwrap();
        let outside = tmp.path().join("no-repo");
        fs::create_dir_all(&outside).unwrap();
        write_active_throttle(
            tmp.path(),
            &ActiveThrottleRecord {
                version: LEASE_VERSION,
                writer_pid: std::process::id(),
                writer_start_us: 1,
                issued_at_ms: now,
                expires_at_ms: now + 300_000,
                tier: "severe".into(),
                binding: "swap".into(),
                reason: "test".into(),
                pace_ms: CRITICAL_PACE_MS,
                repositories: vec![canonical_common_dir(named.to_str().unwrap()).unwrap()],
            },
        )
        .unwrap();
        let read = |cwd: &Path| {
            active_throttle_for_cwd_with_settings(
                tmp.path(),
                cwd.to_str().unwrap(),
                now,
                None,
                slowing_on,
            )
        };
        assert!(read(&named).is_some());
        assert!(read(&named.join("src")).is_some(), "anywhere inside it");
        assert!(
            read(&other).is_none(),
            "a repository the steward did not name"
        );
        assert!(read(&outside).is_none(), "outside every repository");
    }

    #[test]
    fn slowing_skips_agents_outside_trusted_repositories_and_the_lease_names_only_trusted_ones() {
        let tmp = tempfile::tempdir().unwrap();
        let rt = StewardRuntime::default();
        let mut deps = base_deps(tmp.path());
        deps.load = critical_memory();
        deps.common_dirs
            .insert("/untrusted/b".into(), "/untrusted/b/.git".into());
        let in_repo = |pid: i32, cwd: Option<&str>| AgentThrottleCandidate {
            session_cwd: cwd.map(str::to_owned),
            ..agent_worker(pid)
        };
        // A build script runs in its package's directory in the Cargo
        // registry; the `cargo` above it says which repository it serves.
        let registry = "/home/dev/.cargo/registry/src/index/serde-1.0.0";
        let under = |pid: i32, session_repo: &str| AgentThrottleCandidate {
            lineage_cwds: vec![registry.into(), session_repo.into()],
            ancestor_pids: Vec::new(),
            ..in_repo(pid, Some(registry))
        };
        deps.candidates = vec![
            in_repo(4100, Some("/trusted/a")),
            in_repo(4500, Some("/untrusted/b")),
            in_repo(4600, None),
            under(4700, "/trusted/a"),
            under(4800, "/untrusted/b"),
        ];
        let rec = evaluate_tick_with(&rt, "critical_wake", None, &deps);
        assert_eq!(rec.throttled_processes, 2);
        assert_eq!(
            *deps.throttle_ops.borrow(),
            vec![(4100, true), (4700, true)],
            "an untrusted repository's agent, or one in no repository, is never slowed; a build script outside the repository is judged by the session it serves"
        );
        if let Some(lease) = published_lease(tmp.path(), &rec) {
            assert_eq!(lease.repositories, vec!["/trusted/a/.git".to_string()]);
        }
    }

    #[test]
    fn out_of_the_box_the_steward_messages_but_neither_cleans_nor_slows() {
        let tmp = tempfile::tempdir().unwrap();
        let rt = StewardRuntime::default();
        let mut deps = base_deps(tmp.path());
        *deps.settings.borrow_mut() = ManviStewardSettings::default();
        deps.load = make_load(
            SensorState::Fresh,
            Tier::Severe,
            Some(Binding::LowDisk),
            false,
        );
        deps.disk_remedy = true;
        deps.candidates = vec![agent_worker(4100)];
        let rec = evaluate_tick_with(&rt, "critical_wake", None, &deps);
        assert_eq!(rec.outcome, "acted");
        assert!(!deps.delivered.borrow().is_empty(), "agents are messaged");
        assert!(deps.clean_calls.borrow().is_empty(), "nothing is deleted");
        assert!(deps.throttle_ops.borrow().is_empty(), "nothing is slowed");
        assert!(!tmp.path().join(STEWARD_SUBDIR).join(ACTIVE_FILE).exists());
    }

    #[test]
    fn a_crashed_gitpulses_throttles_are_restored_unless_it_is_still_running() {
        let tmp = tempfile::tempdir().unwrap();
        let restored = RefCell::new(Vec::new());
        let restore = |pid: i32, start_us: u64| {
            restored.borrow_mut().push((pid, start_us));
            Ok(true)
        };
        let throttled = BTreeMap::from([(4100, (7, None)), (4300, (9, Some("/r/.git".into())))]);
        write_throttle_journal(tmp.path(), &throttled).unwrap();
        // Written by this process, as by a GitPulse that has since died.
        assert_eq!(restore_throttle_journal(tmp.path(), restore), 2);
        assert_eq!(*restored.borrow(), vec![(4100, 7), (4300, 9)]);
        assert_eq!(journal_entries(tmp.path()), None);

        // A journal whose writer is another, still-running process is its own.
        // The live writer is this process's parent, which only Unix's std
        // names.
        #[cfg(unix)]
        {
            let parent = std::os::unix::process::parent_id();
            let Some(parent_start) = crate::sysload::process_start_us(parent) else {
                return;
            };
            let foreign = ThrottleJournal {
                version: 1,
                writer_pid: parent,
                writer_start_us: parent_start,
                entries: vec![(4100, 7)],
            };
            write_private_steward_file(
                tmp.path(),
                THROTTLE_JOURNAL_FILE,
                &serde_json::to_vec(&foreign).unwrap(),
            )
            .unwrap();
            restored.borrow_mut().clear();
            assert_eq!(restore_throttle_journal(tmp.path(), restore), 0);
            assert!(restored.borrow().is_empty());
            assert_eq!(journal_entries(tmp.path()), Some(vec![(4100, 7)]));
        }
    }

    #[test]
    fn only_an_anthropic_routes_provider_auth_code_marks_the_key_rejected() {
        let failure = |code: &str| ManviError {
            code: code.into(),
            message: "invalid x-api-key".into(),
        };
        assert!(
            model_call_error("anthropic", failure(crate::ai::MANVI_PROVIDER_AUTH))
                .credential_rejected
        );
        // The text names a key, but the code is an outage: not a rejection.
        assert!(!model_call_error("anthropic", failure("E_DEPENDENCY")).credential_rejected);
        // A local server's refusal is not about the Claude key.
        assert!(
            !model_call_error("local", failure(crate::ai::MANVI_PROVIDER_AUTH)).credential_rejected
        );
    }

    #[test]
    fn the_cleanup_selection_tells_the_steward_what_its_answer_says_about_the_key() {
        take_key_answers_noted_here();
        let refused = cleanup_evaluator("claude-a", |_, _, _| {
            Err(ModelCallError {
                message: "E_PROVIDER_AUTH: anthropic: 401".into(),
                credential_rejected: true,
            })
        });
        assert_eq!(
            refused("s", "u", 8),
            Err("E_PROVIDER_AUTH: anthropic: 401".to_string())
        );
        let answered = cleanup_evaluator("claude-a", |_, _, _| Ok("[]".to_string()));
        assert_eq!(answered("s", "u", 8), Ok("[]".to_string()));
        let outage = cleanup_evaluator("claude-a", |_, _, _| {
            Err(ModelCallError::from("E_DEPENDENCY: invalid x-api-key"))
        });
        assert!(outage("s", "u", 8).is_err());
        assert_eq!(
            take_key_answers_noted_here(),
            vec![
                ("claude-a".to_string(), Some(true)),
                ("claude-a".to_string(), Some(false)),
                ("claude-a".to_string(), None),
            ]
        );
    }

    #[test]
    fn only_a_real_anthropic_call_says_whether_the_key_works() {
        let refused: Result<serde_json::Value, ManviError> = Err(ManviError {
            code: crate::ai::MANVI_PROVIDER_AUTH.into(),
            message: "anthropic: 401".into(),
        });
        let outage: Result<serde_json::Value, ManviError> = Err(ManviError {
            code: "E_DEPENDENCY".into(),
            message: "invalid x-api-key".into(),
        });
        let answered: Result<serde_json::Value, ManviError> = Ok(json!({"ok": true}));
        assert_eq!(key_answer_of("anthropic", false, &refused), Some(true));
        assert_eq!(key_answer_of("anthropic", false, &answered), Some(false));
        // A dry run reaches no provider, so it proves nothing about the key.
        assert_eq!(key_answer_of("anthropic", true, &answered), None);
        assert_eq!(key_answer_of("anthropic", false, &outage), None);
        // A local server's answer is not about the Claude key.
        assert_eq!(key_answer_of("local", false, &refused), None);
        assert_eq!(key_answer_of("local", false, &answered), None);
    }

    #[test]
    fn a_rejection_is_forgotten_by_a_working_call_a_new_key_or_a_new_choice() {
        let mut state = InnerState::default();
        assert!(record_cloud_key_answer(&mut state, "claude-a", Some(true)));
        assert_eq!(state.credential_rejected.as_deref(), Some("claude-a"));
        assert!(!record_cloud_key_answer(&mut state, "claude-a", None));
        assert_eq!(state.credential_rejected.as_deref(), Some("claude-a"));
        // A call on any Anthropic model succeeding proves the key works.
        assert!(record_cloud_key_answer(&mut state, "claude-b", Some(false)));
        assert_eq!(state.credential_rejected, None);

        record_cloud_key_answer(&mut state, "claude-a", Some(true));
        forget_credential(&mut state);
        assert_eq!(state.credential_rejected, None, "a saved or deleted key");

        record_cloud_key_answer(&mut state, "claude-a", Some(true));
        adopt_manvi_status(&mut state, &cloud_manvi());
        assert_eq!(state.credential_rejected, None, "a new manvi use choice");
    }

    #[test]
    fn a_key_anthropic_refuses_reads_failed_until_a_call_with_it_succeeds() {
        let tmp = tempfile::tempdir().unwrap();
        let rt = StewardRuntime::default();
        let mut deps = base_deps(tmp.path());
        deps.load = make_load(
            SensorState::Fresh,
            Tier::Elevated,
            Some(Binding::CpuLoad),
            true,
        );
        let status = |rt: &StewardRuntime, deps: &FakeDeps| {
            build_status_with_env(rt, deps.settings(), &cloud_manvi(), None, None)
        };
        assert_eq!(status(&rt, &deps).manvi_state, "ready");

        // A failure that is not the provider refusing the key changes nothing.
        deps.llm_reply = Err("E_DEPENDENCY: overloaded".into());
        evaluate_tick_with(&rt, "scheduled", None, &deps);
        assert_eq!(*deps.llm_calls.borrow(), 1);
        assert_eq!(status(&rt, &deps).manvi_state, "ready");

        // Anthropic refused the key: the steward still messages with its own
        // directive, and the status stops calling the model configured.
        deps.llm_reply = Err(ModelCallError {
            message: "E_PROVIDER_AUTH: assist completion unavailable: anthropic: 401".into(),
            credential_rejected: true,
        });
        deps.now_ms += 1_000;
        let refused = evaluate_tick_with(&rt, "scheduled", None, &deps);
        assert_eq!(*deps.llm_calls.borrow(), 2);
        assert!(!refused.model_generated);
        let rejected = status(&rt, &deps);
        assert_eq!(rejected.manvi_state, "rejected");
        assert_eq!(rejected.manvi_detail, crate::ai::REJECTED_KEY_MESSAGE);
        assert!(
            rejected.next_check_ms.is_some(),
            "loaded ticks keep checking so a replaced key is noticed"
        );
        // The refusal was about that cloud model's key: a local model chosen
        // since, or another cloud model, does not read rejected.
        let local = build_status_with_env(&rt, deps.settings(), &local_manvi(), None, None);
        assert_eq!(local.manvi_state, "ready");
        let mut other_cloud = cloud_manvi();
        other_cloud.model = "claude-opus-5-5".into();
        let other = build_status_with_env(&rt, deps.settings(), &other_cloud, None, None);
        assert_eq!(other.manvi_state, "ready");

        // A later outage does not clear it; a call that succeeds does.
        deps.llm_reply = Err("E_DEPENDENCY: overloaded".into());
        deps.now_ms += 1_000;
        evaluate_tick_with(&rt, "scheduled", None, &deps);
        assert_eq!(status(&rt, &deps).manvi_state, "rejected");
        deps.llm_reply = Ok("CPU load is elevated. Cap cargo builds to 2 jobs.".into());
        deps.now_ms += 1_000;
        let fixed = evaluate_tick_with(&rt, "scheduled", None, &deps);
        assert!(fixed.model_generated);
        assert_eq!(status(&rt, &deps).manvi_state, "ready");
    }

    #[test]
    fn a_shared_cache_is_not_pruned_under_a_repository_that_opted_out_since_planning() {
        // The person switched cleanup off for /opted/out (or stopped trusting
        // it) after the pass was planned; the global switch is still on.
        let asked = std::cell::RefCell::new(Vec::new());
        let policy = |scope: Option<&str>| -> Result<(), String> {
            asked.borrow_mut().push(scope.map(str::to_string));
            match scope {
                Some("/opted/out") => Err("cleanup is off for this repository".into()),
                _ => Ok(()),
            }
        };

        assert_eq!(
            recheck_target_policy("/opted/out", "cache:cargo-registry", &policy),
            Err("cleanup is off for this repository".to_string())
        );
        assert_eq!(*asked.borrow(), vec![None, Some("/opted/out".to_string())]);

        // A repository-owned target is judged by its repository alone.
        asked.borrow_mut().clear();
        assert!(recheck_target_policy("/opted/out", "target", &policy).is_err());
        assert_eq!(*asked.borrow(), vec![Some("/opted/out".to_string())]);

        // Under a repository that still allows it, a shared cache passes only
        // after the global switch has been asked too.
        asked.borrow_mut().clear();
        assert_eq!(
            recheck_target_policy("/still/on", "cache:cargo-registry", &policy),
            Ok(())
        );
        assert_eq!(*asked.borrow(), vec![None, Some("/still/on".to_string())]);

        // A global refusal stops a shared cache before the repository is asked.
        let global_off = |scope: Option<&str>| -> Result<(), String> {
            scope
                .map(|_| ())
                .ok_or_else(|| "steward cleanup is off".to_string())
        };
        assert_eq!(
            recheck_target_policy("/still/on", "cache:cargo-registry", &global_off),
            Err("steward cleanup is off".to_string())
        );
    }

    #[test]
    fn the_steward_never_runs_a_user_confirmed_target() {
        let temp = tempfile::tempdir().unwrap();
        let backup = temp.path().join("workbench.sqlite.pre-schema-11.bak");
        std::fs::write(&backup, "rollback copy").unwrap();
        let mut out = CleanPassOutcome::default();
        let allow = |_: Option<&str>| -> Result<(), String> { Ok(()) };

        let proceed = execute_single_hygiene_target(
            "/not/a/repo",
            "backup:workbench.sqlite.pre-schema-11.bak",
            "backup:workbench.sqlite.pre-schema-11.bak",
            0,
            false,
            None,
            None,
            false,
            "",
            &[],
            &allow,
            &mut out,
        );

        assert!(proceed);
        assert_eq!(out.items.len(), 1);
        assert_eq!(out.items[0].status, "skipped");
        assert!(
            out.items[0].detail.contains("user-confirmed"),
            "{}",
            out.items[0].detail
        );
        assert_eq!(out.reclaimed_bytes, 0);
        assert!(backup.exists());
    }
}
