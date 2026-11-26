//! Locate and drive the installed `devmap` binary.
//!
//! Modelled on [`crate::harness::sidecar::resolve_binary`]: a GUI app does not
//! inherit a shell `PATH`, and `devmap` typically lives in `~/.cargo/bin`. An
//! explicit override that does not resolve is refused rather than searched past
//! — a path somebody typed is a statement, and quietly falling back hides the
//! typo behind whatever else is installed.

use crate::engine::git_cli::validate_repo;
use crate::engine::process::{self, BoundedRun};
use crate::ipc_error::CommandError;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Wall-clock budget for a cold or incremental build. Large repos can take
/// minutes; this is a backstop against a wedged child, not a target.
pub const BUILD_DEADLINE: Duration = Duration::from_secs(15 * 60);

/// Wall-clock budget for `devmap status --json`.
pub const STATUS_DEADLINE: Duration = Duration::from_secs(30);

/// Wall-clock budget for one `devmap preview` call.
pub const PREVIEW_DEADLINE: Duration = Duration::from_secs(60);

/// Coalesce bursty preview requests for the same `(repo, file)`.
///
/// Matches the live-index watcher debounce: a fixture rewrite storm that
/// restages the same paths must not spawn one CLI child per FS event.
pub const PREVIEW_DEBOUNCE: Duration = Duration::from_millis(200);

const PREVIEW_SKIP_NON_SOURCE: &str =
    "path is not indexable source (testdata/fixtures/prose/data); preview skipped";
const PREVIEW_SKIP_DEBOUNCED: &str =
    "preview debounced for this file; at most one CLI call per debounce window";

/// Cap on captured stdout. A status/preview JSON is small; a build report can
/// carry coverage gaps. 8 MiB is a hard ceiling, not a typical size.
pub const STDOUT_CAP: usize = 8 * 1024 * 1024;

type DebounceKey = (String, String);

fn preview_debounces() -> &'static Mutex<HashMap<DebounceKey, Instant>> {
    static MAP: OnceLock<Mutex<HashMap<DebounceKey, Instant>>> = OnceLock::new();
    MAP.get_or_init(|| Mutex::new(HashMap::new()))
}

#[cfg(test)]
pub(crate) fn clear_preview_debounces() {
    if let Ok(mut map) = preview_debounces().lock() {
        map.clear();
    }
}

/// Claims the debounce slot for `key`, or says it is still held.
///
/// Check and insert happen under the one `&mut` borrow of the map, so two
/// callers racing on the same file cannot both be admitted. Expired slots are
/// evicted on every call: a window lasts [`PREVIEW_DEBOUNCE`], so the map
/// holds at most the files touched within one window rather than every file
/// ever previewed.
fn admit_preview_slot(
    map: &mut HashMap<DebounceKey, Instant>,
    key: DebounceKey,
    now: Instant,
) -> bool {
    map.retain(|_, until| *until > now);
    if map.contains_key(&key) {
        return false;
    }
    map.insert(key, now + PREVIEW_DEBOUNCE);
    true
}

/// Whether `devmap preview` should run for this relative path.
///
/// Aligns with the kernel's admission rules, then additionally refuses Data /
/// prose / fixture trees: those are indexed (or excluded) but never worth a
/// speculative-edit CLI spawn — the 1,860-preview storm was almost entirely
/// `testdata/**/*.json` fixture rewrites.
pub fn preview_path_eligible(rel_path: &str) -> bool {
    let norm = rel_path.replace('\\', "/");
    if norm.trim().is_empty() {
        return false;
    }
    if !devmap_extract::is_indexable_source(&norm) {
        return false;
    }
    if devmap_extract::wiring::is_fixture_path(&norm) {
        return false;
    }
    let lang = devmap_extract::detect_language(Path::new(&norm));
    !matches!(
        devmap_extract::languages::liveness_unit_for_language(lang),
        devmap_extract::languages::LivenessUnit::Data
    )
}

fn skipped_preview_file(path: &str, reason: &str) -> PreviewFileResult {
    PreviewFileResult {
        file_path: path.to_string(),
        available: false,
        reason: Some(reason.into()),
        report: None,
    }
}

/// How the binary was found — reported so "GitPulse cannot find devmap" and
/// "GitPulse found a different devmap than your shell" stay distinguishable.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DevmapLookup {
    ExplicitEnv,
    /// Path from persisted `tools.json`.
    SavedConfig,
    PathSearch,
    #[cfg(test)]
    TestOverride,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedDevmap {
    pub path: String,
    pub lookup: DevmapLookup,
}

/// Outcome of a build / refresh invocation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildOutcome {
    pub ok: bool,
    pub binary: String,
    pub lookup: DevmapLookup,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
    pub report: Option<Value>,
}

/// Parsed `devmap status --json` plus locator metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CliStatus {
    pub available: bool,
    pub binary: Option<String>,
    pub lookup: Option<DevmapLookup>,
    pub reason: Option<String>,
    pub status: Option<Value>,
    /// Present when the spawn gate declined to start the probe under load, so
    /// nothing was examined: the status is not unavailable, it is unknown. Typed
    /// so a reader never has to read the sentence in `reason` to tell "devmap
    /// is missing" from "ask again shortly".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deferral: Option<Deferral>,
}

/// What a deferred status still knows.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Deferral {
    /// The last answer this process got from `devmap status` for the
    /// repository, or `None` when it never had one.
    pub last_good: Option<LastGoodIndex>,
}

/// The last good `devmap status` reading of a repository, and how old it is.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LastGoodIndex {
    pub generation_id: Option<u64>,
    /// Milliseconds since that reading was taken.
    pub age_ms: u64,
    /// Whether the index matches the tree *now*: `true`, `false` or `null`
    /// (not known), the same three readings DevMap's own `source_freshness`
    /// gives. Never `true` here: nothing was re-examined, and a reading from
    /// before the deferral cannot vouch for the tree after it. `false` is kept
    /// when that reading already said the index was stale (a build may have
    /// since fixed it, but it was not fresh when last seen); otherwise `null`.
    pub fresh: Option<bool>,
    /// Why `fresh` is what it is.
    pub fresh_reason: String,
}

/// Last good readings kept, one per repository path. A bound so a process
/// that opens many repositories over a long session does not grow it.
const LAST_GOOD_CAP: usize = 256;

fn last_good_readings() -> &'static Mutex<HashMap<String, GoodReading>> {
    static READINGS: OnceLock<Mutex<HashMap<String, GoodReading>>> = OnceLock::new();
    READINGS.get_or_init(|| Mutex::new(HashMap::new()))
}

struct GoodReading {
    generation_id: Option<u64>,
    is_fresh: Option<bool>,
    at: Instant,
}

/// Remembers `status` as the last good reading of `repo`.
fn remember_good_status(repo: &Path, status: &Value) {
    let mut readings = last_good_readings()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let key = repo.to_string_lossy().into_owned();
    if readings.len() >= LAST_GOOD_CAP && !readings.contains_key(&key) {
        if let Some(oldest) = readings
            .iter()
            .min_by_key(|(_, reading)| reading.at)
            .map(|(key, _)| key.clone())
        {
            readings.remove(&oldest);
        }
    }
    readings.insert(
        key,
        GoodReading {
            generation_id: status.get("generation_id").and_then(Value::as_u64),
            is_fresh: status.get("is_fresh").and_then(Value::as_bool),
            at: Instant::now(),
        },
    );
}

/// What a deferred status of `repo` can still say about its last good reading.
fn deferral_for(repo: &Path) -> Deferral {
    let readings = last_good_readings()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let last_good = readings
        .get(repo.to_string_lossy().as_ref())
        .map(|reading| {
            let age = reading.at.elapsed();
            let (fresh, said) = match reading.is_fresh {
                Some(false) => (Some(false), "it already said the index was stale"),
                Some(true) => (None, "it said the index was fresh then"),
                None => (None, "it did not say whether the index was fresh"),
            };
            LastGoodIndex {
                generation_id: reading.generation_id,
                age_ms: u64::try_from(age.as_millis()).unwrap_or(u64::MAX),
                fresh,
                fresh_reason: format!(
                    "devmap status was deferred under load, so freshness was not re-checked; \
                 the last reading was {} s ago and {said}",
                    age.as_secs()
                ),
            }
        });
    Deferral { last_good }
}

/// One file's preview result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreviewFileResult {
    pub file_path: String,
    pub available: bool,
    pub reason: Option<String>,
    pub report: Option<Value>,
}

/// Batch preview over changed files.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreviewOutcome {
    pub available: bool,
    pub binary: Option<String>,
    pub lookup: Option<DevmapLookup>,
    pub reason: Option<String>,
    pub files: Vec<PreviewFileResult>,
    /// True when the caller cancelled mid-batch (or a per-repo build lock was held).
    pub cancelled: bool,
    /// True when the batch was capped before every file was previewed.
    #[serde(default)]
    pub truncated: bool,
    #[serde(default)]
    pub files_total: usize,
    #[serde(default)]
    pub files_omitted: usize,
}

/// Keep in sync with `codeintel::MAX_NEIGHBOR_TARGETS` and TS `CODEINTEL_FANOUT_CAP`.
pub const MAX_PREVIEW_FILES: usize = 16;
pub const PREVIEW_FANOUT_OMITTED_REASON: &str =
    "preview fan-out capped; this file was not previewed";

fn build_guards() -> &'static Mutex<HashSet<String>> {
    static GUARDS: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    GUARDS.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Test seam: a binary path returned ahead of every real lookup.
#[cfg(test)]
static TEST_BINARY: Mutex<Option<String>> = Mutex::new(None);

/// Test seam: a canned [`status`] answer returned instead of running the probe.
///
/// A test that is about the *decision* taken from a status payload must not
/// also be a test of whether a child process reaches `main` and answers inside
/// [`STATUS_DEADLINE`]. Spawning a stub to deliver a fixed JSON document makes
/// the deadline the subject: when the host is loaded the probe answers late,
/// `status` reports `available: false`, and every such test fails as
/// `SkipUnavailable` — a verdict about the host, not about the code under test.
/// Installed only through [`bind_test_status`], which owns the same serial as
/// [`TEST_BINARY`] and clears both on drop.
#[cfg(test)]
static TEST_STATUS: Mutex<Option<CliStatus>> = Mutex::new(None);

/// Test seam: how long the canned [`status`] takes to answer, and the flag that
/// ends the wait early. A probe that hangs is the case the live index's IPC path
/// must survive, and a real hang is a 30 s child nobody wants a test to wait for.
#[cfg(all(test, unix))]
static TEST_STATUS_DELAY: Mutex<Option<(Duration, std::sync::Arc<std::sync::atomic::AtomicBool>)>> =
    Mutex::new(None);

/// Test seam: how many times [`status`] served the canned answer, so a test can
/// say a second probe was not started rather than infer it from timing.
#[cfg(test)]
static TEST_STATUS_CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Every test that installs a global `devmap` override takes
/// [`crate::test_support::env::env_serial`], the process's one environment lock.
///
/// `TEST_BINARY` is process-wide, so two tests that bind their own stub at the
/// same time resolve each other's binary. The symptom is not a clean failure:
/// each stub appends argv to a log inside *its own* fixture, so one test
/// asserts against a log that is missing a spawn while another test's log
/// quietly holds it — and which test fails moves from run to run. A private
/// lock here was also a lock that readers of `GITPULSE_TOOL_CONFIG` under a
/// different one never took, which is how `resolve_binary` could observe a
/// concurrently bound config.
#[cfg(test)]
fn set_test_binary(path: Option<String>) {
    *TEST_BINARY
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = path;
}

#[cfg(test)]
fn set_test_status(status: Option<CliStatus>) {
    *TEST_STATUS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = status;
}

/// An installed override, held for as long as the test needs it.
///
/// The only way to bind one, which is the point: `set_test_binary` is private
/// so a test cannot install a global override without also taking the serial
/// that makes owning it exclusive. Unbinding is this type's `Drop`, so it also
/// cannot be forgotten on an early return or a panic.
#[cfg(test)]
pub(crate) struct TestBinaryBinding(
    /// Held, never read: the serial's whole job is to be released on drop.
    #[allow(dead_code)]
    crate::test_support::env::EnvGuard,
);

#[cfg(test)]
impl Drop for TestBinaryBinding {
    fn drop(&mut self) {
        set_test_binary(None);
        // Both overrides live behind the one serial this binding owns, so both
        // are released here. Leaving a canned status installed would hand it to
        // whichever test took the serial next.
        set_test_status(None);
        #[cfg(unix)]
        release_test_status_delay();
    }
}

// Callers live in `#[cfg(unix)]` tests. A `#[cfg(test)]` method with no Windows
// caller is dead code, and `cargo clippy --all-targets -D warnings` then fails
// the Windows CI leg the release gate requires to be green.
#[cfg(all(test, unix))]
impl TestBinaryBinding {
    /// Answer [`status`] with `status` instead of running the probe, until this
    /// is called again or the binding is dropped.
    ///
    /// A method rather than a free function so the override cannot be installed
    /// without holding the serial: the binding *is* the proof of exclusivity.
    pub(crate) fn set_status(&self, status: CliStatus) {
        set_test_status(Some(status));
    }

    /// [`Self::set_status`] for a probe that takes `delay` to answer. The
    /// returned flag ends the wait early; the binding's drop sets it, so a
    /// probe thread a test left parked never outlives the override.
    pub(crate) fn set_status_delayed(
        &self,
        status: CliStatus,
        delay: Duration,
    ) -> std::sync::Arc<std::sync::atomic::AtomicBool> {
        let release = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        *TEST_STATUS_DELAY
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some((delay, std::sync::Arc::clone(&release)));
        set_test_status(Some(status));
        release
    }
}

/// How many times [`status`] answered from the canned override since the
/// process started. Compare two readings; the absolute number is shared.
#[cfg(all(test, unix))]
pub(crate) fn test_status_calls() -> usize {
    TEST_STATUS_CALLS.load(std::sync::atomic::Ordering::SeqCst)
}

