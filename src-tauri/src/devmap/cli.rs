//! Locate and drive the installed `devmap` binary.
//!
//! Modelled on [`crate::harness::sidecar::resolve_binary`]: a GUI app does not
//! inherit a shell `PATH`, and `devmap` typically lives in `~/.cargo/bin`. An
//! explicit override that does not resolve is refused rather than searched past
//! — a path somebody typed is a statement, and quietly falling back hides the
//! typo behind whatever else is installed.

use crate::engine::git_cli::{self, validate_repo, BoundedRun};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
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

#[derive(Debug, Clone)]
struct PreviewDebounceEntry {
    until: Instant,
    content_fingerprint: u64,
}

fn preview_debounces() -> &'static Mutex<HashMap<(String, String), PreviewDebounceEntry>> {
    static MAP: OnceLock<Mutex<HashMap<(String, String), PreviewDebounceEntry>>> = OnceLock::new();
    MAP.get_or_init(|| Mutex::new(HashMap::new()))
}

#[cfg(test)]
pub(crate) fn clear_preview_debounces() {
    if let Ok(mut map) = preview_debounces().lock() {
        map.clear();
    }
}

fn content_fingerprint(content: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    content.hash(&mut hasher);
    hasher.finish()
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

/// Serializes every test that installs a global `devmap` override.
///
/// `TEST_BINARY` is process-wide, so two tests that bind their own stub at the
/// same time resolve each other's binary. The symptom is not a clean failure:
/// each stub appends argv to a log inside *its own* fixture, so one test
/// asserts against a log that is missing a spawn while another test's log
/// quietly holds it — and which test fails moves from run to run. Latent for as
/// long as the override has existed; anything that lengthens the window between
/// binding and unbinding makes it likelier.
///
/// Poison-tolerant, following [`crate::harness::sidecar::test_serial`]: a
/// panicking test must not wedge every later one, and the data behind this lock
/// is the emptiness of `()`.
#[cfg(test)]
pub(crate) fn test_serial() -> std::sync::MutexGuard<'static, ()> {
    static SERIAL: Mutex<()> = Mutex::new(());
    SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

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
    std::sync::MutexGuard<'static, ()>,
);

#[cfg(test)]
impl Drop for TestBinaryBinding {
    fn drop(&mut self) {
        set_test_binary(None);
        // Both overrides live behind the one serial this binding owns, so both
        // are released here. Leaving a canned status installed would hand it to
        // whichever test took the serial next.
        set_test_status(None);
    }
}

#[cfg(test)]
impl TestBinaryBinding {
    /// Answer [`status`] with `status` instead of running the probe, until this
    /// is called again or the binding is dropped.
    ///
    /// A method rather than a free function so the override cannot be installed
    /// without holding the serial: the binding *is* the proof of exclusivity.
    pub(crate) fn set_status(&self, status: CliStatus) {
        set_test_status(Some(status));
    }
}

/// Install `path` as the binary every lookup resolves, until the returned
/// binding is dropped.
#[cfg(test)]
pub(crate) fn bind_test_binary(path: impl Into<String>) -> TestBinaryBinding {
    let serial = test_serial();
    set_test_binary(Some(path.into()));
    TestBinaryBinding(serial)
}

/// A `devmap status --json` answer that parsed, for a test that is about what
/// the decision logic does with it.
#[cfg(test)]
pub(crate) fn available_status(payload: &str) -> CliStatus {
    CliStatus {
        available: true,
        binary: Some("test-override".into()),
        lookup: Some(DevmapLookup::TestOverride),
        reason: None,
        status: Some(
            serde_json::from_str(payload).expect("canned status payload must be valid JSON"),
        ),
    }
}

/// A probe that did not answer — the shape [`status`] returns when the child
/// could not be run, did not exit inside [`STATUS_DEADLINE`], or wrote
/// something that was not a JSON object.
#[cfg(test)]
pub(crate) fn unavailable_status(reason: &str) -> CliStatus {
    CliStatus {
        available: false,
        binary: Some("test-override".into()),
        lookup: Some(DevmapLookup::TestOverride),
        reason: Some(reason.into()),
        status: None,
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
    git_cli::find_external_tool(bin_name)
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

fn run_devmap(
    binary: &ResolvedDevmap,
    repo: &Path,
    args: &[&str],
    stdin: Option<&[u8]>,
    deadline: Duration,
) -> Result<BoundedRun, String> {
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
    let result = git_cli::run_observed(
        &mut cmd,
        "devmap",
        deadline,
        stdin,
        STDOUT_CAP,
        &mut diagnostic,
    );
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
        return Err(error);
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
) -> Result<BoundedRun, String> {
    run_devmap(binary, repo, args, None, deadline)
}

fn parse_json_stdout(stdout: &str) -> Option<Value> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return None;
    }
    serde_json::from_str(trimmed).ok()
}

