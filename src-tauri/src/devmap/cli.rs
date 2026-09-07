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
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

/// Wall-clock budget for a cold or incremental build. Large repos can take
/// minutes; this is a backstop against a wedged child, not a target.
pub const BUILD_DEADLINE: Duration = Duration::from_secs(15 * 60);

/// Wall-clock budget for `devmap status --json`.
pub const STATUS_DEADLINE: Duration = Duration::from_secs(30);

/// Wall-clock budget for one `devmap preview` call.
pub const PREVIEW_DEADLINE: Duration = Duration::from_secs(60);

/// Cap on captured stdout. A status/preview JSON is small; a build report can
/// carry coverage gaps. 8 MiB is a hard ceiling, not a typical size.
pub const STDOUT_CAP: usize = 8 * 1024 * 1024;

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
}

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
    // Progress on stderr races with JSON on stdout in some terminals; never
    // ask for it when we are capturing machine-readable output.
    if args.contains(&"--json") && !args.contains(&"--progress") {
        cmd.arg("--progress").arg("never");
    }
    git_cli::run_bounded_capped(cmd, "devmap", deadline, stdin, STDOUT_CAP)
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
    // Refuse while a build holds the writer lock — preview reads the store.
    {
        let guards = build_guards()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let key = repo.to_string_lossy();
        if guards.contains(key.as_ref()) {
            return PreviewFileResult {
                file_path: file_path.to_string(),
                available: false,
                reason: Some(format!(
                    "a devmap build is running for {key}; preview refused until it finishes"
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
pub fn preview_many(
    repo_path: &str,
    files: &[(String, String)],
    mut cancel: impl FnMut() -> bool,
) -> PreviewOutcome {
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
            }
        }
    };
    let mut out = Vec::with_capacity(files.len());
    for (path, content) in files {
        if cancel() {
            return PreviewOutcome {
                available: true,
                binary: Some(binary.path),
                lookup: Some(binary.lookup),
                reason: Some("preview cancelled before all files finished".into()),
                files: out,
                cancelled: true,
            };
        }
        out.push(preview(repo_path, path, content));
    }
    PreviewOutcome {
        available: true,
        binary: Some(binary.path),
        lookup: Some(binary.lookup),
        reason: None,
        files: out,
        cancelled: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
    fn preview_surfaces_json_report_from_cli() {
        let _lock = crate::harness::sidecar::test_serial();
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
        assert!(result.available, "{:?}", result.reason);
        let report = result.report.expect("report");
        assert_eq!(report["parse_status"], "Clean");
        assert_eq!(report["compared_against"], "disk");
        assert_eq!(report["ambiguous_callers"], 0);
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
}