/// Park the calling probe for the installed delay, or until it is released.
#[cfg(all(test, unix))]
fn wait_test_status_delay() {
    let installed = TEST_STATUS_DELAY
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    let Some((delay, release)) = installed else {
        return;
    };
    let deadline = Instant::now() + delay;
    while Instant::now() < deadline && !release.load(std::sync::atomic::Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Remove the installed delay and end any wait in progress.
#[cfg(all(test, unix))]
fn release_test_status_delay() {
    if let Some((_, release)) = TEST_STATUS_DELAY
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take()
    {
        release.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Install `path` as the binary every lookup resolves, until the returned
/// binding is dropped.
#[cfg(test)]
pub(crate) fn bind_test_binary(path: impl Into<String>) -> TestBinaryBinding {
    let serial = crate::test_support::env::env_serial();
    set_test_binary(Some(path.into()));
    TestBinaryBinding(serial)
}

/// A `devmap status --json` answer that parsed, for a test that is about what
/// the decision logic does with it.
#[cfg(all(test, unix))]
pub(crate) fn available_status(payload: &str) -> CliStatus {
    CliStatus {
        available: true,
        binary: Some("test-override".into()),
        lookup: Some(DevmapLookup::TestOverride),
        reason: None,
        status: Some(
            serde_json::from_str(payload).expect("canned status payload must be valid JSON"),
        ),
        deferral: None,
    }
}

/// A probe that did not answer — the shape [`status`] returns when the child
/// could not be run, did not exit inside [`STATUS_DEADLINE`], or wrote
/// something that was not a JSON object.
#[cfg(all(test, unix))]
pub(crate) fn unavailable_status(reason: &str) -> CliStatus {
    CliStatus {
        available: false,
        binary: Some("test-override".into()),
        lookup: Some(DevmapLookup::TestOverride),
        reason: Some(reason.into()),
        status: None,
        deferral: None,
    }
}

/// Resolves the `devmap` binary (cached, including negative answers).
///
/// Order: test override → `GITPULSE_DEVMAP_BIN` (refuse if set but missing) →
/// saved config → shared PATH + GUI-fallback search.
pub fn resolve_binary() -> Result<ResolvedDevmap, String> {
    #[cfg(test)]
    {
        if let Some(explicit) = TEST_BINARY
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
        {
            return Ok(ResolvedDevmap {
                path: explicit,
                lookup: DevmapLookup::TestOverride,
            });
        }
    }
    // Explicit env is never cached as Absent across a typo fix in the same
    // process without invalidate — but refuse-on-broken must stay uncached
    // past a missing file: callers need the specific error every time.
    if let Ok(explicit) = std::env::var("GITPULSE_DEVMAP_BIN") {
        let path = PathBuf::from(&explicit);
        if path.is_file() {
            return Ok(ResolvedDevmap {
                path: explicit,
                lookup: DevmapLookup::ExplicitEnv,
            });
        }
        return Err(format!(
            "GITPULSE_DEVMAP_BIN is set to {explicit}, but that path is not a file"
        ));
    }

    let path =
        crate::tool_capability::resolve_cached(crate::tool_install::ExternalTool::Devmap, || {
            resolve_binary_uncached().map(|r| r.path)
        })?;
    // Re-tag lookup for the cached path.
    if crate::tool_config::saved_binary(crate::tool_install::ExternalTool::Devmap).as_deref()
        == Some(path.as_str())
    {
        return Ok(ResolvedDevmap {
            path,
            lookup: DevmapLookup::SavedConfig,
        });
    }
    Ok(ResolvedDevmap {
        path,
        lookup: DevmapLookup::PathSearch,
    })
}

/// Uncached resolve used by the capability cache and by tool_install status.
pub fn resolve_binary_uncached() -> Result<ResolvedDevmap, String> {
    #[cfg(test)]
    {
        if let Some(explicit) = TEST_BINARY
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
        {
            return Ok(ResolvedDevmap {
                path: explicit,
                lookup: DevmapLookup::TestOverride,
            });
        }
    }
    if let Ok(explicit) = std::env::var("GITPULSE_DEVMAP_BIN") {
        let path = PathBuf::from(&explicit);
        if path.is_file() {
            return Ok(ResolvedDevmap {
                path: explicit,
                lookup: DevmapLookup::ExplicitEnv,
            });
        }
        return Err(format!(
            "GITPULSE_DEVMAP_BIN is set to {explicit}, but that path is not a file"
        ));
    }
    if crate::tool_config::is_disabled(crate::tool_install::ExternalTool::Devmap) {
        return Err("devmap is disabled in GitPulse settings".into());
    }
    if let Some(saved) = crate::tool_config::saved_binary(crate::tool_install::ExternalTool::Devmap)
    {
        return Ok(ResolvedDevmap {
            path: saved,
            lookup: DevmapLookup::SavedConfig,
        });
    }
    let bin_name = if cfg!(windows) {
        "devmap.exe"
    } else {
        "devmap"
    };
    // Also search the app-owned bin dir from prebuilt installs.
    if let Ok(app_bin) = crate::tool_install::release::app_bin_dir() {
        let candidate = app_bin.join(bin_name);
        if candidate.is_file() {
            return Ok(ResolvedDevmap {
                path: candidate.display().to_string(),
                lookup: DevmapLookup::PathSearch,
            });
        }
    }
    process::find_external_tool(bin_name)
        .map(|path| ResolvedDevmap {
            path,
            lookup: DevmapLookup::PathSearch,
        })
        .ok_or_else(|| {
            "devmap is not installed (searched env, saved config, PATH and GUI fallbacks; set GITPULSE_DEVMAP_BIN or run setup)"
                .to_string()
        })
}

#[derive(Debug)]
struct BuildGuard {
    key: String,
}

impl BuildGuard {
    fn try_acquire(repo: &Path) -> Result<Self, String> {
        let key = repo.to_string_lossy().into_owned();
        let mut guards = build_guards()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !guards.insert(key.clone()) {
            return Err(format!(
                "a devmap build is already running for {key}; refuse to start a second"
            ));
        }
        Ok(Self { key })
    }
}

/// True while a build/refresh holds the per-repo guard — live refresh must
/// skip rather than queue a second child.
pub fn is_build_in_flight(repo_path: &str) -> bool {
    let Ok(repo) = validate_repo(repo_path) else {
        return false;
    };
    let key = repo.to_string_lossy();
    let guards = build_guards()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    guards.contains(key.as_ref())
}

impl Drop for BuildGuard {
    fn drop(&mut self) {
        let mut guards = build_guards()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guards.remove(&self.key);
    }
}

fn bytes_to_string(bytes: Vec<u8>) -> String {
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Caps the build child's rayon pool at half the logical cores, rounded up
/// (the same rule as GitPulse's own pool, [`crate::pools::rayon_threads`]),
/// unless the caller already set `RAYON_NUM_THREADS` on the command or the
/// environment GitPulse runs in has one: an explicit choice is never
/// overridden. Without a cap a build ran one rayon thread per core.
fn limit_build_threads(cmd: &mut Command) {
    const VAR: &str = "RAYON_NUM_THREADS";
    let on_command = cmd
        .get_envs()
        .any(|(key, value)| key == VAR && value.is_some());
    if on_command || std::env::var_os(VAR).is_some() {
        return;
    }
    let cpus = std::thread::available_parallelism().map_or(2, std::num::NonZeroUsize::get);
    cmd.env(VAR, crate::pools::rayon_threads(cpus).to_string());
}

/// Verbs that only read the store. Their child is `devmap`, not git, so they
/// are admitted outside the git spawn budget (see
/// [`process::with_unmetered_spawn_rate`]) and bound by [`ReadBudget`] instead.
/// A verb not named here (a build, `seed`, an `integrate` action) stays on the
/// git budget.
const READ_VERBS: [&str; 3] = ["status", "preview", "doctor"];

/// `devmap` reads in flight at once, across every repository of this process
/// (the bound is per process, not per user: the git budget's shared record does
/// not see these children).
///
/// The git budget cannot bound these: a read on a stacked lane starts git
/// inside `devmap`, where the gate does not see it, and every read costs the
/// store's CPU and file cache whatever it spawns. The slots are split by
/// admission class the way the git gate splits its own ([`Admission`]):
/// background reads (the code map's probes, at most `live::PROBE_LIMIT` of them)
/// may hold only [`BACKGROUND_READ_SLOTS`], and the rest are kept for the user's
/// own reads (a Refresh, doctor, preview: reactive or interactive work), so a
/// full set of probes, each holding a slot for as long as its child runs, can
/// never defer them.
pub(crate) const BACKGROUND_READ_SLOTS: usize = super::live::PROBE_LIMIT;

/// Slots kept for reads that are not background work.
const USER_READ_SLOTS: usize = 2;

pub(crate) const READ_SLOTS: usize = BACKGROUND_READ_SLOTS + USER_READ_SLOTS;

/// How long a read waits for one of [`READ_SLOTS`] before it is deferred. The
/// same bound a reactive git read waits for a spawn token.
const READ_WAIT: Duration = process::SPAWN_QUEUE_BUDGET;

/// The devmap read budget: counted slots with a bounded wait, a share of them
/// closed to background work.
pub(crate) struct ReadBudget {
    slots: usize,
    background_slots: usize,
    held: Mutex<ReadsHeld>,
    freed: std::sync::Condvar,
}

#[derive(Default)]
struct ReadsHeld {
    total: usize,
    background: usize,
    /// Reads that are not background work (reactive and interactive both) and
    /// are waiting for a slot. While any wait, a background read is admitted
    /// only if it is the first background read in flight, so a probe arriving
    /// as a slot frees cannot jump a user's read that was there first, and a
    /// steady stream of reactive reads cannot defer every probe forever. The
    /// git gate does the same, with a narrower count: it holds background back
    /// only for waiting interactive callers (`waiting_interactive`), and lets a
    /// waiting background caller run when no background child is running.
    waiting_user: usize,
}

/// One of the budget's slots, returned on drop.
pub(crate) struct ReadPermit<'a> {
    budget: &'a ReadBudget,
    background: bool,
}

impl Drop for ReadPermit<'_> {
    fn drop(&mut self) {
        {
            let mut held = self
                .budget
                .held
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            held.total -= 1;
            held.background -= usize::from(self.background);
        }
        // Waiters of different classes are blocked by different conditions, so
        // waking one could wake a background waiter that still cannot run.
        self.budget.freed.notify_all();
    }
}

impl ReadBudget {
    pub(crate) const fn new(slots: usize, background_slots: usize) -> Self {
        Self {
            slots,
            background_slots,
            held: Mutex::new(ReadsHeld {
                total: 0,
                background: 0,
                waiting_user: 0,
            }),
            freed: std::sync::Condvar::new(),
        }
    }

    /// (total, background) slots held now.
    #[cfg(test)]
    fn held_now(&self) -> (usize, usize) {
        let held = self
            .held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (held.total, held.background)
    }

    #[cfg(test)]
    fn waiting_user_now(&self) -> usize {
        self.held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .waiting_user
    }

    /// A slot for the calling thread's admission class, waiting at most `wait`
    /// for one. A refusal is a deferral in the shape every reader of one
    /// matches, and names this budget, not git's.
    pub(crate) fn admit(&self, wait: Duration) -> Result<ReadPermit<'_>, CommandError> {
        let background = process::current_admission() == process::Admission::Background;
        let cap = if background {
            self.background_slots.min(self.slots)
        } else {
            self.slots
        };
        let began = Instant::now();
        let mut held = self
            .held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut waiting = false;
        let outcome = loop {
            let open = held.total < self.slots
                && (!background
                    || (held.background < cap && (held.waiting_user == 0 || held.background == 0)));
            if open {
                break Ok(());
            }
            let left = wait.saturating_sub(began.elapsed());
            if left.is_zero() {
                break Err(process::deferred_under_load_error(
                    "devmap",
                    began.elapsed(),
                    &format!(
                        "{} of {} devmap read slots are already held and none was released in time",
                        if background {
                            held.background
                        } else {
                            held.total
                        },
                        if background { cap } else { self.slots }
                    ),
                ));
            }
            if !background && !waiting {
                waiting = true;
                held.waiting_user += 1;
            }
            held = self
                .freed
                .wait_timeout(held, left)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        };
        if waiting {
            held.waiting_user -= 1;
            // Background reads held back by this waiter may run now.
            self.freed.notify_all();
        }
        outcome?;
        held.total += 1;
        held.background += usize::from(background);
        Ok(ReadPermit {
            budget: self,
            background,
        })
    }
}

fn read_budget() -> &'static ReadBudget {
    #[cfg(test)]
    if let Some(budget) = TEST_READ_BUDGET.get() {
        return budget;
    }
    static BUDGET: ReadBudget = ReadBudget::new(READ_SLOTS, BACKGROUND_READ_SLOTS);
    &BUDGET
}

// The budget (and wait) this thread's reads use instead of the process's:
// set by `with_read_budget`, so a test can fill the slots without taking
// them from every other test.
#[cfg(test)]
thread_local! {
    static TEST_READ_BUDGET: std::cell::Cell<Option<&'static ReadBudget>> =
        const { std::cell::Cell::new(None) };
    static TEST_READ_WAIT: std::cell::Cell<Option<Duration>> =
        const { std::cell::Cell::new(None) };
}

#[cfg(test)]
fn with_read_budget<T>(budget: &'static ReadBudget, wait: Duration, body: impl FnOnce() -> T) -> T {
    let _restore = std::mem::DropGuard::new(
        (
            TEST_READ_BUDGET.replace(Some(budget)),
            TEST_READ_WAIT.replace(Some(wait)),
        ),
        |(budget, wait)| {
            TEST_READ_BUDGET.set(budget);
            TEST_READ_WAIT.set(wait);
        },
    );
    body()
}

fn read_wait() -> Duration {
    #[cfg(test)]
    if let Some(wait) = TEST_READ_WAIT.get() {
        return wait;
    }
    READ_WAIT
}

fn run_devmap(
    binary: &ResolvedDevmap,
    repo: &Path,
    args: &[&str],
    stdin: Option<&[u8]>,
    deadline: Duration,
) -> Result<BoundedRun, CommandError> {
    let mut cmd = Command::new(&binary.path);
    cmd.current_dir(repo);
    cmd.args(args);
    // stdout and stderr are independently drained. Plain build progress makes
    // the active phase diagnosable without changing the JSON payload.
    if args.contains(&"--json") && !args.contains(&"--progress") {
        cmd.arg("--progress")
            .arg(if args.first() == Some(&"build") {
                "always"
            } else {
                "never"
            });
    }
    let effective_args = cmd
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let mut diagnostic =
        super::diagnostics::CommandLog::start(binary, repo, &effective_args, deadline);
    let is_build = args.first() == Some(&"build") || args == SYNC_DELTA_ARGS;
    if is_build {
        limit_build_threads(&mut cmd);
    }
    let mut run = |cmd: &mut Command| {
        process::run_observed(cmd, "devmap", deadline, stdin, STDOUT_CAP, &mut diagnostic)
    };
    // An index build is background work whoever asked for it: it runs in the
    // OS background band (and with half the cores' worth of rayon threads, see
    // above) so it cannot take the machine from the UI.
    let result = if is_build {
        process::with_background_band(|| run(&mut cmd))
    } else if args.first().is_some_and(|verb| READ_VERBS.contains(verb)) {
        // A read starts `devmap`, not git: it is admitted outside the git
        // budget and held to the read budget instead.
        read_budget()
            .admit(read_wait())
            .and_then(|_slot| process::with_unmetered_spawn_rate(|| run(&mut cmd)))
    } else {
        run(&mut cmd)
    };
    let protocol_error = result
        .as_ref()
        .ok()
        .filter(|run| {
            run.success
                && run.incomplete.is_none()
                && run.stderr_incomplete.is_none()
                && args.contains(&"--json")
        })
        .and_then(|run| match serde_json::from_slice::<Value>(&run.stdout) {
            Ok(report) if !report.is_object() => {
                Some("devmap returned a JSON value instead of a report object".to_string())
            }
            Ok(report) if report.get("error").is_some_and(|error| !error.is_null()) => Some(
                "devmap returned an error report despite exit 0; see DevMap logs for details"
                    .to_string(),
            ),
            Ok(report) if report.get("ok") == Some(&Value::Bool(false)) => Some(
                "devmap returned ok=false despite exit 0; see DevMap logs for details".to_string(),
            ),
            Ok(_) => None,
            Err(error) => Some(format!("devmap returned non-JSON stdout: {error}")),
        });
    diagnostic.finish(&result, protocol_error.as_deref());
    if let Some(error) = protocol_error {
        return Err(error.into());
    }
    result?.require_complete("devmap")
}

/// Run `devmap` for a sibling module in this package.
///
/// `run_devmap` stays private because every caller in this file pairs it with
/// its own deadline, argument set and outcome shape; this is the single seam
/// `integrate` needs, with the same bounded-run, protocol-check and diagnostic
/// logging behaviour.
pub(super) fn run_devmap_public(
    binary: &ResolvedDevmap,
    repo: &Path,
    args: &[&str],
    deadline: Duration,
) -> Result<BoundedRun, CommandError> {
    run_devmap(binary, repo, args, None, deadline)
}

fn parse_json_stdout(stdout: &str) -> Option<Value> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return None;
    }
    serde_json::from_str(trimmed).ok()
}

/// Why a failed `--json` devmap run failed.
///
/// Under `--json` DevMap answers a refusal on stdout as one `{"error": ...}`
/// document and keeps its human lines off stderr, so stderr alone is empty for
/// exactly the failures that have a reason. The envelope's text is DevMap's own
/// and names the file or argument at fault; stderr is the fallback for a run
/// that wrote no envelope (an older binary, a crash), and `fallback` the last
/// resort. Never empty.
pub(super) fn failure_detail(stdout: &str, stderr: &str, fallback: String) -> String {
    let envelope = parse_json_stdout(stdout)
        .and_then(|doc| doc.get("error").and_then(Value::as_str).map(str::to_string))
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty());
    envelope
        .or_else(|| Some(stderr.trim().to_string()).filter(|text| !text.is_empty()))
        .unwrap_or(fallback)
}

/// A run error becomes a `timed_out` outcome only when the build started and
/// ran past its deadline. A build refused at the slot wait, deferred under
/// load, or that never spawned did not run out of time; it stays an `Err` so
/// the caller can tell "never ran" from "hung" (see
/// [`crate::engine::process::RunError::classify`]).
fn timed_out_outcome(
    binary: &ResolvedDevmap,
    err: CommandError,
) -> Result<BuildOutcome, CommandError> {
    if !process::is_run_timeout(err.message()) {
        return Err(err);
    }
    Ok(BuildOutcome {
        ok: false,
        binary: binary.path.clone(),
        lookup: binary.lookup.clone(),
        exit_code: None,
        stdout: String::new(),
        stderr: err.into_message(),
        timed_out: true,
        report: None,
    })
}

