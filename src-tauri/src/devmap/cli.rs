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

#[cfg(test)]
pub(crate) fn set_test_binary(path: Option<String>) {
    *TEST_BINARY
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = path;
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

/// Cold or full rebuild (`devmap build --manifest --json`).
pub fn build(repo_path: &str) -> Result<BuildOutcome, String> {
    let repo = validate_repo(repo_path)?;
    let _guard = BuildGuard::try_acquire(&repo)?;
    let binary = resolve_binary()?;
    match run_devmap(
        &binary,
        &repo,
        &["build", "--manifest", "--json"],
        None,
        BUILD_DEADLINE,
    ) {
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

/// Incremental rebuild (`devmap build --json` without `--full`).
pub fn refresh(repo_path: &str) -> Result<BuildOutcome, String> {
    let repo = validate_repo(repo_path)?;
    let _guard = BuildGuard::try_acquire(&repo)?;
    let binary = resolve_binary()?;
    match run_devmap(&binary, &repo, &["build", "--json"], None, BUILD_DEADLINE) {
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

/// `devmap status --json`.
pub fn status(repo_path: &str) -> CliStatus {
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

    #[cfg(unix)]
    struct ResetTestBinary;
    #[cfg(unix)]
    impl Drop for ResetTestBinary {
        fn drop(&mut self) {
            set_test_binary(None);
        }
    }

    #[cfg(unix)]
    fn bind_recording_devmap(repo: &Path) -> ResetTestBinary {
        let bin = write_recording_devmap(repo);
        set_test_binary(Some(bin.to_string_lossy().into_owned()));
        ResetTestBinary
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
        set_test_binary(None);
        // SAFETY: serialized behind the sidecar test guard; restored below.
        std::env::set_var("GITPULSE_DEVMAP_BIN", "/no/such/devmap-binary");
        let err = resolve_binary().expect_err("must refuse");
        std::env::remove_var("GITPULSE_DEVMAP_BIN");
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
        set_test_binary(Some(bin.to_string_lossy().into_owned()));
        let files: Vec<(String, String)> = (0..20)
            .map(|i| (format!("src/f{i}.rs"), "fn x() {}\n".into()))
            .collect();
        let out = preview_many(&repo.path().to_string_lossy(), &files, || false);
        set_test_binary(None);
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
        set_test_binary(Some(bin.to_string_lossy().into_owned()));
        let result = preview(
            &repo.path().to_string_lossy(),
            "src/lib.rs",
            "fn main() {}\n",
        );
        set_test_binary(None);
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
        set_test_binary(Some(bin.to_string_lossy().into_owned()));
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
        set_test_binary(None);
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
        set_test_binary(Some(bin.to_string_lossy().into_owned()));
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                set_test_binary(None);
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
        struct ResetBinary;
        impl Drop for ResetBinary {
            fn drop(&mut self) {
                set_test_binary(None);
            }
        }
        let _reset = ResetBinary;
        set_test_binary(Some(bin.to_string_lossy().into_owned()));
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
        let _reset = bind_recording_devmap(repo.path());
        fs::write(
            repo.path().join("status.json"),
            r#"{"is_fresh":false,"schema_outdated":false,"degraded_reason":"stored extraction payload is obsolete; rebuild with the current analyzer"}"#,
        )
        .unwrap();
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
        let _reset = bind_recording_devmap(repo.path());
        fs::write(
            repo.path().join("status.json"),
            r#"{"is_fresh":false,"schema_outdated":false,"rebuild_reason":"payload-obsolete"}"#,
        )
        .unwrap();
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
        let _reset = bind_recording_devmap(repo.path());
        fs::write(
            repo.path().join("status.json"),
            r#"{"is_fresh":true,"schema_outdated":false,"degraded_reason":"stored extraction payload is obsolete; rebuild with the current analyzer"}"#,
        )
        .unwrap();
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
        let _reset = bind_recording_devmap(repo.path());
        for reason in [
            "source tree differs from the indexed generation; rebuild or drain watcher edits",
            "source discovery refusals differ from the indexed generation; rebuild required",
            "analyzer freshness unverified: the binary was built without the parsing frontend",
            "source freshness unverified: this generation has no repository root",
        ] {
            let _ = fs::remove_file(repo.path().join("argv.log"));
            fs::write(
                repo.path().join("status.json"),
                format!(
                    r#"{{"is_fresh":false,"schema_outdated":false,"degraded_reason":{reason:?}}}"#
                ),
            )
            .unwrap();
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
        let _reset = bind_recording_devmap(repo.path());
        for payload in [
            r#"{"is_fresh":false,"schema_outdated":false}"#,
            r#"{"is_fresh":false,"schema_outdated":false,"degraded_reason":null}"#,
            r#"{"is_fresh":false,"schema_outdated":false,"degraded_reason":""}"#,
            r#"{"is_fresh":false,"schema_outdated":false,"degraded_reason":"obsolete"}"#,
            r#"{"is_fresh":false,"schema_outdated":false,"degraded_reason":"payload is obsolete"}"#,
            r#"{"is_fresh":false,"schema_outdated":false,"message":"stored extraction payload is obsolete; rebuild with the current analyzer"}"#,
        ] {
            let _ = fs::remove_file(repo.path().join("argv.log"));
            fs::write(repo.path().join("status.json"), payload).unwrap();
            let outcome = crate::devmap::maybe_refresh(canonical.to_str().unwrap(), true);
            assert_eq!(
                outcome.decision,
                crate::devmap::LiveRefreshDecision::Refresh,
                "{payload}"
            );
            assert_build_argv(&argv_log(repo.path()), false);
        }
    }

    #[test]
    #[cfg(unix)]
    fn live_refresh_schema_outdated_wins_over_obsolete_payload() {
        let _lock = crate::harness::sidecar::test_serial();
        let repo = git_repo();
        let canonical = repo.path().canonicalize().unwrap();
        let _reset = bind_recording_devmap(repo.path());
        fs::write(
            repo.path().join("status.json"),
            r#"{"is_fresh":false,"schema_outdated":true,"degraded_reason":"stored extraction payload is obsolete; rebuild with the current analyzer"}"#,
        )
        .unwrap();
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

    #[test]
    #[cfg(unix)]
    fn live_refresh_obsolete_payload_never_falls_back_to_incremental_across_a_storm() {
        let _lock = crate::harness::sidecar::test_serial();
        let repo = git_repo();
        let canonical = repo.path().canonicalize().unwrap();
        let _reset = bind_recording_devmap(repo.path());
        fs::write(
            repo.path().join("status.json"),
            r#"{"is_fresh":false,"schema_outdated":false,"degraded_reason":"stored extraction payload is obsolete; rebuild with the current analyzer"}"#,
        )
        .unwrap();
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
}