fn timed_out(err: &str) -> bool {
    err.contains("timed out") || err.contains("timeout")
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
fn spawn_build(repo_path: &str, args: &[&str]) -> Result<BuildOutcome, String> {
    let repo = validate_repo(repo_path)?;
    let _guard = BuildGuard::try_acquire(&repo)?;
    let binary = resolve_binary()?;
    if let super::init::ExcludeOutcome::Refused { reason } =
        super::init::ensure_state_dir_excluded(&repo)
    {
        log::warn!(
            target: "devmap",
            "{}: building an index whose state directory git will show as untracked — {reason}",
            repo.display()
        );
    }
    match run_devmap(&binary, &repo, args, None, BUILD_DEADLINE) {
        Ok(run) => Ok(build_outcome_from_run(&binary, run)),
        Err(e) if timed_out(&e) => Ok(BuildOutcome {
            ok: false,
            binary: binary.path,
            lookup: binary.lookup,
            exit_code: None,
            stdout: String::new(),
            stderr: e,
            timed_out: true,
            report: None,
        }),
        Err(e) => Err(e),
    }
}

/// Cold or full rebuild (`devmap build --manifest --json`).
///
/// The manifest flag is what writes the consumer artifacts `repo_map.json` and
/// `code_graph.json`; a plain build updates the database only.
pub fn build(repo_path: &str) -> Result<BuildOutcome, String> {
    spawn_build(repo_path, &["build", "--manifest", "--json"])
}

/// Incremental rebuild (`devmap build --json` without `--full`).
pub fn refresh(repo_path: &str) -> Result<BuildOutcome, String> {
    spawn_build(repo_path, &["build", "--json"])
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
        Err(e) => return unavailable(e, None),
    };
    let binary = match resolve_binary() {
        Ok(b) => b,
        Err(e) => return unavailable(e, None),
    };
    let run = match run_devmap(&binary, &repo, &["doctor", "--json"], None, STATUS_DEADLINE) {
        Ok(run) => run,
        Err(e) => return unavailable(e, Some(binary.path)),
    };
    let stdout = bytes_to_string(run.stdout);
    let stderr = bytes_to_string(run.stderr);
    if !run.success {
        let detail = if stderr.trim().is_empty() {
            format!("devmap doctor exited {}", run.status_code)
        } else {
            stderr.trim().to_string()
        };
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
        if let Some(canned) = TEST_STATUS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
        {
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
                reason: Some(e),
                status: None,
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
            }
        }
    };
    let run = match run_devmap(&binary, &repo, &["status", "--json"], None, STATUS_DEADLINE) {
        Ok(run) => run,
        Err(e) => {
            return CliStatus {
                available: false,
                binary: Some(binary.path),
                lookup: Some(binary.lookup),
                reason: Some(e),
                status: None,
            }
        }
    };
    let stdout = bytes_to_string(run.stdout);
    let stderr = bytes_to_string(run.stderr);
    if !run.success {
        let detail = if stderr.trim().is_empty() {
            format!("devmap status exited {}", run.status_code)
        } else {
            stderr.trim().to_string()
        };
        return CliStatus {
            available: false,
            binary: Some(binary.path),
            lookup: Some(binary.lookup),
            reason: Some(detail),
            status: parse_json_stdout(&stdout),
        };
    }
    match parse_json_stdout(&stdout) {
        Some(status) => CliStatus {
            available: true,
            binary: Some(binary.path),
            lookup: Some(binary.lookup),
            reason: None,
            status: Some(status),
        },
        None => CliStatus {
            available: false,
            binary: Some(binary.path),
            lookup: Some(binary.lookup),
            reason: Some("devmap status returned non-JSON stdout".into()),
            status: None,
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
                reason: Some(e),
                report: None,
            }
        }
    };
    let repo_key = repo.to_string_lossy().into_owned();
    let fingerprint = content_fingerprint(content);
    let now = Instant::now();
    if let Ok(map) = preview_debounces().lock() {
        if let Some(entry) = map.get(&(repo_key.clone(), file_path.to_string())) {
            if entry.until > now {
                // Content changes inside the window still coalesce: a fixture
                // rewrite storm must not spawn one child per FS event.
                let _ = fingerprint == entry.content_fingerprint;
                return skipped_preview_file(file_path, PREVIEW_SKIP_DEBOUNCED);
            }
        }
    }
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
    // Admit the debounce slot before spawn so concurrent callers for the same
    // file see SkipDebounced rather than racing a second child.
    if let Ok(mut map) = preview_debounces().lock() {
        map.insert(
            (repo_key.clone(), file_path.to_string()),
            PreviewDebounceEntry {
                until: Instant::now() + PREVIEW_DEBOUNCE,
                content_fingerprint: fingerprint,
            },
        );
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
                reason: Some(e),
                report: None,
            }
        }
    };
    let stdout = bytes_to_string(run.stdout);
    let stderr = bytes_to_string(run.stderr);
    if !run.success {
        let detail = if stderr.trim().is_empty() {
            format!("devmap preview exited {}", run.status_code)
        } else {
            stderr.trim().to_string()
        };
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
    use super::*;

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
        for fallback in crate::engine::git_cli::external_tool_fallback_dirs() {
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
            .output()
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
        let _no_override = test_serial();
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
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                clear_preview_debounces();
            }
        }
        let _reset = Reset;
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
        let _lock = test_serial();
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
            .output()
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
            .output()
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
}