fn build_outcome_from_run(binary: &ResolvedDevmap, run: BoundedRun) -> BuildOutcome {
    let stdout = bytes_to_string(run.stdout);
    let stderr = bytes_to_string(run.stderr);
    BuildOutcome {
        ok: run.success,
        binary: binary.path.clone(),
        lookup: binary.lookup.clone(),
        exit_code: Some(run.status_code),
        report: parse_json_stdout(&stdout),
        stdout,
        stderr,
        timed_out: false,
    }
}

/// Spawn a build, having first made sure it cannot dirty `git status`.
///
/// The ignore hygiene lives here, at the one point every build path passes
/// through, because *building* is what creates the state directory. Doing it
/// only when a repository is opened was not enough: the live gate indexes
/// every retained tab while initialization runs for the active one, so a
/// repository sitting in a background tab was indexed by a rule that had never
/// been written for it and gained a permanent untracked `.devmap/`. The manual
/// **Build index** button reaches the same code from the other direction.
///
/// Idempotent and cheap — an already-ignored directory costs one
/// `git check-ignore` and returns — so it is affordable on a path that is
/// about to spawn a full index build.
///
/// A refusal does not stop the build. Hiding the directory is hygiene; the
/// index is what the user or the gate actually asked for, and a repository
/// with an unusual ignore setup must not silently lose its code intelligence
/// over a cosmetic concern. It is logged rather than swallowed, and
/// initialization surfaces the same refusal in the UI for the active tab.
fn spawn_build(repo_path: &str, args: &'static [&'static str]) -> Result<BuildOutcome, String> {
    prepare_args(repo_path, args)?.run_then(|outcome| outcome)
}

const MANIFEST_BUILD_ARGS: &[&str] = &["build", "--manifest", "--json"];
const INCREMENTAL_BUILD_ARGS: &[&str] = &["build", "--json"];
const SYNC_DELTA_ARGS: &[&str] = &["seed", "--sync-delta", "--json"];

/// A build whose writer slot is already claimed and whose child has not run.
///
/// Everything that must be settled before a caller can answer happens in
/// [`prepare`]: the repository is validated, the per-repository
/// [`BuildGuard`] is held and the binary is resolved. A caller that hands the
/// run to another thread therefore cannot race a second build in between —
/// [`is_build_in_flight`] is already true when `prepare` returns.
pub struct PreparedBuild {
    repo: PathBuf,
    binary: ResolvedDevmap,
    args: &'static [&'static str],
    guard: BuildGuard,
}

/// Claim the writer slot for a build of `repo_path` without running it.
///
/// `manifest` selects `devmap build --manifest --json` (see [`build`]) over the
/// incremental `devmap build --json` (see [`refresh`]).
pub fn prepare(repo_path: &str, manifest: bool) -> Result<PreparedBuild, String> {
    prepare_args(
        repo_path,
        if manifest {
            MANIFEST_BUILD_ARGS
        } else {
            INCREMENTAL_BUILD_ARGS
        },
    )
}

/// Claim the writer slot to sync a stacked worktree's `delta.sqlite`
/// (`devmap seed --sync-delta --json`) without running a full `devmap build`.
pub fn prepare_sync_delta(repo_path: &str) -> Result<PreparedBuild, String> {
    prepare_args(repo_path, SYNC_DELTA_ARGS)
}

fn prepare_args(repo_path: &str, args: &'static [&'static str]) -> Result<PreparedBuild, String> {
    claim_build(repo_path)
        .map_err(ClaimRefusal::into_message)?
        .prepare_args(args)
}

/// Why [`claim_build`] could not hand out a permit.
#[derive(Debug)]
pub enum ClaimRefusal {
    /// Another probe or build already holds the repository's writer slot.
    Busy(String),
    /// The path is not a repository this app may index; the message says why.
    Invalid(String),
}

impl ClaimRefusal {
    fn into_message(self) -> String {
        match self {
            ClaimRefusal::Busy(message) | ClaimRefusal::Invalid(message) => message,
        }
    }
}

/// The exclusive right to probe and build one repository's index.
///
/// The live gate used to take the per-repository [`BuildGuard`] only when its
/// build was about to spawn, so every caller that arrived while the first was
/// still asking `devmap status` ran a status child of its own, and one that
/// probed just before the first build finished then built a second time behind
/// it. A permit is the guard claimed *early*: held from the first question to
/// the last answer, it makes the probe and the build one operation per
/// repository. A concurrent caller is refused at once ([`ClaimRefusal::Busy`]),
/// which the live gate maps to `skip_building`; the frontend retries on its own
/// backoff, and that retry is the coalesced follow-up. It is the same guard
/// [`PreparedBuild`] holds, handed on by [`BuildPermit::prepare`], not a second
/// lock beside it.
#[derive(Debug)]
pub struct BuildPermit {
    repo: PathBuf,
    guard: BuildGuard,
}

/// Claim `repo_path`'s writer slot, refusing (never queueing) when it is held.
pub fn claim_build(repo_path: &str) -> Result<BuildPermit, ClaimRefusal> {
    let repo = validate_repo(repo_path).map_err(|e| ClaimRefusal::Invalid(e.into_message()))?;
    let guard = BuildGuard::try_acquire(&repo).map_err(ClaimRefusal::Busy)?;
    Ok(BuildPermit { repo, guard })
}

impl BuildPermit {
    /// [`prepare`] under this permit: the writer slot stays held, unbroken,
    /// from the claim to the end of the build.
    pub fn prepare(self, manifest: bool) -> Result<PreparedBuild, String> {
        self.prepare_args(if manifest {
            MANIFEST_BUILD_ARGS
        } else {
            INCREMENTAL_BUILD_ARGS
        })
    }

    /// [`prepare_sync_delta`] under this permit: keeps the per-repo writer slot
    /// held across probe -> stacked delta sync (`devmap seed --sync-delta --json`).
    pub fn prepare_sync_delta(self) -> Result<PreparedBuild, String> {
        self.prepare_args(SYNC_DELTA_ARGS)
    }

    fn prepare_args(self, args: &'static [&'static str]) -> Result<PreparedBuild, String> {
        let binary = resolve_binary()?;
        Ok(PreparedBuild {
            repo: self.repo,
            binary,
            args,
            guard: self.guard,
        })
    }
}

impl PreparedBuild {
    /// Run the child, then hand its outcome to `settle` while the writer slot
    /// is still held.
    ///
    /// Settling first is what lets a caller record what the build meant before
    /// anyone can observe the repository as idle: between the child exiting and
    /// the guard dropping, [`is_build_in_flight`] still answers true.
    pub fn run_then<R>(self, settle: impl FnOnce(Result<BuildOutcome, String>) -> R) -> R {
        let PreparedBuild {
            repo,
            binary,
            args,
            guard,
        } = self;
        if let super::init::ExcludeOutcome::Refused { reason } =
            super::init::ensure_state_dir_excluded(&repo)
        {
            log::warn!(
                target: "devmap",
                "{}: building an index whose state directory git will show as untracked — {reason}",
                repo.display()
            );
        }
        let outcome = match run_devmap(&binary, &repo, args, None, BUILD_DEADLINE) {
            Ok(run) => Ok(build_outcome_from_run(&binary, run)),
            Err(e) => timed_out_outcome(&binary, e),
        };
        // The build's caller reads the text; no kind reaches a command from here.
        let settled = settle(outcome.map_err(CommandError::into_message));
        drop(guard);
        settled
    }
}

/// Cold or full rebuild (`devmap build --manifest --json`).
///
/// The manifest flag is what writes the consumer artifacts `repo_map.json` and
/// `code_graph.json`; a plain build updates the database only.
pub fn build(repo_path: &str) -> Result<BuildOutcome, String> {
    spawn_build(repo_path, MANIFEST_BUILD_ARGS)
}

/// Incremental rebuild (`devmap build --json` without `--full`).
pub fn refresh(repo_path: &str) -> Result<BuildOutcome, String> {
    spawn_build(repo_path, INCREMENTAL_BUILD_ARGS)
}

/// Synchronize dirty/diverged files into an existing stacked worktree's
/// `delta.sqlite` (`devmap seed --sync-delta --json`) using real Tree-sitter.
pub fn sync_stacked_delta(repo_path: &str) -> Result<BuildOutcome, String> {
    spawn_build(repo_path, SYNC_DELTA_ARGS)
}

/// Installation health `devmap doctor --json` already measures and GitPulse
/// never showed.
///
/// Each field is a warning the kernel emits about the *installation* rather
/// than about any one repository: a second `devmap` on `PATH` shadowing the
/// one hooks call, the same unpinned MCP server registered in several hosts,
/// long-lived `devmap mcp` processes started before the current binary, and a
/// host plugin bundle whose version or hook shape no longer matches. Every one
/// of them makes a correct-looking answer come from the wrong binary, which is
/// precisely the failure a silent field cannot be debugged from.
///
/// The named fields are the ones GitPulse can order by consequence; they are
/// not the definition of what gets shown. Every `*_warning` key in the payload
/// is read, and one with no named field here lands in `extra_warnings` rather
/// than being dropped — see [`NAMED_WARNING_FIELDS`]. That distinction is the
/// whole reason this is not a list of five: devmap grew a sixth warning, the
/// hand-written list did not, and `stray_state_warning` fired on a user's
/// machine into a panel that reported nothing.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DoctorReport {
    pub available: bool,
    pub binary: Option<String>,
    pub reason: Option<String>,
    /// Two or more `devmap` binaries of different builds were found.
    pub binary_skew_warning: Option<String>,
    /// The same unpinned `devmap mcp` command registered more than once.
    pub duplicate_mcp_registration_warning: Option<String>,
    /// `devmap mcp` processes older than the installed binary's mtime.
    pub stale_server_warning: Option<String>,
    /// An installed host plugin bundle that no longer matches this binary.
    pub plugin_warning: Option<String>,
    /// A registration naming a `devmap` that is not there.
    pub missing_binary_warning: Option<String>,
    /// A DevMap state directory with no store beside it.
    pub stray_state_warning: Option<String>,
    /// Every other `*_warning` this binary emitted, as `key: text`.
    ///
    /// The named fields above are the ones GitPulse can order by consequence.
    /// This is what catches the next one. `stray_state_warning` shipped in
    /// devmap and went unrendered here for exactly as long as the field list
    /// was written by hand — a warning that fired on the user's machine and
    /// reached a panel that then said "found nothing to report".
    #[serde(default)]
    pub extra_warnings: Vec<String>,
    /// Schema this binary speaks, for the compatibility strip.
    pub expected_schema_version: Option<i64>,
    pub code_graph_schema_version: Option<i64>,
    pub linked_grammar_count: Option<i64>,
    pub version: Option<String>,
}

/// Payload keys [`DoctorReport`] has a named field for.
///
/// Anything else ending in `_warning` lands in `extra_warnings`. Kept beside
/// the struct so the two are edited together, and asserted field-for-field by
/// `every_named_warning_field_is_claimed` rather than trusted.
const NAMED_WARNING_FIELDS: &[&str] = &[
    "binary_skew_warning",
    "duplicate_mcp_registration_warning",
    "stale_server_warning",
    "plugin_warning",
    "missing_binary_warning",
    "stray_state_warning",
];

/// Longest one warning may be before GitPulse shortens it for display.
///
/// These strings are another program's output, rendered verbatim into a single
/// DOM node. `stale_server_warning` enumerates one process id per running
/// `devmap mcp`, and on the machine that reported this it listed 84 — every
/// Claude Code session on the host spawns two, because the server is
/// registered twice. That is already past reading; it is not a bound. The
/// bound is here, and it is generous enough that no warning devmap emits today
/// reaches it.
const MAX_WARNING_BYTES: usize = 2048;

/// Most warnings GitPulse will render at once.
///
/// devmap has six warning fields, so this can only be reached by a payload
/// inventing `*_warning` keys. Bounded anyway: `warnings` crosses IPC and
/// becomes DOM, and "the tool said so" is not a size limit.
const MAX_WARNINGS: usize = 32;

/// One warning, shortened if it has to be — and saying so with both numbers.
///
/// A shortened warning that does not admit it is the same failure this module
/// is full of guards against: the reader cannot tell a complete list of stale
/// processes from the first 2 KiB of one. Cut on a character boundary, because
/// these strings carry `—` and `…` and a raw byte slice panics inside one.
fn bound_warning(warning: &str) -> String {
    if warning.len() <= MAX_WARNING_BYTES {
        return warning.to_string();
    }
    let mut cut = MAX_WARNING_BYTES;
    while cut > 0 && !warning.is_char_boundary(cut) {
        cut -= 1;
    }
    format!(
        "{}… [GitPulse shortened this warning: {cut} of {} bytes shown]",
        &warning[..cut],
        warning.len()
    )
}

/// Read `devmap doctor --json`.
///
/// `repo` is where the probe runs. Doctor is documented read-only — it refuses
/// to create a store — so a scratch directory is a valid place to ask when no
/// repository is open, and is what the caller should pass rather than reaching
/// into an arbitrary checkout.
pub fn doctor(repo_path: &str) -> DoctorReport {
    let unavailable = |reason: String, binary: Option<String>| DoctorReport {
        available: false,
        binary,
        reason: Some(reason),
        ..DoctorReport::default()
    };
    let repo = match validate_repo(repo_path) {
        Ok(repo) => repo,
        Err(e) => return unavailable(e.into_message(), None),
    };
    let binary = match resolve_binary() {
        Ok(b) => b,
        Err(e) => return unavailable(e, None),
    };
    let run = match run_devmap(&binary, &repo, &["doctor", "--json"], None, STATUS_DEADLINE) {
        Ok(run) => run,
        Err(e) => return unavailable(e.into_message(), Some(binary.path)),
    };
    let stdout = bytes_to_string(run.stdout);
    let stderr = bytes_to_string(run.stderr);
    if !run.success {
        let detail = failure_detail(
            &stdout,
            &stderr,
            format!("devmap doctor exited {}", run.status_code),
        );
        return unavailable(detail, Some(binary.path));
    }
    let Some(payload) = parse_json_stdout(&stdout) else {
        return unavailable(
            "devmap doctor returned non-JSON stdout".into(),
            Some(binary.path),
        );
    };
    DoctorReport::from_payload(&payload, binary.path)
}

impl DoctorReport {
    /// Read one `devmap doctor --json` object.
    ///
    /// Split out of [`doctor`] so the parse can be exercised against payloads
    /// a spawned binary cannot be made to produce on demand — an unrecognised
    /// warning key, a warning that is not a string, a payload inventing
    /// hundreds of them.
    fn from_payload(payload: &Value, binary: String) -> Self {
        let text = |key: &str| {
            payload
                .get(key)
                .and_then(Value::as_str)
                .map(str::to_string)
                .filter(|value| !value.trim().is_empty())
        };
        let number = |key: &str| payload.get(key).and_then(Value::as_i64);
        // Swept, not enumerated: any `*_warning` this binary emits that
        // GitPulse has no named field for is carried through rather than
        // dropped. Sorted so the tail is stable across runs — serde_json
        // preserves object order, but the order two devmap builds emit keys in
        // is not GitPulse's to depend on.
        let mut extra_warnings: Vec<String> = payload
            .as_object()
            .into_iter()
            .flatten()
            .filter(|(key, _)| {
                key.ends_with("_warning") && !NAMED_WARNING_FIELDS.contains(&key.as_str())
            })
            .filter_map(|(key, value)| {
                let text = value.as_str()?.trim();
                (!text.is_empty()).then(|| format!("{key}: {text}"))
            })
            .collect();
        extra_warnings.sort();
        Self {
            available: true,
            binary: Some(binary),
            reason: None,
            binary_skew_warning: text("binary_skew_warning"),
            duplicate_mcp_registration_warning: text("duplicate_mcp_registration_warning"),
            stale_server_warning: text("stale_server_warning"),
            plugin_warning: text("plugin_warning"),
            missing_binary_warning: text("missing_binary_warning"),
            stray_state_warning: text("stray_state_warning"),
            extra_warnings,
            expected_schema_version: number("expected_schema_version"),
            code_graph_schema_version: number("code_graph_schema_version"),
            linked_grammar_count: number("linked_grammar_count"),
            version: text("version"),
        }
    }

    /// Every warning this report carries, in the order a reader should see
    /// them: the ones that change which binary answers come first.
    pub fn warnings(&self) -> Vec<String> {
        let mut all: Vec<String> = [
            self.missing_binary_warning.as_ref(),
            self.binary_skew_warning.as_ref(),
            self.stale_server_warning.as_ref(),
            self.duplicate_mcp_registration_warning.as_ref(),
            self.plugin_warning.as_ref(),
            self.stray_state_warning.as_ref(),
        ]
        .into_iter()
        .flatten()
        .cloned()
        // Unknown severity, so last — but never absent. A warning GitPulse
        // does not recognise is still a warning that fired.
        .chain(self.extra_warnings.iter().cloned())
        .map(|warning| bound_warning(&warning))
        .collect();
        if all.len() <= MAX_WARNINGS {
            return all;
        }
        // A capped list rendered as the whole list is the same lie as a
        // shortened warning that does not admit it — "6 warnings" and "the
        // first 31 of 400" must not look alike. The cap keeps its own slot so
        // the count it reports is the count that was dropped.
        let dropped = all.len() - (MAX_WARNINGS - 1);
        all.truncate(MAX_WARNINGS - 1);
        all.push(format!(
            "[GitPulse is showing {} of {} warnings; {dropped} more were not rendered]",
            MAX_WARNINGS - 1,
            dropped + MAX_WARNINGS - 1
        ));
        all
    }
}

/// `devmap status --json`.
pub fn status(repo_path: &str) -> CliStatus {
    // Sited ahead of the repo and binary checks on purpose: a test that installs
    // a canned answer is saying "this is what the probe replied", and running
    // any part of the probe anyway would put the child's start-up latency back
    // on the path the override exists to take it off.
    #[cfg(test)]
    {
        // Read into a local so the lock is released before the answer is
        // delayed: held across the wait, it serialized every concurrent probe.
        let canned = TEST_STATUS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(canned) = canned {
            TEST_STATUS_CALLS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            #[cfg(unix)]
            wait_test_status_delay();
            return canned;
        }
    }
    let repo = match validate_repo(repo_path) {
        Ok(repo) => repo,
        Err(e) => {
            return CliStatus {
                available: false,
                binary: None,
                lookup: None,
                reason: Some(e.into_message()),
                status: None,
                deferral: None,
            }
        }
    };
    let binary = match resolve_binary() {
        Ok(b) => b,
        Err(e) => {
            return CliStatus {
                available: false,
                binary: None,
                lookup: None,
                reason: Some(e),
                status: None,
                deferral: None,
            }
        }
    };
    let run = match run_devmap(&binary, &repo, &["status", "--json"], None, STATUS_DEADLINE) {
        Ok(run) => run,
        Err(e) => {
            let reason = e.into_message();
            let deferral = process::is_deferred_under_load(&reason).then(|| deferral_for(&repo));
            return CliStatus {
                available: false,
                binary: Some(binary.path),
                lookup: Some(binary.lookup),
                reason: Some(reason),
                status: None,
                deferral,
            };
        }
    };
    let stdout = bytes_to_string(run.stdout);
    let stderr = bytes_to_string(run.stderr);
    if !run.success {
        let detail = failure_detail(
            &stdout,
            &stderr,
            format!("devmap status exited {}", run.status_code),
        );
        return CliStatus {
            available: false,
            binary: Some(binary.path),
            lookup: Some(binary.lookup),
            reason: Some(detail),
            status: parse_json_stdout(&stdout),
            deferral: None,
        };
    }
    match parse_json_stdout(&stdout) {
        Some(status) => {
            remember_good_status(&repo, &status);
            CliStatus {
                available: true,
                binary: Some(binary.path),
                lookup: Some(binary.lookup),
                reason: None,
                status: Some(status),
                deferral: None,
            }
        }
        None => CliStatus {
            available: false,
            binary: Some(binary.path),
            lookup: Some(binary.lookup),
            reason: Some("devmap status returned non-JSON stdout".into()),
            status: None,
            deferral: None,
        },
    }
}

/// `devmap preview --file <path> --content - --json` with the buffer on stdin.
pub fn preview(repo_path: &str, file_path: &str, content: &str) -> PreviewFileResult {
    if file_path.trim().is_empty() {
        return PreviewFileResult {
            file_path: file_path.to_string(),
            available: false,
            reason: Some("file_path must not be blank".into()),
            report: None,
        };
    }
    if !preview_path_eligible(file_path) {
        return skipped_preview_file(file_path, PREVIEW_SKIP_NON_SOURCE);
    }
    let repo = match validate_repo(repo_path) {
        Ok(repo) => repo,
        Err(e) => {
            return PreviewFileResult {
                file_path: file_path.to_string(),
                available: false,
                reason: Some(e.into_message()),
                report: None,
            }
        }
    };
    let repo_key = repo.to_string_lossy().into_owned();
    // Refuse while a build holds the writer lock — preview reads the store.
    {
        let guards = build_guards()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if guards.contains(repo_key.as_str()) {
            return PreviewFileResult {
                file_path: file_path.to_string(),
                available: false,
                reason: Some(format!(
                    "a devmap build is running for {repo_key}; preview refused until it finishes"
                )),
                report: None,
            };
        }
    }
    let binary = match resolve_binary() {
        Ok(b) => b,
        Err(e) => {
            return PreviewFileResult {
                file_path: file_path.to_string(),
                available: false,
                reason: Some(e),
                report: None,
            }
        }
    };
    // Claim the debounce slot before spawn so concurrent callers for the same
    // file see SkipDebounced rather than racing a second child. Content
    // changes inside the window coalesce too: a fixture rewrite storm must not
    // spawn one child per FS event.
    let admitted = {
        let mut map = preview_debounces()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        admit_preview_slot(
            &mut map,
            (repo_key.clone(), file_path.to_string()),
            Instant::now(),
        )
    };
    if !admitted {
        return skipped_preview_file(file_path, PREVIEW_SKIP_DEBOUNCED);
    }
    let run = match run_devmap(
        &binary,
        &repo,
        &["preview", "--file", file_path, "--content", "-", "--json"],
        Some(content.as_bytes()),
        PREVIEW_DEADLINE,
    ) {
        Ok(run) => run,
        Err(e) => {
            return PreviewFileResult {
                file_path: file_path.to_string(),
                available: false,
                reason: Some(e.into_message()),
                report: None,
            }
        }
    };
    let stdout = bytes_to_string(run.stdout);
    let stderr = bytes_to_string(run.stderr);
    if !run.success {
        let detail = failure_detail(
            &stdout,
            &stderr,
            format!("devmap preview exited {}", run.status_code),
        );
        return PreviewFileResult {
            file_path: file_path.to_string(),
            available: false,
            reason: Some(detail),
            report: parse_json_stdout(&stdout),
        };
    }
    match parse_json_stdout(&stdout) {
        Some(report) => PreviewFileResult {
            file_path: file_path.to_string(),
            available: true,
            reason: None,
            report: Some(report),
        },
        None => PreviewFileResult {
            file_path: file_path.to_string(),
            available: false,
            reason: Some("devmap preview returned non-JSON stdout".into()),
            report: None,
        },
    }
}

/// Preview many changed files. `cancel` is polled between files.
///
/// Non-source paths (fixtures, JSON/prose/data, ignored trees) are filtered
/// *before* the fan-out cap so a staged fixture storm cannot consume the
/// [`MAX_PREVIEW_FILES`] budget and starve real source. Walks at most
/// [`MAX_PREVIEW_FILES`] eligible files; omitted and ineligible files are
/// unavailable, not silent.
pub fn preview_many(
    repo_path: &str,
    files: &[(String, String)],
    mut cancel: impl FnMut() -> bool,
) -> PreviewOutcome {
    let files_total = files.len();
    let binary = match resolve_binary() {
        Ok(b) => b,
        Err(e) => {
            return PreviewOutcome {
                available: false,
                binary: None,
                lookup: None,
                reason: Some(e),
                files: Vec::new(),
                cancelled: false,
                truncated: false,
                files_total,
                files_omitted: 0,
            }
        }
    };
    let mut ineligible = Vec::new();
    let mut eligible = Vec::new();
    for (path, content) in files {
        if preview_path_eligible(path) {
            eligible.push((path.clone(), content.clone()));
        } else {
            ineligible.push(skipped_preview_file(path, PREVIEW_SKIP_NON_SOURCE));
        }
    }
    let cap = MAX_PREVIEW_FILES.min(eligible.len());
    let (walk, omitted) = eligible.split_at(cap);
    let files_omitted = omitted.len() + ineligible.len();
    let truncated = !omitted.is_empty();
    let mut out = Vec::with_capacity(files.len());
    for (path, content) in walk {
        if cancel() {
            out.extend(omitted.iter().map(|(path, _)| omitted_preview_file(path)));
            out.extend(ineligible);
            return PreviewOutcome {
                available: true,
                binary: Some(binary.path),
                lookup: Some(binary.lookup),
                reason: Some("preview cancelled before all files finished".into()),
                files: out,
                cancelled: true,
                truncated,
                files_total,
                files_omitted,
            };
        }
        out.push(preview(repo_path, path, content));
    }
    out.extend(omitted.iter().map(|(path, _)| omitted_preview_file(path)));
    out.extend(ineligible);
    let skipped_non_source = files_omitted.saturating_sub(omitted.len());
    PreviewOutcome {
        available: true,
        binary: Some(binary.path),
        lookup: Some(binary.lookup),
        reason: if truncated {
            Some(format!(
                "preview fan-out capped at {MAX_PREVIEW_FILES} files; {files_omitted} file(s) not previewed"
            ))
        } else if skipped_non_source > 0 && walk.is_empty() {
            Some(format!(
                "{skipped_non_source} staged path(s) skipped as non-source (testdata/fixtures/prose/data)"
            ))
        } else {
            None
        },
        files: out,
        cancelled: false,
        truncated,
        files_total,
        files_omitted,
    }
}

fn omitted_preview_file(path: &str) -> PreviewFileResult {
    PreviewFileResult {
        file_path: path.to_string(),
        available: false,
        reason: Some(PREVIEW_FANOUT_OMITTED_REASON.into()),
        report: None,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_json_refusal_is_read_from_the_envelope_not_from_stderr() {
        let envelope = r#"{"error":"refusing to rewrite /w/.mcp.json: not a JSON object"}"#;
        // Under --json DevMap prints the envelope on stdout and nothing on stderr.
        assert_eq!(
            failure_detail(envelope, "", "devmap integrate exited 1".into()),
            "refusing to rewrite /w/.mcp.json: not a JSON object"
        );
        // The envelope is DevMap's own text; a progress line on stderr does not replace it.
        assert_eq!(
            failure_detail(envelope, "indexing...\n", "x".into()),
            "refusing to rewrite /w/.mcp.json: not a JSON object"
        );
    }

    #[test]
    fn a_failure_without_an_envelope_keeps_stderr_then_the_exit_fallback() {
        assert_eq!(failure_detail("", " boom \n", "exited 2".into()), "boom");
        for stdout in [
            "",
            "not json",
            r#"{"error":""}"#,
            r#"{"error":7}"#,
            r#"{"files":3}"#,
            "[]",
        ] {
            assert_eq!(
                failure_detail(stdout, "  ", "exited 2".into()),
                "exited 2",
                "{stdout}"
            );
        }
    }

    use super::*;
    use crate::procguard::LockedSpawn;

    /// A warning devmap emits and GitPulse has never heard of must still be
    /// shown.
    ///
    /// This is the shape of the `stray_state_warning` miss: devmap grew a
    /// sixth warning field, the parse here named five, and the sixth fired on
    /// the user's machine into a panel that then rendered "devmap doctor found
    /// nothing to report". The list is swept now, so the next field devmap
    /// adds arrives on the day it ships rather than the day someone
    /// remembers.
    #[test]
    fn an_unrecognised_warning_field_is_carried_not_dropped() {
        let payload = serde_json::json!({
            "stray_state_warning": "state directory exists without a store: ~/.devmap",
            "quarantined_grammar_warning": "3 grammars failed to link",
            "version": "0.2.2",
        });
        let report = DoctorReport::from_payload(&payload, "/usr/local/bin/devmap".into());
        let warnings = report.warnings();
        assert!(
            warnings.iter().any(|w| w.contains("~/.devmap")),
            "the field that was actually dropped is still dropped: {warnings:?}"
        );
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("quarantined_grammar_warning")
                    && w.contains("3 grammars failed to link")),
            "an unknown warning field was dropped, and its key was not named: {warnings:?}"
        );
        // Unknown severity ranks last, but never absent.
        assert_eq!(
            warnings.last().map(String::as_str),
            Some("quarantined_grammar_warning: 3 grammars failed to link")
        );
    }

    /// Nothing that is not a warning may be swept up as one.
    #[test]
    fn the_sweep_takes_warnings_and_only_warnings() {
        let payload = serde_json::json!({
            "store_path": "/repo/.devcouncil/codeintel/devmap.sqlite",
            "binaries": [{"path": "/usr/local/bin/devmap"}],
            "warning": "not a *_warning key",
            "_warning": "",
            "blank_warning": "   ",
            "numeric_warning": 7,
            "null_warning": serde_json::Value::Null,
            "nested_warning": {"text": "objects are not messages"},
        });
        let report = DoctorReport::from_payload(&payload, "/usr/local/bin/devmap".into());
        assert!(
            report.warnings().is_empty(),
            "non-warning or empty fields were rendered as warnings: {:?}",
            report.warnings()
        );
    }

    /// The named list and the struct are one decision; drift between them
    /// silently re-opens the `extra_warnings` escape hatch on a field that is
    /// supposed to be ordered by consequence, or hides a real field twice.
    ///
    /// Derived from this file's own source rather than restated, because a
    /// second hand-written list is the defect, not the fix.
    #[test]
    fn every_named_warning_field_is_claimed() {
        let source = include_str!("cli.rs");
        let body = source
            .split_once("pub struct DoctorReport {")
            .expect("struct present")
            .1
            .split_once("\n}")
            .expect("struct closes")
            .0;
        let declared: Vec<&str> = body
            .lines()
            .filter_map(|line| line.trim().strip_prefix("pub "))
            .filter_map(|line| line.split_once(':'))
            .map(|(name, _)| name)
            .filter(|name| name.ends_with("_warning"))
            .collect();
        assert!(
            !declared.is_empty(),
            "parsed no fields — the test is broken"
        );
        let mut sorted_declared = declared.clone();
        sorted_declared.sort_unstable();
        let mut sorted_named = NAMED_WARNING_FIELDS.to_vec();
        sorted_named.sort_unstable();
        assert_eq!(
            sorted_declared, sorted_named,
            "NAMED_WARNING_FIELDS drifted from the struct fields"
        );
    }

    /// A shortened warning that does not admit it is a capped sample rendered
    /// as complete coverage — the reader cannot tell a full list of stale
    /// processes from the first 2 KiB of one.
    ///
    /// Swept across every length near the budget rather than spot-checked:
    /// these strings carry `—` and `…`, and a raw byte slice panics whenever
    /// the cut lands inside one.
    #[test]
    fn a_shortened_warning_says_so_with_both_numbers() {
        let short = "pid 7564, 7570; restart hosts";
        assert_eq!(
            bound_warning(short),
            short,
            "a warning that fits was altered"
        );

        for pad in 0..64 {
            // `…` is three bytes, so this walks the cut through every phase of
            // a multi-byte character straddling the budget.
            let long = format!("{}{}", "…".repeat(MAX_WARNING_BYTES), "x".repeat(pad));
            let bounded = bound_warning(&long);
            assert!(
                bounded.contains("GitPulse shortened this warning"),
                "shortened silently at pad {pad}"
            );
            assert!(
                bounded.contains(&long.len().to_string()),
                "the original size is missing at pad {pad}: {bounded}"
            );
            // Char-boundary safety is proven by this not having panicked, and
            // by the result still being valid UTF-8 text we can measure.
            assert!(bounded.chars().count() > 0);
        }
    }

    /// The warning list itself is bounded: it crosses IPC and becomes DOM, and
    /// "the tool said so" is not a size limit.
    #[test]
    fn the_warning_list_is_bounded() {
        let mut payload = serde_json::Map::new();
        for i in 0..(MAX_WARNINGS * 4) {
            payload.insert(format!("k{i:03}_warning"), serde_json::json!("noise"));
        }
        let report = DoctorReport::from_payload(
            &serde_json::Value::Object(payload),
            "/usr/local/bin/devmap".into(),
        );
        let warnings = report.warnings();
        assert_eq!(warnings.len(), MAX_WARNINGS);
        // And the cap says so, with both numbers: a capped list that reads
        // like a complete one is the defect, not the cap.
        let last = warnings.last().expect("capped list is not empty");
        assert!(
            last.contains(&(MAX_WARNINGS - 1).to_string())
                && last.contains(&(MAX_WARNINGS * 4).to_string()),
            "the cap did not report what it dropped: {last}"
        );
    }

    /// The reported fault this whole change exists for, end to end.
    ///
    /// GitPulse resolves `devmap` through `PATH` *plus* the GUI-launch
    /// fallback dirs, so a Dock-launched app finds `~/.local/bin/devmap`. It
    /// then used to spawn it with launchd's own `/usr/bin:/bin:/usr/sbin:/sbin`
    /// — a PATH with no `~/.local/bin` in it. `devmap doctor` resolves the bare
    /// `devmap` command that host MCP configs name against that PATH, found
    /// nothing, and reported:
    ///
    /// > host MCP config names a devmap path that is not a file: devmap;
    /// > install the binary or re-run integrate … this is not version skew
    ///
    /// which GitPulse rendered under "Installation health" as a fault on the
    /// user's machine. Nothing was wrong with the install. The check could not
    /// run, and named a cause it did not have.
    ///
    /// PATH is pinned to the launchd-minimal value for the spawn so the
    /// assertion holds on any host: a developer shell that already carries
    /// every fallback dir would otherwise let the pre-fix behaviour pass here
    /// and fail in CI, or on the machine that reported this.
    #[test]
    #[cfg(unix)]
    fn devmap_is_spawned_with_a_path_that_can_see_devmap() {
        const LAUNCHD_MINIMAL: &str = "/usr/bin:/bin:/usr/sbin:/sbin";
        let _serial = crate::harness::sidecar::test_serial();
        let dir = tempfile::TempDir::new().unwrap();
        let recorded = dir.path().join("child-path");
        let path = write_fake_devmap(dir.path());
        fs::write(
            &path,
            format!(
                "#!/bin/sh\nprintf '%s' \"$PATH\" > '{}'\nprintf '{{}}'\n",
                recorded.display()
            ),
        )
        .unwrap();
        let binary = ResolvedDevmap {
            path: path.to_string_lossy().into_owned(),
            lookup: DevmapLookup::PathSearch,
        };

        let run = {
            let _env = crate::test_support::env::bind_env(&_serial).set("PATH", LAUNCHD_MINIMAL);
            run_devmap(
                &binary,
                dir.path(),
                &["status", "--json"],
                None,
                Duration::from_secs(5),
            )
        };
        run.expect("stub devmap must run");

        let child_path = fs::read_to_string(&recorded).expect("child did not record its PATH");
        let entries: Vec<PathBuf> = std::env::split_paths(&child_path).collect();
        assert_ne!(
            child_path, LAUNCHD_MINIMAL,
            "the child inherited the launch PATH unchanged"
        );
        for fallback in crate::engine::process::external_tool_fallback_dirs() {
            assert!(
                entries.contains(&fallback),
                "devmap cannot see {} — the directory it may have been resolved from: {entries:?}",
                fallback.display()
            );
        }
        // The launch PATH still comes first: a fallback dir must never shadow
        // a tool the user deliberately put earlier on their PATH.
        assert_eq!(entries[0], PathBuf::from("/usr/bin"));
    }

    #[test]
    #[cfg(unix)]
    fn devmap_failures_retain_diagnostic_context_without_preview_source() {
        crate::logging::init();
        let dir = tempfile::TempDir::new().unwrap();
        let path = write_fake_devmap(dir.path());
        fs::write(&path, "#!/bin/sh\necho 'diagnostic-probe: opening index' >&2\nprintf '{\"error\":\"broken store\"}'\nexit 7\n").unwrap();
        let binary = ResolvedDevmap {
            path: path.to_string_lossy().into_owned(),
            lookup: DevmapLookup::PathSearch,
        };
        let run = run_devmap(
            &binary,
            dir.path(),
            &["preview", "--json"],
            Some(b"PRIVATE_PREVIEW_SOURCE"),
            Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(run.status_code, 7);
        let logs = crate::logging::diagnostic_tail(500).join("\n");
        assert!(
            logs.contains("diagnostic-probe: opening index"),
            "DevMap stderr was discarded"
        );
        assert!(logs.contains("broken store"));
        assert!(logs.contains("elapsed_ms"));
        assert!(logs.contains("exit_code"));
        assert!(!logs.contains("PRIVATE_PREVIEW_SOURCE"));
    }

    #[test]
    #[cfg(unix)]
    fn devmap_logs_redact_multiline_credentials_before_persistence() {
        crate::logging::init();
        let dir = tempfile::TempDir::new().unwrap();
        let path = write_fake_devmap(dir.path());
        fs::write(&path, "#!/bin/sh\nprintf '%s\\n' '-----BEGIN PRIVATE KEY-----' 'DEVMAP_PRIVATE_KEY_MATERIAL' '-----END PRIVATE KEY-----' >&2\necho '{}'\n").unwrap();
        let binary = ResolvedDevmap {
            path: path.to_string_lossy().into_owned(),
            lookup: DevmapLookup::PathSearch,
        };
        run_devmap(
            &binary,
            dir.path(),
            &["status", "--json"],
            None,
            Duration::from_secs(5),
        )
        .unwrap();
        let logs = crate::logging::diagnostic_tail(500).join("\n");
        assert!(
            !logs.contains("DEVMAP_PRIVATE_KEY_MATERIAL"),
            "multiline secret leaked through a progress record"
        );
    }

    #[test]
    #[cfg(unix)]
    fn devmap_rejects_successful_non_json_payloads() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = write_fake_devmap(dir.path());
        let binary = ResolvedDevmap {
            path: path.to_string_lossy().into_owned(),
            lookup: DevmapLookup::PathSearch,
        };
        for payload in ["not JSON", "null", "{\"error\":\"store unavailable\"}"] {
            fs::write(&path, format!("#!/bin/sh\nprintf '%s' '{payload}'\n")).unwrap();
            let result = run_devmap(
                &binary,
                dir.path(),
                &["build", "--json"],
                None,
                Duration::from_secs(5),
            );
            assert!(result.is_err(), "invalid result was accepted: {payload}");
        }
    }

    #[test]
    #[cfg(unix)]
    fn audit_devmap_refuses_valid_json_with_unfinished_output() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = write_fake_devmap(dir.path());
        fs::write(&path, "#!/bin/sh\nprintf '{\"ok\":true}'\nsleep 3 &\n").unwrap();
        let binary = ResolvedDevmap {
            path: path.to_string_lossy().into_owned(),
            lookup: DevmapLookup::PathSearch,
        };
        let result = run_devmap(
            &binary,
            dir.path(),
            &["status", "--json"],
            None,
            Duration::from_secs(5),
        );
        assert!(result.is_err(), "incomplete JSON was accepted: {result:?}");
    }
    use std::fs;

    fn git_repo() -> tempfile::TempDir {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let output = std::process::Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(dir.path())
            .output_locked()
            .expect("spawn git init");
        assert!(output.status.success());
        crate::test_support::trust_repo(dir.path());
        dir
    }

    /// A host-native fake `devmap`. A `#!/bin/sh` script is not a Win32
    /// application (os error 193); Windows gets a `.cmd` that answers the same
    /// preview JSON without reading stdin.
    fn write_fake_devmap(dir: &Path) -> PathBuf {
        #[cfg(unix)]
        {
            let path = dir.join("devmap");
            let script = r#"#!/bin/sh
if [ "$1" = "preview" ]; then
  cat >/dev/null
  printf '%s\n' '{"file_path":"src/lib.rs","parse_status":"Clean","delta_available":true,"file_is_indexed":true,"compared_against":"disk","symbols":[],"bodies_not_compared":0,"ambiguous_callers":0,"broken_callers":{"items":[],"shown":0,"hidden":0,"total":0,"truncated":false,"tokens_used":0,"resolution":{"Available":null}}}'
  exit 0
fi
echo "unexpected: $*" >&2
exit 2
"#;
            fs::write(&path, script).expect("write fake");
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&path).expect("meta").permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&path, perms).expect("chmod");
            path
        }
        #[cfg(windows)]
        {
            let path = dir.join("devmap.cmd");
            let script = r#"@echo off
if /I "%~1"=="preview" (
  echo {"file_path":"src/lib.rs","parse_status":"Clean","delta_available":true,"file_is_indexed":true,"compared_against":"disk","symbols":[],"bodies_not_compared":0,"ambiguous_callers":0,"broken_callers":{"items":[],"shown":0,"hidden":0,"total":0,"truncated":false,"tokens_used":0,"resolution":{"Available":null}}}
  exit /b 0
)
echo unexpected: %* 1>&2
exit /b 2
"#;
            fs::write(&path, script).expect("write fake");
            path
        }
    }

    #[cfg(unix)]
    fn write_recording_devmap(dir: &Path) -> PathBuf {
        let path = dir.join("devmap");
        fs::write(
            &path,
            r#"#!/bin/sh
printf '%s\n' "$*" >> argv.log
if [ "$1" = "status" ]; then
  if [ -f status.json ]; then cat status.json; else printf '%s\n' '{"is_fresh":false,"schema_outdated":false}'; fi
  exit 0
fi
if [ "$1" = "build" ]; then
  printf '%s\n' '{"ok":true}'
  exit 0
fi
echo unexpected: "$*" >&2
exit 2
"#,
        )
        .expect("write recording devmap");
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&path).expect("meta").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&path, perms).expect("chmod");
        path
    }

    /// Bind the argv-recording stub for the rest of this test.
    ///
    /// Returns the binding itself rather than a local guard: `TestBinaryBinding`
    /// already owns both the override and the serial, and wrapping it in a
    /// second guard that also took the serial was a deadlock waiting to be
    /// found — the helper acquired it, then the binding acquired it again.
    #[cfg(unix)]
    fn bind_recording_devmap(repo: &Path) -> TestBinaryBinding {
        let bin = write_recording_devmap(repo);
        bind_test_binary(bin.to_string_lossy().into_owned())
    }

    /// Give a fixture the consumer artifacts a real `--manifest` build writes.
    ///
    /// The recording stub answers `build` without touching the filesystem, so
    /// without this a fixture looks like a repository whose map was never
    /// written — which is its own reason to take the `--manifest` path, and
    /// would quietly change what an "incremental" assertion is measuring.
    #[cfg(unix)]
    fn write_stub_artifacts(root: &Path) {
        for path in [
            super::super::repo_map::repo_map_path(root),
            super::super::viz::code_graph_path(root),
        ] {
            fs::create_dir_all(path.parent().expect("artifact parent")).expect("artifact dir");
            fs::write(&path, "{}").expect("artifact");
        }
    }

    #[cfg(unix)]
    fn argv_log(repo: &Path) -> String {
        fs::read_to_string(repo.join("argv.log")).unwrap_or_default()
    }

    #[cfg(unix)]
    fn assert_build_argv(log: &str, expect_manifest: bool) {
        let builds: Vec<&str> = log
            .lines()
            .filter(|line| line.split_whitespace().next() == Some("build"))
            .collect();
        assert!(!builds.is_empty(), "no build spawned, argv log:\n{log}");
        for line in &builds {
            let tokens: Vec<&str> = line.split_whitespace().collect();
            assert!(
                tokens.contains(&"--json"),
                "build must request JSON, argv log:\n{log}"
            );
            let manifest = tokens.contains(&"--manifest");
            assert_eq!(
                manifest, expect_manifest,
                "expected manifest={expect_manifest}, argv log:\n{log}"
            );
        }
    }

    #[test]
    fn explicit_env_that_does_not_resolve_is_refused() {
        let _lock = crate::harness::sidecar::test_serial();
        // Two different things are process-global here and each has its own
        // serial: the sidecar's guards the environment, this one guards the
        // binary override. This test asserts on *resolution order*, and the
        // override is consulted before `GITPULSE_DEVMAP_BIN` — so a binding
        // held by any concurrent test would satisfy the lookup and the refusal
        // under test would never happen. Acquired after the sidecar's, which is
        // the order every binding site uses; reversing it anywhere would be a
        // lock-order inversion.
        let _no_override = crate::test_support::env::env_serial();
        let _env = crate::test_support::env::bind_env(&_lock)
            .set("GITPULSE_DEVMAP_BIN", "/no/such/devmap-binary");
        let err = resolve_binary().expect_err("must refuse");
        assert!(err.contains("GITPULSE_DEVMAP_BIN"), "{err}");
        assert!(err.contains("not a file"), "{err}");
    }

    #[test]
    fn preview_many_walks_at_most_the_fanout_cap() {
        let _lock = crate::harness::sidecar::test_serial();
        clear_preview_debounces();
        let repo = git_repo();
        let bin_dir = tempfile::TempDir::new().expect("bindir");
        let bin = write_fake_devmap(bin_dir.path());
        let _bound = bind_test_binary(bin.to_string_lossy().into_owned());
        let files: Vec<(String, String)> = (0..20)
            .map(|i| (format!("src/f{i}.rs"), "fn x() {}\n".into()))
            .collect();
        let out = preview_many(&repo.path().to_string_lossy(), &files, || false);
        clear_preview_debounces();
        let walked = out.files.iter().filter(|file| file.available).count();
        assert_eq!(walked, 16, "preview_many walked {walked} available files");
        assert_eq!(out.files.len(), 20);
        assert!(out.truncated);
        assert_eq!(out.files_omitted, 4);
        assert_eq!(out.files_total, 20);
    }

    #[test]
    fn preview_surfaces_json_report_from_cli() {
        let _lock = crate::harness::sidecar::test_serial();
        clear_preview_debounces();
        let repo = git_repo();
        let bin_dir = tempfile::TempDir::new().expect("bindir");
        let bin = write_fake_devmap(bin_dir.path());
        let _bound = bind_test_binary(bin.to_string_lossy().into_owned());
        let result = preview(
            &repo.path().to_string_lossy(),
            "src/lib.rs",
            "fn main() {}\n",
        );
        clear_preview_debounces();
        assert!(result.available, "{:?}", result.reason);
        let report = result.report.expect("report");
        assert_eq!(report["parse_status"], "Clean");
        assert_eq!(report["compared_against"], "disk");
        assert_eq!(report["ambiguous_callers"], 0);
    }

    #[test]
    fn preview_path_eligible_skips_fixtures_json_and_prose() {
        assert!(preview_path_eligible("src/main.rs"));
        assert!(preview_path_eligible("pkg/lib.go"));
        assert!(
            !preview_path_eligible("rust/testdata/golden/languages/rust/nodes.json"),
            "testdata JSON must never spawn preview"
        );
        assert!(!preview_path_eligible("fixtures/sample.rs"));
        assert!(!preview_path_eligible("docs/guide.md"));
        assert!(!preview_path_eligible("config/settings.json"));
        assert!(!preview_path_eligible("README.md"));
        assert!(!preview_path_eligible("notes.yaml"));
    }

    #[test]
    fn preview_many_filters_non_source_before_fanout_cap() {
        let _lock = crate::harness::sidecar::test_serial();
        clear_preview_debounces();
        let repo = git_repo();
        let bin_dir = tempfile::TempDir::new().expect("bindir");
        let bin = write_fake_devmap(bin_dir.path());
        let _bound = bind_test_binary(bin.to_string_lossy().into_owned());
        // Twenty fixture/json paths plus one real source — without the
        // pre-cap filter the fan-out budget would be spent on fixtures and
        // `src/lib.rs` would never be previewed.
        let mut files: Vec<(String, String)> = (0..20)
            .map(|i| {
                (
                    format!("rust/testdata/golden/languages/lang{i}/nodes.json"),
                    "{}\n".into(),
                )
            })
            .collect();
        files.push(("src/lib.rs".into(), "fn main() {}\n".into()));
        let out = preview_many(&repo.path().to_string_lossy(), &files, || false);
        clear_preview_debounces();
        let available: Vec<_> = out
            .files
            .iter()
            .filter(|file| file.available)
            .map(|file| file.file_path.as_str())
            .collect();
        assert_eq!(available, ["src/lib.rs"], "{available:?}");
        assert_eq!(out.files_total, 21);
        assert!(
            out.files.iter().filter(|file| !file.available).all(|file| {
                file.reason.as_deref().is_some_and(|reason| {
                    reason.contains("non-source") || reason.contains("testdata")
                })
            }),
            "ineligible paths must name the skip reason"
        );
    }

    #[test]
    #[cfg(unix)]
    fn preview_debounce_bounds_bursts_for_one_file() {
        let _lock = crate::harness::sidecar::test_serial();
        clear_preview_debounces();
        let repo = git_repo();
        let bin = repo.path().join("devmap");
        fs::write(
            &bin,
            r#"#!/bin/sh
printf x >> preview-calls
if [ "$1" = "preview" ]; then
  cat >/dev/null
  printf '%s\n' '{"file_path":"src/lib.rs","parse_status":"Clean","delta_available":true,"file_is_indexed":true,"compared_against":"disk","symbols":[],"bodies_not_compared":0,"ambiguous_callers":0,"broken_callers":{"items":[],"shown":0,"hidden":0,"total":0,"truncated":false,"tokens_used":0,"resolution":{"Available":null}}}'
  exit 0
fi
exit 2
"#,
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&bin).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&bin, perms).unwrap();
        let _bound = bind_test_binary(bin.to_string_lossy().into_owned());
        let _reset = std::mem::DropGuard::new((), |()| {
            clear_preview_debounces();
        });
        let path = repo.path().to_string_lossy().into_owned();
        let mut available = 0u32;
        let mut debounced = 0u32;
        for i in 0..40 {
            let result = preview(&path, "src/lib.rs", &format!("fn main() {{ /* {i} */ }}\n"));
            if result.available {
                available += 1;
            } else if result
                .reason
                .as_deref()
                .is_some_and(|reason| reason.contains("debounced"))
            {
                debounced += 1;
            } else {
                panic!("unexpected preview outcome: {result:?}");
            }
        }
        let calls = fs::read_to_string(repo.path().join("preview-calls")).unwrap_or_default();
        assert_eq!(
            calls.len(),
            available as usize,
            "CLI spawn count must match available results"
        );
        assert!(
            available <= 4,
            "burst must be bounded by debounce, got {available} available / {debounced} debounced"
        );
        assert!(
            debounced >= 30,
            "most of the burst should hit debounce, got {debounced}"
        );
    }

    #[test]
    fn racing_callers_claim_one_debounce_slot_and_expired_slots_are_evicted() {
        let map = Mutex::new(HashMap::new());
        let key = ("/repo".to_string(), "src/lib.rs".to_string());
        let now = Instant::now();
        let admitted = std::sync::atomic::AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for _ in 0..32 {
                scope.spawn(|| {
                    let mut guard = map.lock().unwrap();
                    if admit_preview_slot(&mut guard, key.clone(), now) {
                        admitted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    }
                });
            }
        });
        assert_eq!(admitted.load(std::sync::atomic::Ordering::SeqCst), 1);

        let mut map = map.into_inner().unwrap();
        for i in 0..100 {
            admit_preview_slot(&mut map, ("/repo".into(), format!("f{i}.rs")), now);
        }
        assert_eq!(map.len(), 101);
        let later = now + PREVIEW_DEBOUNCE + Duration::from_millis(1);
        assert!(admit_preview_slot(&mut map, key, later));
        assert_eq!(map.len(), 1, "expired slots must be evicted");
    }

    #[test]
    fn second_build_for_same_repo_is_refused() {
        let _lock = crate::harness::sidecar::test_serial();
        let repo = git_repo();
        let key = repo.path().to_path_buf();
        let _held = BuildGuard::try_acquire(&key).expect("first");
        let err = BuildGuard::try_acquire(&key).expect_err("second");
        assert!(err.contains("already running"), "{err}");
    }

    #[test]
    #[cfg(unix)]
    fn busy_live_refresh_does_not_spawn_status_during_a_build() {
        let _lock = crate::harness::sidecar::test_serial();
        let repo = git_repo();
        let canonical = repo.path().canonicalize().unwrap();
        let bin = write_fake_devmap(repo.path());
        fs::write(&bin, "#!/bin/sh\necho called >> status-calls\necho '{\"is_fresh\":false,\"schema_outdated\":false}'\n").unwrap();
        let _bound = bind_test_binary(bin.to_string_lossy().into_owned());
        let _guard = BuildGuard::try_acquire(&canonical).unwrap();
        for _ in 0..32 {
            let outcome = crate::devmap::maybe_refresh(canonical.to_str().unwrap(), true);
            assert_eq!(
                outcome.decision,
                crate::devmap::LiveRefreshDecision::SkipBuilding
            );
            assert!(outcome.build.is_none());
        }
        assert!(
            !repo.path().join("status-calls").exists(),
            "busy retries spawned status children"
        );
    }

    #[test]
    #[cfg(unix)]
    fn live_refresh_obsolete_payload_uses_manifest_rebuild() {
        let _lock = crate::harness::sidecar::test_serial();
        let repo = git_repo();
        let canonical = repo.path().canonicalize().unwrap();
        let devmap = bind_recording_devmap(repo.path());
        devmap.set_status(available_status(
            r#"{"is_fresh":false,"schema_outdated":false,"degraded_reason":"stored extraction payload is obsolete; rebuild with the current analyzer"}"#,
        ));
        let outcome = crate::devmap::maybe_refresh(canonical.to_str().unwrap(), true);
        assert_eq!(
            outcome.decision,
            crate::devmap::LiveRefreshDecision::Refresh
        );
        assert!(outcome.build.as_ref().is_some_and(|b| b.ok), "{outcome:?}");
        assert_build_argv(&argv_log(repo.path()), true);
    }

    #[test]
    #[cfg(unix)]
    fn live_refresh_obsolete_rebuild_reason_uses_manifest_rebuild() {
        let _lock = crate::harness::sidecar::test_serial();
        let repo = git_repo();
        let canonical = repo.path().canonicalize().unwrap();
        let devmap = bind_recording_devmap(repo.path());
        devmap.set_status(available_status(
            r#"{"is_fresh":false,"schema_outdated":false,"rebuild_reason":"payload-obsolete"}"#,
        ));
        let outcome = crate::devmap::maybe_refresh(canonical.to_str().unwrap(), false);
        assert_eq!(
            outcome.decision,
            crate::devmap::LiveRefreshDecision::Refresh
        );
        assert!(outcome.build.as_ref().is_some_and(|b| b.ok), "{outcome:?}");
        assert_build_argv(&argv_log(repo.path()), true);
    }

    #[test]
    #[cfg(unix)]
    fn live_refresh_obsolete_payload_rebuilds_even_when_status_claims_fresh() {
        let _lock = crate::harness::sidecar::test_serial();
        let repo = git_repo();
        let canonical = repo.path().canonicalize().unwrap();
        let devmap = bind_recording_devmap(repo.path());
        devmap.set_status(available_status(
            r#"{"is_fresh":true,"schema_outdated":false,"degraded_reason":"stored extraction payload is obsolete; rebuild with the current analyzer"}"#,
        ));
        let outcome = crate::devmap::maybe_refresh(canonical.to_str().unwrap(), false);
        assert_eq!(
            outcome.decision,
            crate::devmap::LiveRefreshDecision::Refresh
        );
        assert!(outcome.build.as_ref().is_some_and(|b| b.ok), "{outcome:?}");
        assert_build_argv(&argv_log(repo.path()), true);
    }

    #[test]
    #[cfg(unix)]
    fn live_refresh_source_tree_staleness_stays_incremental() {
        let _lock = crate::harness::sidecar::test_serial();
        let repo = git_repo();
        let canonical = repo.path().canonicalize().unwrap();
        write_stub_artifacts(repo.path());
        let devmap = bind_recording_devmap(repo.path());
        for reason in [
            "source tree differs from the indexed generation; rebuild or drain watcher edits",
            "source discovery refusals differ from the indexed generation; rebuild required",
            "analyzer freshness unverified: the binary was built without the parsing frontend",
            "source freshness unverified: this generation has no repository root",
        ] {
            let _ = fs::remove_file(repo.path().join("argv.log"));
            // Each reason is its own case. The rest the previous iteration's
            // build earned belongs to the governor's tests in live.rs.
            crate::devmap::clear_live_echo_cooldown(canonical.to_str().unwrap());
            devmap.set_status(available_status(&format!(
                r#"{{"is_fresh":false,"schema_outdated":false,"degraded_reason":{reason:?}}}"#
            )));
            let outcome = crate::devmap::maybe_refresh(canonical.to_str().unwrap(), true);
            assert_eq!(
                outcome.decision,
                crate::devmap::LiveRefreshDecision::Refresh,
                "{reason}"
            );
            assert_build_argv(&argv_log(repo.path()), false);
        }
    }

    #[test]
    #[cfg(unix)]
    fn live_refresh_ordinary_staleness_stays_incremental() {
        let _lock = crate::harness::sidecar::test_serial();
        let repo = git_repo();
        let canonical = repo.path().canonicalize().unwrap();
        write_stub_artifacts(repo.path());
        let devmap = bind_recording_devmap(repo.path());
        for payload in [
            r#"{"is_fresh":false,"schema_outdated":false}"#,
            r#"{"is_fresh":false,"schema_outdated":false,"degraded_reason":null}"#,
            r#"{"is_fresh":false,"schema_outdated":false,"degraded_reason":""}"#,
            r#"{"is_fresh":false,"schema_outdated":false,"degraded_reason":"obsolete"}"#,
            r#"{"is_fresh":false,"schema_outdated":false,"degraded_reason":"payload is obsolete"}"#,
            r#"{"is_fresh":false,"schema_outdated":false,"message":"stored extraction payload is obsolete; rebuild with the current analyzer"}"#,
        ] {
            let _ = fs::remove_file(repo.path().join("argv.log"));
            // Each payload is its own case. The rest the previous iteration's
            // build earned belongs to the governor's tests in live.rs.
            crate::devmap::clear_live_echo_cooldown(canonical.to_str().unwrap());
            devmap.set_status(available_status(payload));
            let outcome = crate::devmap::maybe_refresh(canonical.to_str().unwrap(), true);
            assert_eq!(
                outcome.decision,
                crate::devmap::LiveRefreshDecision::Refresh,
                "{payload}"
            );
            assert_build_argv(&argv_log(repo.path()), false);
        }
    }

    /// A schema-behind store the kernel says it can migrate must be rebuilt,
    /// with `--manifest`, rather than refused forever. Before this, nothing in
    /// the app ever rebuilt such a store: the gate skipped and the skip was
    /// permanent.
    #[test]
    #[cfg(unix)]
    fn live_refresh_migrates_a_schema_the_kernel_says_it_can_rebuild() {
        let _lock = crate::harness::sidecar::test_serial();
        let repo = git_repo();
        let canonical = repo.path().canonicalize().unwrap();
        write_stub_artifacts(repo.path());
        let devmap = bind_recording_devmap(repo.path());
        devmap.set_status(available_status(
            r#"{"is_fresh":false,"schema_outdated":true,"rebuild_required":true,"rebuild_reason":"schema-behind","schema_relation":"upgradeable","degraded_reason":"store schema is 19, this binary speaks 20; run `devmap build` to migrate it"}"#,
        ));
        let outcome = crate::devmap::maybe_refresh(canonical.to_str().unwrap(), true);
        assert_eq!(
            outcome.decision,
            crate::devmap::LiveRefreshDecision::Refresh
        );
        assert_build_argv(&argv_log(repo.path()), true);
    }

    /// The other side of the same rule: a store the kernel does *not* say it
    /// can rebuild stays refused, and no build is spawned. `newer` is the case
    /// that matters — rebuilding it would downgrade a database a newer reader
    /// owns.
    #[test]
    #[cfg(unix)]
    fn live_refresh_never_rebuilds_a_newer_store() {
        let _lock = crate::harness::sidecar::test_serial();
        let repo = git_repo();
        let canonical = repo.path().canonicalize().unwrap();
        write_stub_artifacts(repo.path());
        let devmap = bind_recording_devmap(repo.path());
        devmap.set_status(available_status(
            r#"{"is_fresh":false,"schema_outdated":true,"rebuild_required":false,"schema_relation":"newer","degraded_reason":"store schema is 21, newer than the 20 this binary speaks; install a matching or newer devmap binary"}"#,
        ));
        let outcome = crate::devmap::maybe_refresh(canonical.to_str().unwrap(), true);
        assert_eq!(
            outcome.decision,
            crate::devmap::LiveRefreshDecision::SkipSchemaOutdated
        );
        let log = argv_log(repo.path());
        assert!(
            !log.lines().any(|line| line.starts_with("build ")),
            "a newer store must never be rebuilt:\n{log}"
        );
        // The refusal carries the CLI's own remedy rather than advice the user
        // cannot act on.
        let reason = outcome.reason.unwrap_or_default();
        assert!(
            reason.contains("install a matching or newer devmap binary"),
            "{reason}"
        );
    }

    #[test]
    #[cfg(unix)]
    fn live_refresh_schema_outdated_wins_over_obsolete_payload() {
        let _lock = crate::harness::sidecar::test_serial();
        let repo = git_repo();
        let canonical = repo.path().canonicalize().unwrap();
        let devmap = bind_recording_devmap(repo.path());
        devmap.set_status(available_status(
            r#"{"is_fresh":false,"schema_outdated":true,"degraded_reason":"stored extraction payload is obsolete; rebuild with the current analyzer"}"#,
        ));
        let outcome = crate::devmap::maybe_refresh(canonical.to_str().unwrap(), true);
        assert_eq!(
            outcome.decision,
            crate::devmap::LiveRefreshDecision::SkipSchemaOutdated
        );
        assert!(outcome.build.is_none(), "{outcome:?}");
        let log = argv_log(repo.path());
        assert!(
            !log.lines().any(|line| line.starts_with("build ")),
            "schema skip spawned a build:\n{log}"
        );
    }

    /// [`status`] refuses a path it cannot use, and says which, without spawning
    /// anything.
    ///
    /// The tests above hand `maybe_refresh` a canned answer, so they no longer
    /// reach these arms; the `run_devmap` tests cover the child and the parse.
    /// This covers the join: an unusable path is `available: false` carrying the
    /// reason, which is what [`crate::devmap::LiveRefreshDecision::SkipUnavailable`]
    /// goes on to report to the user.
    #[test]
    fn status_refuses_a_path_it_cannot_use_and_names_the_reason() {
        // This module's own serial, not the sidecar's: what has to be excluded
        // here is a concurrently installed `TEST_STATUS`, and that is the serial
        // guarding it. Holding the sidecar's instead would exclude today's
        // setters only because all of them happen to take both.
        let _lock = crate::test_support::env::env_serial();
        for path in [
            "",
            "relative/path",
            "/definitely/missing-gitpulse-devmap-status",
        ] {
            let answer = status(path);
            assert!(!answer.available, "{path:?} was accepted: {answer:?}");
            assert!(
                answer.status.is_none(),
                "{path:?} produced a payload: {answer:?}"
            );
            assert!(
                answer
                    .reason
                    .is_some_and(|reason| !reason.trim().is_empty()),
                "{path:?} refused without saying why"
            );
        }
    }

    /// A probe that did not answer must stand down and say why, rather than
    /// rebuild on a guess.
    ///
    /// This path used to be reachable only by accident: when the host was busy
    /// enough that the stub child missed [`STATUS_DEADLINE`], the tests above
    /// took it and failed with `SkipUnavailable`. Asking for it deliberately is
    /// what separates "the probe is not ready" from "the probe will never be
    /// ready", and it is the only assertion here that the refusal carries the
    /// probe's own reason instead of a generic sentence.
    #[test]
    #[cfg(unix)]
    fn live_refresh_stands_down_when_the_probe_cannot_answer() {
        let _lock = crate::harness::sidecar::test_serial();
        let repo = git_repo();
        let canonical = repo.path().canonicalize().unwrap();
        write_stub_artifacts(repo.path());
        let devmap = bind_recording_devmap(repo.path());
        devmap.set_status(unavailable_status("devmap timed out after 30s"));
        let outcome = crate::devmap::maybe_refresh(canonical.to_str().unwrap(), true);
        assert_eq!(
            outcome.decision,
            crate::devmap::LiveRefreshDecision::SkipUnavailable
        );
        assert!(outcome.build.is_none(), "{outcome:?}");
        assert_eq!(
            outcome.reason.as_deref(),
            Some("devmap timed out after 30s"),
            "the refusal must carry the probe's own reason"
        );
        // Standing down means standing down: an unavailable probe must not be
        // followed by a build spawned on no information at all.
        let log = argv_log(repo.path());
        assert!(
            !log.lines().any(|line| line.starts_with("build ")),
            "an unavailable probe spawned a build:\n{log}"
        );
    }

    /// A probe the spawn gate declined under load is not a devmap that cannot
    /// answer, and must not be reported as one.
    ///
    /// The live heal runs as background work, which the gate sheds outright
    /// while the spawn budget is low — exactly the condition at launch, when
    /// every restored tab asks at once. Reported as `skip_unavailable`, the
    /// frontend recorded the repository as settled and never asked again, so
    /// those maps stayed stale for the rest of the session. The wire name is
    /// asserted, not the variant, because the frontend switches on the string.
    #[test]
    #[cfg(unix)]
    fn live_refresh_reports_a_probe_shed_under_load_as_deferred() {
        use crate::engine::process::{refusal_message, Refusal};
        let _lock = crate::harness::sidecar::test_serial();
        let repo = git_repo();
        let canonical = repo.path().canonicalize().unwrap();
        write_stub_artifacts(repo.path());
        let devmap = bind_recording_devmap(repo.path());
        for refusal in [
            Refusal::Shed,
            Refusal::Refused {
                waited: Duration::from_millis(1_500),
            },
        ] {
            let declined = refusal_message("devmap", refusal);
            devmap.set_status(unavailable_status(declined.message()));
            let outcome = crate::devmap::maybe_refresh(canonical.to_str().unwrap(), true);
            assert_eq!(
                serde_json::to_value(outcome.decision).unwrap(),
                "skip_deferred",
                "{refusal:?}: {outcome:?}"
            );
            assert!(outcome.build.is_none(), "{outcome:?}");
            assert_eq!(outcome.reason.as_deref(), Some(declined.message()));
            // Nothing was examined, so nothing may be claimed about the store.
            assert!(
                !outcome.facts.available && !outcome.facts.is_fresh,
                "{outcome:?}"
            );
        }
        let log = argv_log(repo.path());
        assert!(
            !log.lines().any(|line| line.starts_with("build ")),
            "a deferred probe spawned a build:\n{log}"
        );
    }

    /// `devmap status` with the git spawn budget spent, in the class the code
    /// map's probe runs in. Returns the answer and what an index build, which
    /// stays on the git budget, said under the same bucket.
    #[cfg(unix)]
    fn status_and_build_with_the_git_budget_spent(
        stacked: bool,
    ) -> (CliStatus, Result<BuildOutcome, String>) {
        use crate::engine::process::{spent_gate, with_background_processes, with_gate_for_test};
        let _lock = crate::harness::sidecar::test_serial();
        let repo = git_repo();
        let canonical = repo.path().canonicalize().unwrap();
        if stacked {
            let lease = devmap_query::paths::stack_lease_path(&canonical);
            fs::create_dir_all(lease.parent().unwrap()).unwrap();
            fs::write(&lease, r#"{"format_version":1}"#).unwrap();
        }
        write_stub_artifacts(repo.path());
        let _devmap = bind_recording_devmap(repo.path());
        let path = canonical.to_str().unwrap();
        let gate = spent_gate();
        with_gate_for_test(gate, || {
            with_background_processes(|| (status(path), refresh(path)))
        })
    }

    /// A `devmap status` read spawns no git, so it is not admitted against the
    /// git spawn budget. The code map's probe was shed at 0.000 s fifteen times
    /// in four minutes on 2026-11-21 ("background work is shed while the git
    /// spawn rate budget is low (shared bucket: 0-22 tokens, needs more than
    /// 48)") and devmap never ran (card ft-f815889e, criterion C6). The build
    /// beside it is the control: it spends the same bucket and is still refused.
    #[test]
    #[cfg(unix)]
    fn a_status_read_is_answered_while_the_git_spawn_budget_is_spent() {
        let (answer, build) = status_and_build_with_the_git_budget_spent(false);
        assert!(answer.available, "the read was refused: {answer:?}");
        assert!(answer.status.is_some(), "{answer:?}");
        let build = build.expect_err("a build draws from the git budget");
        assert!(
            crate::engine::process::is_deferred_under_load(&build),
            "the control build was not refused by the budget: {build}"
        );
    }

    /// The same on a stacked lane, whose reads do spawn git (inside `devmap`,
    /// where the gate cannot see it): the answer is not "no git cost, so no
    /// budget" but a budget of their own, see the next test.
    #[test]
    #[cfg(unix)]
    fn a_stacked_lane_status_read_is_answered_while_the_git_spawn_budget_is_spent() {
        let (answer, _) = status_and_build_with_the_git_budget_spent(true);
        assert!(answer.available, "the read was refused: {answer:?}");
    }

    /// A status the gate deferred says so in a typed field, carrying the last
    /// good generation and how old that reading is, and is never fresh: nothing
    /// was examined, so the reading before it cannot vouch for the tree now
    /// (card ft-f815889e, criteria C7 and (b)). The code map reads this field
    /// instead of mistaking the deferral for a missing tool.
    #[test]
    #[cfg(unix)]
    fn a_deferred_status_carries_the_last_good_generation_and_its_age_and_is_never_fresh() {
        use crate::engine::process::with_forced_deferral;
        let _lock = crate::harness::sidecar::test_serial();
        let repo = git_repo();
        let canonical = repo.path().canonicalize().unwrap();
        let path = canonical.to_str().unwrap();
        fs::write(
            canonical.join("status.json"),
            r#"{"is_fresh":true,"schema_outdated":false,"generation_id":41}"#,
        )
        .unwrap();
        let _devmap = bind_recording_devmap(repo.path());
        let good = status(path);
        assert!(good.available, "{good:?}");

        let shed = serde_json::to_value(with_forced_deferral(|| status(path))).unwrap();
        assert_eq!(shed["available"], false, "{shed}");
        let last = &shed["deferral"]["last_good"];
        assert_eq!(last["generation_id"], 41, "{shed}");
        assert!(last["age_ms"].is_u64(), "{shed}");
        assert_ne!(
            last["fresh"],
            serde_json::json!(true),
            "a reading from before the deferral cannot say the index is fresh now: {shed}"
        );
        assert!(
            last["fresh_reason"]
                .as_str()
                .is_some_and(|reason| reason.contains("not re-checked")),
            "{shed}"
        );

        let other = git_repo();
        let other_path = other.path().canonicalize().unwrap();
        let unread = serde_json::to_value(with_forced_deferral(|| {
            status(other_path.to_str().unwrap())
        }))
        .unwrap();
        assert!(unread["deferral"].is_object(), "{unread}");
        assert!(
            unread["deferral"]["last_good"].is_null(),
            "no reading was ever taken of this repository: {unread}"
        );
    }

    /// A stacked lane's reads start git of their own inside `devmap`, where the
    /// git budget cannot see it, so reads have a budget of their own: with every
    /// slot taken a read waits the bound and is then deferred, in the sentence
    /// every reader of a deferral matches and without blaming the git budget.
    #[test]
    #[cfg(unix)]
    fn a_read_waits_a_bounded_time_for_the_devmap_read_budget_then_defers() {
        use crate::engine::process::{spent_gate, with_background_processes, with_gate_for_test};
        let _lock = crate::harness::sidecar::test_serial();
        let repo = git_repo();
        let canonical = repo.path().canonicalize().unwrap();
        let lease = devmap_query::paths::stack_lease_path(&canonical);
        fs::create_dir_all(lease.parent().unwrap()).unwrap();
        fs::write(&lease, r#"{"format_version":1}"#).unwrap();
        let _devmap = bind_recording_devmap(repo.path());
        let path = canonical.to_str().unwrap();
        let budget: &'static ReadBudget = Box::leak(Box::new(ReadBudget::new(2, 2)));
        let held: Vec<_> = (0..2)
            .map(|_| budget.admit(Duration::ZERO).expect("a free slot"))
            .collect();
        let read = || {
            with_read_budget(budget, Duration::from_millis(150), || {
                with_gate_for_test(spent_gate(), || with_background_processes(|| status(path)))
            })
        };

        let began = Instant::now();
        let refused = read();
        let waited = began.elapsed();
        let reason = refused.reason.clone().unwrap_or_default();
        assert!(!refused.available, "{refused:?}");
        assert!(
            crate::engine::process::is_deferred_under_load(&reason),
            "not a deferral: {reason}"
        );
        assert!(!reason.contains("git"), "blames the git budget: {reason}");
        assert!(
            waited >= Duration::from_millis(140)
                && waited < crate::test_support::coverage_relaxed(Duration::from_secs(5)),
            "waited {waited:?}, expected about 150 ms"
        );

        drop(held);
        let answered = read();
        assert!(
            answered.available,
            "a freed slot admits the read: {answered:?}"
        );
    }

    /// More readers than slots never run more children than slots, and the ones
    /// that had to wait are answered once a slot frees.
    #[test]
    #[cfg(unix)]
    fn devmap_reads_never_run_more_children_than_the_read_budget_has_slots() {
        use std::os::unix::fs::PermissionsExt;
        let _lock = crate::harness::sidecar::test_serial();
        let repo = git_repo();
        let canonical = repo.path().canonicalize().unwrap();
        let stub = repo.path().join("devmap");
        fs::write(
            &stub,
            "#!/bin/sh\nprintf 'S\\n' >> depth.log\nsleep 0.3\nprintf 'E\\n' >> depth.log\nprintf '%s\\n' '{\"is_fresh\":true,\"schema_outdated\":false}'\n",
        )
        .unwrap();
        fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
        let _devmap = bind_test_binary(stub.to_string_lossy().into_owned());
        let path = canonical.to_str().unwrap();
        let budget: &'static ReadBudget = Box::leak(Box::new(ReadBudget::new(2, 2)));
        let answers: Vec<CliStatus> = std::thread::scope(|scope| {
            let readers: Vec<_> = (0..6)
                .map(|_| {
                    scope.spawn(|| {
                        with_read_budget(budget, Duration::from_secs(20), || status(path))
                    })
                })
                .collect();
            readers.into_iter().map(|r| r.join().unwrap()).collect()
        });
        assert!(answers.iter().all(|a| a.available), "{answers:?}");
        let (mut depth, mut deepest) = (0usize, 0usize);
        for line in fs::read_to_string(canonical.join("depth.log"))
            .unwrap()
            .lines()
        {
            match line {
                "S" => depth += 1,
                _ => depth -= 1,
            }
            deepest = deepest.max(depth);
        }
        assert_eq!(depth, 0);
        assert!(deepest <= 2, "{deepest} children at once, budget is 2");
    }

    /// The reserve is real in both directions: background reads stop at their
    /// share however many slots are free, and a slot that is not theirs is
    /// there for a read that is not background work.
    #[test]
    fn background_reads_cannot_take_the_slots_kept_for_the_user() {
        use crate::engine::process::with_background_processes;
        let budget: &'static ReadBudget = Box::leak(Box::new(ReadBudget::new(3, 2)));
        let held: Vec<_> = with_background_processes(|| {
            (0..2)
                .map(|_| budget.admit(Duration::ZERO).expect("a background slot"))
                .collect()
        });
        let refused = with_background_processes(|| budget.admit(Duration::ZERO))
            .err()
            .expect("a third background read has no slot");
        assert!(
            crate::engine::process::is_deferred_under_load(refused.message()),
            "not a deferral: {}",
            refused.message()
        );
        let user = budget.admit(Duration::ZERO).expect("the reserved slot");
        assert!(budget.admit(Duration::ZERO).is_err(), "all three are held");
        drop(held);
        drop(user);
        assert!(budget.admit(Duration::ZERO).is_ok(), "every slot came back");
    }

    /// A user's read already waiting for a slot is served before a background
    /// read that arrives as the slot frees (the git gate's `waiting_interactive`).
    #[test]
    fn a_probe_arriving_as_a_slot_frees_cannot_jump_a_waiting_user_read() {
        use crate::engine::process::with_background_processes;
        for round in 0..40 {
            let budget: &'static ReadBudget = Box::leak(Box::new(ReadBudget::new(2, 2)));
            let held: Vec<_> = with_background_processes(|| {
                (0..2)
                    .map(|_| budget.admit(Duration::ZERO).expect("a background slot"))
                    .collect()
            });
            std::thread::scope(|scope| {
                let user = scope.spawn(|| budget.admit(Duration::from_secs(10)));
                assert!(
                    crate::test_support::wait_until(Duration::from_secs(10), || {
                        budget.waiting_user_now() > 0
                    }),
                    "the user's read never waited"
                );
                let mut held = held;
                drop(held.pop());
                let probe = with_background_processes(|| budget.admit(Duration::ZERO));
                assert!(
                    probe.is_err(),
                    "round {round}: a probe took the slot a user's read was waiting for"
                );
                user.join()
                    .unwrap()
                    .expect("the waiting user's read is admitted");
            });
        }
    }

    /// Reactive reads that keep the budget busy and always have one waiting do
    /// not starve the probes: with no background read held, a probe is admitted
    /// whatever user reads wait (the git gate's anti-starvation rule).
    #[test]
    fn a_steady_flood_of_user_reads_cannot_defer_a_probe_forever() {
        use crate::engine::process::with_background_processes;
        let budget: &'static ReadBudget = Box::leak(Box::new(ReadBudget::new(2, 2)));
        let stop = std::sync::atomic::AtomicBool::new(false);
        let admitted = std::thread::scope(|scope| {
            // Four readers on two slots: someone is always waiting.
            for _ in 0..4 {
                scope.spawn(|| {
                    while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                        if let Ok(slot) = budget.admit(Duration::from_secs(10)) {
                            std::thread::sleep(Duration::from_millis(5));
                            drop(slot);
                        }
                    }
                });
            }
            let waited = crate::test_support::wait_until(Duration::from_secs(10), || {
                budget.waiting_user_now() > 0
            });
            if !waited {
                stop.store(true, std::sync::atomic::Ordering::Relaxed);
            }
            assert!(waited, "no user read ever waited on the flooded budget");
            let probe = with_background_processes(|| budget.admit(Duration::from_secs(2)));
            stop.store(true, std::sync::atomic::Ordering::Relaxed);
            probe.map(drop)
        });
        assert!(
            admitted.is_ok(),
            "a probe was deferred behind user reads: {admitted:?}"
        );
    }

    /// A budget shaped as the process's own, leaked so a test can hold slots.
    fn production_shaped_budget() -> &'static ReadBudget {
        Box::leak(Box::new(ReadBudget::new(READ_SLOTS, BACKGROUND_READ_SLOTS)))
    }

    /// The code map's probes run as background work, up to `live::PROBE_LIMIT`
    /// at once, and a probe holds a slot for as long as its child runs. A
    /// user's own read (Refresh, doctor, preview: reactive or interactive work)
    /// must not queue behind them: it is admitted at once beside a full set of
    /// probes, while the probes themselves are still admitted up to their limit
    /// (card ft-f815889e, review finding C6).
    #[test]
    #[cfg(unix)]
    fn a_users_devmap_read_is_not_deferred_by_a_full_set_of_background_probes() {
        use crate::engine::process::{
            run_as_user_action, with_background_processes, with_gate_for_test,
        };
        use std::os::unix::fs::PermissionsExt;
        let _lock = crate::harness::sidecar::test_serial();
        let repo = git_repo();
        let canonical = repo.path().canonicalize().unwrap();
        let stub = repo.path().join("devmap");
        fs::write(
            &stub,
            "#!/bin/sh\nprintf 'S\\n' >> depth.log\nsleep 2\nprintf '%s\\n' '{\"is_fresh\":true,\"schema_outdated\":false}'\n",
        )
        .unwrap();
        fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
        let _devmap = bind_test_binary(stub.to_string_lossy().into_owned());
        let path = canonical.to_str().unwrap();
        let budget = production_shaped_budget();
        // A gate roomy enough for four background children whatever the host's
        // core count (background work gets a quarter of the slots): the test
        // must discriminate on a small host as well as on a large one.
        let gate = crate::engine::process::leak_gate(crate::engine::process::SpawnGate::new(16));
        let started =
            || fs::read_to_string(canonical.join("depth.log")).map_or(0, |log| log.lines().count());
        let (background, user, interactive) = std::thread::scope(|scope| {
            let probes: Vec<_> = (0..crate::devmap::live::PROBE_LIMIT)
                .map(|_| {
                    scope.spawn(|| {
                        with_gate_for_test(gate, || {
                            with_read_budget(budget, Duration::from_secs(20), || {
                                with_background_processes(|| status(path))
                            })
                        })
                    })
                })
                .collect();
            crate::test_support::wait_until(Duration::from_secs(10), || {
                started() >= crate::devmap::live::PROBE_LIMIT
            });
            assert_eq!(
                started(),
                crate::devmap::live::PROBE_LIMIT,
                "every probe up to the probe limit is admitted at once"
            );
            assert_eq!(
                budget.held_now(),
                (
                    crate::devmap::live::PROBE_LIMIT,
                    crate::devmap::live::PROBE_LIMIT
                ),
                "the probes hold every background slot before the user's read is issued"
            );
            let user = with_gate_for_test(gate, || {
                with_read_budget(budget, Duration::from_millis(150), || status(path))
            });
            let interactive = with_gate_for_test(gate, || {
                with_read_budget(budget, Duration::from_millis(150), || {
                    run_as_user_action(|| status(path))
                })
            });
            let background: Vec<CliStatus> =
                probes.into_iter().map(|p| p.join().unwrap()).collect();
            (background, user, interactive)
        });
        assert!(background.iter().all(|a| a.available), "{background:?}");
        assert!(user.available, "a reactive read was deferred: {user:?}");
        assert!(
            interactive.available,
            "a user action's read was deferred: {interactive:?}"
        );
    }

    /// A build that ran out of time is an outcome with `timed_out`; one that
    /// never started (slot wait refused, deferred, spawn failed, even down a
    /// path that spells "timeout") stays an error the caller can tell apart.
    #[test]
    fn only_a_build_that_ran_out_of_time_is_a_timed_out_outcome() {
        use crate::engine::process::{is_slot_wait_timeout, refusal_message, Refusal};
        let binary = ResolvedDevmap {
            path: "/bin/devmap".into(),
            lookup: DevmapLookup::PathSearch,
        };
        let ran_out = timed_out_outcome(&binary, "devmap timed out after 600s".into())
            .expect("a build that hung is an outcome");
        assert!(ran_out.timed_out && !ran_out.ok);

        let slot = "devmap timed out after 0.5s waiting for a process slot";
        let err = timed_out_outcome(&binary, slot.into()).expect_err("never started");
        assert!(
            is_slot_wait_timeout(err.message()),
            "{err}",
            err = err.message()
        );

        let deferred = refusal_message(
            "devmap",
            Refusal::Refused {
                waited: Duration::from_millis(500),
            },
        );
        assert_eq!(
            timed_out_outcome(&binary, deferred.clone()).expect_err("deferred"),
            deferred
        );

        let spawn =
            "Failed to spawn devmap: no such program: /Users/x/tools/timeout-wrapper/devmap";
        assert_eq!(
            timed_out_outcome(&binary, spawn.into()).expect_err("spawn failure"),
            spawn
        );
    }

    #[test]
    #[cfg(unix)]
    fn live_refresh_obsolete_payload_never_falls_back_to_incremental_across_a_storm() {
        let _lock = crate::harness::sidecar::test_serial();
        let repo = git_repo();
        let canonical = repo.path().canonicalize().unwrap();
        let devmap = bind_recording_devmap(repo.path());
        devmap.set_status(available_status(
            r#"{"is_fresh":false,"schema_outdated":false,"degraded_reason":"stored extraction payload is obsolete; rebuild with the current analyzer"}"#,
        ));
        for repo_changed in [true, false] {
            let outcome = crate::devmap::maybe_refresh(canonical.to_str().unwrap(), repo_changed);
            assert_eq!(
                outcome.decision,
                crate::devmap::LiveRefreshDecision::Refresh,
                "repo_changed={repo_changed}"
            );
        }
        assert_build_argv(&argv_log(repo.path()), true);
    }

    #[test]
    #[cfg(unix)]
    fn explicit_failed_report_cannot_be_a_successful_command() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = write_fake_devmap(dir.path());
        fs::write(&path, "#!/bin/sh\necho '{\"ok\":false}'\n").unwrap();
        let binary = ResolvedDevmap {
            path: path.to_string_lossy().into_owned(),
            lookup: DevmapLookup::PathSearch,
        };
        for command in ["build", "status", "preview"] {
            assert!(
                run_devmap(
                    &binary,
                    dir.path(),
                    &[command, "--json"],
                    None,
                    Duration::from_secs(5)
                )
                .is_err(),
                "{command} accepted an explicit failure with exit 0"
            );
        }
    }
    /// Install one stub that serves every repository and records, in a shared
    /// directory, how many stub children are alive at the same moment.
    ///
    /// Each repository's own `argv.log` (the stub runs with the repository as
    /// its working directory) says how many `status` and `build` children that
    /// repository was charged. `status` and `build` both linger long enough
    /// that callers released together genuinely overlap.
    #[cfg(unix)]
    fn write_overlap_devmap(dir: &Path, gate: &Path) -> PathBuf {
        let path = dir.join("devmap");
        let script = format!(
            r#"#!/bin/sh
printf '%s\n' "$*" >> argv.log
touch "{gate}/alive.$$"
ls "{gate}" | grep -c '^alive\.' >> "{gate}/peak.log"
if [ "$1" = "status" ]; then
  sleep 0.3
  rm -f "{gate}/alive.$$"
  printf '%s\n' '{{"is_fresh":false,"schema_outdated":false}}'
  exit 0
fi
if [ "$1" = "build" ]; then
  sleep 0.6
  rm -f "{gate}/alive.$$"
  printf '%s\n' '{{"ok":true}}'
  exit 0
fi
rm -f "{gate}/alive.$$"
exit 2
"#,
            gate = gate.display()
        );
        fs::write(&path, script).expect("write overlap devmap");
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&path).expect("meta").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&path, perms).expect("chmod");
        path
    }

    #[cfg(unix)]
    fn count_argv(repo: &Path, verb: &str) -> usize {
        argv_log(repo)
            .lines()
            .filter(|line| line.split_whitespace().next() == Some(verb))
            .count()
    }

    /// A watcher storm that reaches `maybe_refresh` from many threads at once,
    /// for several repositories, costs each repository one probe and at most
    /// one build, and never more live children than the background class may
    /// hold.
    ///
    /// The per-repo guard used to be taken only when the build spawned, so every
    /// caller that arrived while the first was still probing ran its own
    /// `devmap status` child, and a caller that probed before the first build
    /// finished then ran a second, redundant build behind it.
    #[test]
    #[cfg(unix)]
    fn concurrent_live_refreshes_cost_one_probe_and_one_build_per_repo() {
        const REPOS: usize = 4;
        const CALLERS: usize = 6;
        let _lock = crate::harness::sidecar::test_serial();
        let repos: Vec<_> = (0..REPOS).map(|_| git_repo()).collect();
        let canonical: Vec<String> = repos
            .iter()
            .map(|repo| {
                repo.path()
                    .canonicalize()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        let gate = tempfile::TempDir::new().expect("gate dir");
        let bin_dir = tempfile::TempDir::new().expect("bin dir");
        let bin = write_overlap_devmap(bin_dir.path(), gate.path());
        let _bound = bind_test_binary(bin.to_string_lossy().into_owned());

        let barrier = std::sync::Arc::new(std::sync::Barrier::new(REPOS * CALLERS));
        let mut handles = Vec::new();
        for path in &canonical {
            for _ in 0..CALLERS {
                let (path, barrier) = (path.clone(), std::sync::Arc::clone(&barrier));
                handles.push(std::thread::spawn(move || {
                    barrier.wait();
                    crate::engine::process::with_background_processes(|| {
                        crate::devmap::maybe_refresh(&path, true)
                    })
                }));
            }
        }
        let outcomes: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().expect("refresh thread"))
            .collect();

        let mut total_builds = 0;
        for repo in &repos {
            let statuses = count_argv(repo.path(), "status");
            let builds = count_argv(repo.path(), "build");
            assert!(
                statuses <= 1,
                "{statuses} probes for one repo:\n{}",
                argv_log(repo.path())
            );
            assert!(
                builds <= 1,
                "{builds} builds for one repo:\n{}",
                argv_log(repo.path())
            );
            total_builds += builds;
        }
        assert!(
            total_builds >= 1,
            "the storm never produced a build: {outcomes:?}"
        );
        let refreshed = outcomes
            .iter()
            .filter(|outcome| outcome.decision == crate::devmap::LiveRefreshDecision::Refresh)
            .count();
        assert_eq!(
            refreshed, total_builds,
            "a refresh decision without a build: {outcomes:?}"
        );
        let peak = fs::read_to_string(gate.path().join("peak.log"))
            .unwrap_or_default()
            .lines()
            .filter_map(|line| line.trim().parse::<usize>().ok())
            .max()
            .unwrap_or(0);
        assert!(
            (1..=4).contains(&peak),
            "background devmap children alive at once: {peak}"
        );
    }

    /// A caller turned away by a held permit spawns nothing and is told the
    /// writer is busy; the same call is admitted the moment the permit drops.
    #[test]
    #[cfg(unix)]
    fn a_held_permit_turns_a_live_refresh_away_without_a_probe() {
        let _lock = crate::harness::sidecar::test_serial();
        let repo = git_repo();
        let canonical = repo.path().canonicalize().unwrap();
        let path = canonical.to_str().unwrap();
        let _devmap = bind_recording_devmap(repo.path());
        let permit = claim_build(path).expect("first claim");
        assert!(claim_build(path).is_err(), "a second claim must be refused");
        let busy = crate::devmap::maybe_refresh(path, true);
        assert_eq!(
            busy.decision,
            crate::devmap::LiveRefreshDecision::SkipBuilding
        );
        assert!(busy.facts.already_building, "{busy:?}");
        assert_eq!(
            argv_log(repo.path()),
            "",
            "a refused caller spawned a child"
        );
        drop(permit);
        assert!(!is_build_in_flight(path));
        let admitted = crate::devmap::maybe_refresh(path, true);
        assert_ne!(
            admitted.decision,
            crate::devmap::LiveRefreshDecision::SkipBuilding,
            "{admitted:?}"
        );
        assert!(count_argv(repo.path(), "status") >= 1);
    }
    /// A repository is indexed by the live gate whenever it is a retained tab,
    /// but initialization runs only for the *active* one. So the build path
    /// itself has to guarantee the hygiene, or a background tab silently gains
    /// an untracked `.devmap/` that nothing ever wrote a rule for.
    #[test]
    #[cfg(unix)]
    fn building_an_unopened_repository_still_hides_its_state_directory() {
        let repo = tempfile::TempDir::new().unwrap();
        let out = std::process::Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(repo.path())
            .output_locked()
            .expect("git init");
        assert!(out.status.success());
        crate::test_support::trust_repo(repo.path());

        // Deliberately *not* calling `devmap::init::initialize` first: this is
        // the repository nobody activated.
        let bin = write_fake_devmap(repo.path());
        fs::write(
            &bin,
            "#!/bin/sh
printf '%s\n' '{\"ok\":true}'
",
        )
        .unwrap();
        let _bound = bind_test_binary(bin.to_string_lossy().into_owned());

        let built = refresh(&repo.path().to_string_lossy()).expect("refresh");
        assert!(built.ok, "{built:?}");

        // Asked behaviourally rather than by matching the file's text: after a
        // build, git must already ignore the directory. Before this moved into
        // the build path the answer here was `Added` — proof that the very
        // first thing to hide it was this assertion, long after the index had
        // been written.
        let after = super::super::init::ensure_state_dir_excluded(repo.path());
        assert!(
            matches!(
                after,
                super::super::init::ExcludeOutcome::AlreadyIgnored { .. }
            ),
            "the build path left the state directory visible: {after:?}"
        );
    }

    /// Hygiene is not a precondition for code intelligence. A repository whose
    /// exclude file cannot be written must still get its index — losing every
    /// answer over an untracked directory would be the worse trade.
    #[test]
    #[cfg(unix)]
    fn a_refused_exclude_does_not_cancel_the_build() {
        let repo = tempfile::TempDir::new().unwrap();
        let out = std::process::Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(repo.path())
            .output_locked()
            .expect("git init");
        assert!(out.status.success());
        crate::test_support::trust_repo(repo.path());

        // A symlinked exclude file is refused by design: writing through it
        // would redirect the append outside the git directory.
        let info = repo.path().join(".git").join("info");
        fs::create_dir_all(&info).unwrap();
        let exclude = info.join("exclude");
        let _ = fs::remove_file(&exclude);
        std::os::unix::fs::symlink(repo.path().join("elsewhere"), &exclude).unwrap();

        let bin = write_fake_devmap(repo.path());
        fs::write(
            &bin,
            "#!/bin/sh
printf '%s\n' '{\"ok\":true}'
",
        )
        .unwrap();
        let _bound = bind_test_binary(bin.to_string_lossy().into_owned());

        let built = build(&repo.path().to_string_lossy()).expect("build");
        assert!(
            built.ok,
            "a refused exclude must not cancel the build: {built:?}"
        );
    }

    /// The build child, run from a plain (non-background) caller, is in the OS
    /// background band and has a capped rayon pool. The stub reports what its
    /// own process sees: its scheduler priority (waiting for the parent's
    /// demotion, which lands just after the spawn) and `RAYON_NUM_THREADS`.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_build_child_runs_in_the_background_band_with_half_the_rayon_threads() {
        let _lock = crate::harness::sidecar::test_serial();
        let repo = git_repo();
        let bin = write_fake_devmap(repo.path());
        fs::write(
            &bin,
            r#"#!/bin/sh
i=0
p=
while [ $i -lt 1200 ]; do
  p=$(/bin/ps -o pri= -p $$ | tr -d ' ')
  [ "$p" -lt 10 ] && break
  sleep 0.05
  i=$((i+1))
done
printf '%s %s\n' "$p" "${RAYON_NUM_THREADS:-unset}" > build.probe
printf '%s\n' '{"ok":true}'
"#,
        )
        .unwrap();
        let _bound = bind_test_binary(bin.to_string_lossy().into_owned());
        assert_eq!(
            crate::engine::process::current_admission_name(),
            "reactive",
            "the caller is not a background one"
        );

        let built = build(&repo.path().to_string_lossy()).expect("build");
        assert!(built.ok, "{built:?}");
        let probe = fs::read_to_string(repo.path().join("build.probe")).expect("probe");
        let mut fields = probe.split_whitespace();
        let priority: i32 = fields.next().unwrap().parse().expect("priority");
        let rayon = fields.next().unwrap();
        assert!(priority < 10, "build ran at priority {priority}: {probe}");
        // `bind_test_binary` holds the env serial, so this read is stable.
        let expected = std::env::var("RAYON_NUM_THREADS").unwrap_or_else(|_| {
            let cpus = std::thread::available_parallelism().map_or(2, std::num::NonZeroUsize::get);
            crate::pools::rayon_threads(cpus).to_string()
        });
        assert_eq!(rayon, expected);
    }

    #[test]
    fn an_explicit_rayon_thread_count_on_the_command_is_kept() {
        let mut cmd = Command::new("devmap");
        cmd.env("RAYON_NUM_THREADS", "3");
        limit_build_threads(&mut cmd);
        let value = cmd
            .get_envs()
            .find(|(key, _)| *key == "RAYON_NUM_THREADS")
            .and_then(|(_, value)| value.map(|v| v.to_string_lossy().into_owned()));
        assert_eq!(value.as_deref(), Some("3"));
    }

    #[cfg(unix)]
    #[test]
    fn sync_stacked_delta_invokes_seed_sync_delta_json() {
        let repo = git_repo();
        let bin_dir = tempfile::tempdir().unwrap();
        let bin = write_fake_devmap(bin_dir.path());
        fs::write(
            &bin,
            r#"#!/bin/sh
printf '%s\n' "$*" > sync_delta.argv
printf '%s\n' '{"synced_files":2,"deleted_files":0,"cleaned_files":0,"rebased_to_standalone":null}'
"#,
        )
        .unwrap();
        let _bound = bind_test_binary(bin.to_string_lossy().into_owned());
        let out = sync_stacked_delta(&repo.path().to_string_lossy()).expect("sync_stacked_delta");
        assert!(out.ok, "{out:?}");
        let argv = fs::read_to_string(repo.path().join("sync_delta.argv")).expect("argv");
        let tokens: Vec<&str> = argv.split_whitespace().collect();
        assert!(
            tokens.starts_with(&["seed", "--sync-delta", "--json"]),
            "unexpected argv: {tokens:?}"
        );
    }
}
