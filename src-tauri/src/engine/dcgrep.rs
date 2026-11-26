//! Adapter for DevCouncil's `dcgrep` CLI in GitPulse's File Explorer.
//!
//! When a compatible `dcgrep` binary is available on the host (reporting
//! `schema_version: 1` and the required File Explorer capabilities via
//! `dcgrep health`), working-tree content search and working-tree file
//! enumeration use `dcgrep` first and fall back cleanly to `git` when absent,
//! unsupported, or on repository configurations that `dcgrep` + direct
//! `.git/index` reading do not emulate.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::engine::content_search::{
    clip_match_text, ContentMatch, ContentSearchOptions, ContentSearchReport, SEARCH_TIMEOUT,
};
use crate::engine::git_cli::sandbox_join;
use crate::engine::process::{self, Incomplete, KeepDeadlinePrefix, OutputStream, ProcessObserver};
use crate::ipc_error::CommandError;
use crate::repository_trust::git_directories;
use crate::storage::repo_tuning::index_file::{
    index_shape_supported, repo_has_custom_ignore_or_unicode_config,
};
use crate::tool_capability::BinaryProbeCache;
use crate::tool_install::release::app_bin_dir_path;

const GITPULSE_DCGREP_ENV: &str = "GITPULSE_DCGREP_BIN";
const MANVI_GREP_ENV: &str = "MANVI_GREP_BINARY";
const DCGREP_BIN_NAME: &str = if cfg!(windows) {
    "dcgrep.exe"
} else {
    "dcgrep"
};
const ABSENT_TTL: Duration = Duration::from_secs(60);
const HEALTH_TIMEOUT: Duration = Duration::from_secs(5);
const HEALTH_OUTPUT_CAP: usize = 64 * 1024;
const SEARCH_OUTPUT_CAP: usize = 64 * 1024 * 1024;
const DCGREP_MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;
const DCGREP_MAX_LINE_BYTES: usize = 4 * 1024;
const REGEX_SIZE_LIMIT: usize = 16 * 1024 * 1024;

static PROBE_CACHE: BinaryProbeCache<bool> = BinaryProbeCache::new();
static ABSENT_SINCE: Mutex<Option<Instant>> = Mutex::new(None);

#[cfg(test)]
#[derive(Clone)]
enum TestOverride {
    Path(PathBuf),
    Absent,
}

#[cfg(test)]
std::thread_local! {
    static TEST_OVERRIDE: std::cell::RefCell<Option<TestOverride>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) struct TestDcgrepBinding {
    _serial: crate::test_support::env::EnvGuard,
    prev: Option<TestOverride>,
}

#[cfg(test)]
impl TestDcgrepBinding {
    pub(crate) fn bind_test_binary(path: &Path) -> Self {
        let _serial = crate::test_support::env::env_serial();
        let prev = TEST_OVERRIDE.with(|cell| {
            cell.borrow_mut()
                .replace(TestOverride::Path(path.to_path_buf()))
        });
        invalidate_cache();
        Self { _serial, prev }
    }

    pub(crate) fn bind_absent_binary() -> Self {
        let _serial = crate::test_support::env::env_serial();
        let prev = TEST_OVERRIDE.with(|cell| cell.borrow_mut().replace(TestOverride::Absent));
        invalidate_cache();
        Self { _serial, prev }
    }
}

#[cfg(test)]
impl Drop for TestDcgrepBinding {
    fn drop(&mut self) {
        let prev = self.prev.take();
        TEST_OVERRIDE.with(|cell| {
            *cell.borrow_mut() = prev;
        });
        invalidate_cache();
    }
}

#[derive(Deserialize)]
struct HealthCapabilities {
    #[serde(default)]
    fixed_strings: bool,
    #[serde(default)]
    include_hidden: bool,
    #[serde(default)]
    preserve_whitespace: bool,
    #[serde(default)]
    include_oversized: bool,
    #[serde(default)]
    include_symlinks: bool,
    #[serde(default)]
    precompose_unicode: bool,
    #[serde(default)]
    include_devcouncil: bool,
    #[serde(default)]
    ignore_dot_ignore: bool,
    #[serde(default)]
    include_git_tracked: bool,
}

#[derive(Deserialize)]
struct HealthReply {
    #[serde(default)]
    ok: bool,
    #[serde(default)]
    searcher: String,
    #[serde(default)]
    schema_version: u32,
    capabilities: Option<HealthCapabilities>,
}

#[derive(Serialize)]
struct SearchRequest<'a> {
    pattern: &'a str,
    root: String,
    max_results: usize,
    include_ignored: bool,
    include_hidden: bool,
    fixed_strings: bool,
    case_insensitive: bool,
    preserve_whitespace: bool,
    precompose_unicode: bool,
    include_devcouncil: bool,
    ignore_dot_ignore: bool,
    include_git_tracked: bool,
    max_file_bytes: u64,
    max_line_bytes: usize,
}

#[derive(Default, Deserialize)]
struct SkippedCounts {
    #[serde(default)]
    too_large: usize,
    #[serde(default)]
    unreadable: usize,
    #[serde(default)]
    unrepresentable_name: usize,
}

#[derive(Deserialize)]
struct SearchHit {
    path: String,
    line_number: u64,
    line: String,
    #[serde(default)]
    line_truncated: bool,
}

#[derive(Deserialize)]
struct SearchReply {
    #[serde(default)]
    ok: bool,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    count: usize,
    #[serde(default)]
    matches: Vec<SearchHit>,
    #[serde(default)]
    truncated: bool,
    #[serde(default)]
    skipped: SkippedCounts,
}

struct CancelObserver<'a> {
    user_cancel: &'a dyn Fn() -> bool,
    user_cancelled: bool,
}

impl ProcessObserver for CancelObserver<'_> {
    fn cancelled(&self) -> bool {
        self.user_cancelled || (self.user_cancel)()
    }

    fn cancellation(&self) -> Option<CommandError> {
        if self.cancelled() {
            Some(crate::engine::process::read_cancel::cancelled())
        } else {
            None
        }
    }

    fn output(&mut self, _stream: OutputStream, _bytes: &[u8]) {
        if (self.user_cancel)() {
            self.user_cancelled = true;
        }
    }
}

pub(crate) fn invalidate_cache() {
    PROBE_CACHE.clear();
    let mut absent = ABSENT_SINCE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *absent = None;
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable_file(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|meta| meta.is_file())
        .unwrap_or(false)
}

fn env_var_nonempty(name: &str) -> Option<String> {
    let val = std::env::var_os(name)?;
    let text = val.to_string_lossy();
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn absent_cached() -> bool {
    #[cfg(test)]
    if process::forced_failure_active() {
        return false;
    }
    let guard = ABSENT_SINCE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    guard.is_some_and(|at| at.elapsed() < ABSENT_TTL)
}

fn set_absent_cache(is_absent: bool) {
    #[cfg(test)]
    if process::forced_failure_active() {
        return;
    }
    let mut guard = ABSENT_SINCE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *guard = is_absent.then(Instant::now);
}

fn locate_binary_with_policy(
    use_absent_cache: bool,
    fail_on_invalid_gitpulse_env: bool,
) -> Result<Option<(String, bool)>, CommandError> {
    #[cfg(test)]
    if let Some(binding) = TEST_OVERRIDE.with(|cell| cell.borrow().clone()) {
        return match binding {
            TestOverride::Absent => Ok(None),
            TestOverride::Path(path) => {
                if is_executable_file(&path) || !fail_on_invalid_gitpulse_env {
                    Ok(Some((path.to_string_lossy().into_owned(), false)))
                } else {
                    Ok(None)
                }
            }
        };
    }

    if let Some(explicit) = env_var_nonempty(GITPULSE_DCGREP_ENV) {
        let path = PathBuf::from(&explicit);
        if fail_on_invalid_gitpulse_env && !is_executable_file(&path) {
            return Err(format!(
                "{GITPULSE_DCGREP_ENV} points to a missing or non-executable binary: {}",
                path.display()
            )
            .into());
        }
        return Ok(Some((path.to_string_lossy().into_owned(), true)));
    }

    if let Some(manvi_bin) = env_var_nonempty(MANVI_GREP_ENV) {
        if manvi_bin.contains('/') || manvi_bin.contains('\\') {
            let path = Path::new(&manvi_bin);
            if is_executable_file(path) || !fail_on_invalid_gitpulse_env {
                return Ok(Some((manvi_bin, false)));
            }
            return Ok(None);
        }
        return Ok(process::find_external_tool(&manvi_bin).map(|path| (path, false)));
    }

    if let Ok(bin_dir) = app_bin_dir_path() {
        let bundled = bin_dir.join(DCGREP_BIN_NAME);
        if is_executable_file(&bundled) {
            return Ok(Some((bundled.to_string_lossy().into_owned(), false)));
        }
    }

    if use_absent_cache && absent_cached() {
        return Ok(None);
    }

    match process::find_external_tool("dcgrep") {
        Some(found) => {
            if use_absent_cache {
                set_absent_cache(false);
            }
            Ok(Some((found, false)))
        }
        None => {
            if use_absent_cache {
                set_absent_cache(true);
            }
            Ok(None)
        }
    }
}

/// Canonical host-binary locator shared with the DevCouncil suite inventory.
pub(crate) fn locate_binary_for_inventory() -> Option<String> {
    locate_binary_with_policy(false, false)
        .ok()
        .flatten()
        .map(|(path, _)| path)
}

fn resolve_candidate_binary() -> Result<Option<(String, bool)>, CommandError> {
    locate_binary_with_policy(true, true)
}

fn is_load_deferral(err: &CommandError) -> bool {
    let msg = err.message();
    process::is_deferred_under_load(msg) || process::is_slot_wait_timeout(msg)
}

fn health_supports_explorer(stdout: &[u8]) -> bool {
    let Ok(reply) = serde_json::from_slice::<HealthReply>(stdout) else {
        return false;
    };
    let Some(caps) = reply.capabilities else {
        return false;
    };
    reply.ok
        && reply.searcher == "dc-grep"
        && reply.schema_version == 1
        && caps.fixed_strings
        && caps.include_hidden
        && caps.preserve_whitespace
        && caps.include_oversized
        && caps.include_symlinks
        && caps.precompose_unicode
        && caps.include_devcouncil
        && caps.ignore_dot_ignore
        && caps.include_git_tracked
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum HealthProbeOutcome {
    Supported,
    Unsupported,
    Cancelled,
}

fn run_health_probe_inner(
    bin: &str,
    cancelled: &dyn Fn() -> bool,
    exec: impl FnOnce(
        &mut std::process::Command,
        &mut dyn ProcessObserver,
    ) -> Result<process::BoundedRun, CommandError>,
) -> Result<HealthProbeOutcome, CommandError> {
    if cancelled() {
        return Ok(HealthProbeOutcome::Cancelled);
    }
    let temp = std::env::temp_dir();
    let mut cmd = process::build_capture_command(bin, &["health"], Some(&temp), &[], None, None);
    let mut observer = CancelObserver {
        user_cancel: cancelled,
        user_cancelled: false,
    };
    let run = match exec(&mut cmd, &mut observer) {
        Ok(run) => run,
        Err(err) => {
            if observer.user_cancelled || cancelled() {
                return Ok(HealthProbeOutcome::Cancelled);
            }
            return Err(err);
        }
    };
    if run.cancelled || observer.user_cancelled || cancelled() {
        return Ok(HealthProbeOutcome::Cancelled);
    }
    if !run.success || run.incomplete.is_some() {
        return Ok(HealthProbeOutcome::Unsupported);
    }
    if health_supports_explorer(&run.stdout) {
        Ok(HealthProbeOutcome::Supported)
    } else {
        Ok(HealthProbeOutcome::Unsupported)
    }
}

fn run_health_probe(
    bin: &str,
    cancelled: &dyn Fn() -> bool,
) -> Result<HealthProbeOutcome, CommandError> {
    run_health_probe_inner(bin, cancelled, |cmd, observer| {
        process::run_observed(
            cmd,
            "dcgrep health",
            HEALTH_TIMEOUT,
            None,
            HEALTH_OUTPUT_CAP,
            observer,
        )
    })
}

#[cfg(test)]
fn run_health_probe_on_gate(
    bin: &str,
    cancelled: &dyn Fn() -> bool,
    gate: &'static process::SpawnGate,
) -> Result<HealthProbeOutcome, CommandError> {
    run_health_probe_inner(bin, cancelled, |cmd, observer| {
        process::run_with_gate(
            cmd,
            "dcgrep health",
            HEALTH_TIMEOUT,
            None,
            HEALTH_OUTPUT_CAP,
            observer,
            gate,
        )
    })
}

fn probe_capabilities_inner(
    bin: &str,
    cancelled: &dyn Fn() -> bool,
    probe: impl FnOnce() -> Result<HealthProbeOutcome, CommandError>,
) -> Result<HealthProbeOutcome, CommandError> {
    if cancelled() {
        return Ok(HealthProbeOutcome::Cancelled);
    }
    let mut deferred_err: Option<CommandError> = None;
    let mut probe_cancelled = false;
    let supported = PROBE_CACHE.get_or_probe(bin, || match probe() {
        Ok(HealthProbeOutcome::Supported) => (true, true),
        Ok(HealthProbeOutcome::Unsupported) => (false, true),
        Ok(HealthProbeOutcome::Cancelled) => {
            probe_cancelled = true;
            (false, false)
        }
        Err(err) => {
            if is_load_deferral(&err) {
                deferred_err = Some(err);
            }
            (false, false)
        }
    });
    if let Some(err) = deferred_err {
        return Err(err);
    }
    if probe_cancelled || cancelled() {
        return Ok(HealthProbeOutcome::Cancelled);
    }
    Ok(if supported {
        HealthProbeOutcome::Supported
    } else {
        HealthProbeOutcome::Unsupported
    })
}

fn probe_capabilities(
    bin: &str,
    cancelled: &dyn Fn() -> bool,
) -> Result<HealthProbeOutcome, CommandError> {
    probe_capabilities_inner(bin, cancelled, || run_health_probe(bin, cancelled))
}

#[cfg(test)]
fn probe_capabilities_on_gate(
    bin: &str,
    cancelled: &dyn Fn() -> bool,
    gate: &'static process::SpawnGate,
) -> Result<HealthProbeOutcome, CommandError> {
    probe_capabilities_inner(bin, cancelled, || {
        run_health_probe_on_gate(bin, cancelled, gate)
    })
}

enum UsableBinary {
    Ready(String),
    Unavailable,
    Cancelled,
}

fn resolve_usable_binary_inner(
    cancelled: &dyn Fn() -> bool,
    probe: impl FnOnce(&str) -> Result<HealthProbeOutcome, CommandError>,
) -> Result<UsableBinary, CommandError> {
    if cancelled() {
        return Ok(UsableBinary::Cancelled);
    }
    let Some((bin, explicit_gitpulse_env)) = resolve_candidate_binary()? else {
        return Ok(UsableBinary::Unavailable);
    };
    match probe(&bin)? {
        HealthProbeOutcome::Supported => Ok(UsableBinary::Ready(bin)),
        HealthProbeOutcome::Cancelled => Ok(UsableBinary::Cancelled),
        HealthProbeOutcome::Unsupported => {
            if explicit_gitpulse_env {
                Err(format!(
                    "{GITPULSE_DCGREP_ENV} points to an unusable or incompatible dcgrep binary: {bin}"
                )
                .into())
            } else {
                Ok(UsableBinary::Unavailable)
            }
        }
    }
}

fn resolve_usable_binary_with(cancelled: &dyn Fn() -> bool) -> Result<UsableBinary, CommandError> {
    resolve_usable_binary_inner(cancelled, |bin| probe_capabilities(bin, cancelled))
}

#[cfg(test)]
fn resolve_usable_binary_on_gate(
    cancelled: &dyn Fn() -> bool,
    gate: &'static process::SpawnGate,
) -> Result<UsableBinary, CommandError> {
    resolve_usable_binary_inner(cancelled, |bin| {
        probe_capabilities_on_gate(bin, cancelled, gate)
    })
}

#[cfg(test)]
fn resolve_usable_binary() -> Result<Option<String>, CommandError> {
    match resolve_usable_binary_with(&|| false)? {
        UsableBinary::Ready(bin) => Ok(Some(bin)),
        UsableBinary::Unavailable | UsableBinary::Cancelled => Ok(None),
    }
}

fn cancelled_report() -> ContentSearchReport {
    ContentSearchReport {
        matches: Vec::new(),
        files: 0,
        truncated: true,
        truncated_reason: Some("cancelled".to_string()),
        revision: None,
    }
}

fn incomplete_report(reason: &str) -> ContentSearchReport {
    ContentSearchReport {
        matches: Vec::new(),
        files: 0,
        truncated: true,
        truncated_reason: Some(reason.to_string()),
        revision: None,
    }
}

fn find_literal_byte_pos(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn has_unsupported_repo_layout(repo: &Path) -> Option<(PathBuf, PathBuf)> {
    if !repo.join(".git").exists() || repo.join(".gitmodules").exists() {
        return None;
    }
    let Ok((git_dir, common_dir)) = git_directories(repo) else {
        return None;
    };
    if repo_has_custom_ignore_or_unicode_config(&git_dir, &common_dir) {
        return None;
    }
    Some((git_dir, common_dir))
}

fn pattern_has_unescaped_end_anchor(pattern: &str) -> bool {
    let mut escaped = false;
    for ch in pattern.chars() {
        if escaped {
            escaped = false;
            if matches!(ch, 'z' | 'Z') {
                return true;
            }
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        if ch == '$' {
            return true;
        }
    }
    false
}

/// Attempts working-tree content search via `dcgrep` when a compatible binary
/// is installed and repository rules match standard ignore semantics.
/// Returns `Ok(None)` to fall back cleanly to `git grep`.
pub(crate) fn search_working_tree(
    repo: &Path,
    pattern: &str,
    options: &ContentSearchOptions,
    limit: usize,
    cancelled: &dyn Fn() -> bool,
) -> Result<Option<ContentSearchReport>, CommandError> {
    search_working_tree_inner(repo, pattern, options, limit, cancelled, || {
        resolve_usable_binary_with(cancelled)
    })
}

#[cfg(test)]
fn search_working_tree_on_gate(
    repo: &Path,
    pattern: &str,
    options: &ContentSearchOptions,
    limit: usize,
    cancelled: &dyn Fn() -> bool,
    gate: &'static process::SpawnGate,
) -> Result<Option<ContentSearchReport>, CommandError> {
    search_working_tree_inner(repo, pattern, options, limit, cancelled, || {
        resolve_usable_binary_on_gate(cancelled, gate)
    })
}

fn search_working_tree_inner(
    repo: &Path,
    pattern: &str,
    options: &ContentSearchOptions,
    limit: usize,
    cancelled: &dyn Fn() -> bool,
    resolve_bin: impl FnOnce() -> Result<UsableBinary, CommandError>,
) -> Result<Option<ContentSearchReport>, CommandError> {
    if cancelled() {
        return Ok(Some(cancelled_report()));
    }
    let bin = match resolve_bin()? {
        UsableBinary::Ready(bin) => bin,
        UsableBinary::Unavailable => return Ok(None),
        UsableBinary::Cancelled => return Ok(Some(cancelled_report())),
    };
    if !options.fixed_strings && pattern.trim().is_empty() {
        return Ok(None);
    }
    let Some((git_dir, common_dir)) = has_unsupported_repo_layout(repo) else {
        return Ok(None);
    };
    if !index_shape_supported(&git_dir, &common_dir) {
        return Ok(None);
    }
    if cancelled() {
        return Ok(Some(cancelled_report()));
    }

    let request = SearchRequest {
        pattern,
        root: process::plain_path_text(&repo.to_string_lossy()),
        max_results: limit,
        include_ignored: false,
        include_hidden: true,
        fixed_strings: options.fixed_strings,
        case_insensitive: options.ignore_case,
        preserve_whitespace: true,
        precompose_unicode: cfg!(target_os = "macos"),
        include_devcouncil: true,
        ignore_dot_ignore: true,
        include_git_tracked: true,
        max_file_bytes: DCGREP_MAX_FILE_BYTES,
        max_line_bytes: DCGREP_MAX_LINE_BYTES,
    };
    let Ok(body) = serde_json::to_vec(&request) else {
        return Ok(None);
    };

    let _keep = KeepDeadlinePrefix::enter();
    let mut observer = CancelObserver {
        user_cancel: cancelled,
        user_cancelled: false,
    };
    let mut cmd = process::build_capture_command(&bin, &["search"], Some(repo), &[], None, None);
    let run = match process::run_observed(
        &mut cmd,
        "dcgrep search",
        SEARCH_TIMEOUT,
        Some(&body),
        SEARCH_OUTPUT_CAP,
        &mut observer,
    ) {
        Ok(run) => run,
        Err(err) => {
            if observer.user_cancelled || cancelled() {
                return Ok(Some(cancelled_report()));
            }
            if is_load_deferral(&err) {
                return Err(err);
            }
            if process::is_spawn_not_found(err.message()) {
                return Ok(None);
            }
            return Ok(None);
        }
    };

    if run.cancelled || observer.user_cancelled || cancelled() {
        return Ok(Some(cancelled_report()));
    }
    if let Some(incomplete) = run.incomplete {
        return Ok(Some(match incomplete {
            Incomplete::Deadline { .. } => incomplete_report("deadline"),
            Incomplete::OverCap(_) | Incomplete::Unread(_) => incomplete_report("output_cap"),
        }));
    }

    let Ok(reply) = serde_json::from_slice::<SearchReply>(&run.stdout) else {
        return Ok(None);
    };
    if !reply.ok {
        let detail = reply
            .error
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("invalid search request");
        return Err(format!("Search failed: {detail}").into());
    }
    if !run.success || reply.count != reply.matches.len() {
        return Ok(None);
    }
    if reply.skipped.too_large > 0
        || reply.skipped.unreadable > 0
        || reply.skipped.unrepresentable_name > 0
    {
        return Ok(None);
    }

    let col_regex = if options.fixed_strings && !options.ignore_case {
        None
    } else {
        let regex_source = if options.fixed_strings {
            regex::escape(pattern)
        } else {
            pattern.to_string()
        };
        let Ok(compiled) = regex::bytes::RegexBuilder::new(&regex_source)
            .case_insensitive(options.ignore_case)
            .multi_line(true)
            .crlf(true)
            .size_limit(REGEX_SIZE_LIMIT)
            .build()
        else {
            return Ok(None);
        };
        Some(compiled)
    };

    let mut matches = Vec::with_capacity(reply.matches.len());
    let mut distinct_files: HashSet<String> = HashSet::new();
    for hit in reply.matches {
        if hit.line_number == 0
            || hit
                .path
                .split('/')
                .any(|seg| seg.is_empty() || seg == "." || seg == "..")
            || !Path::new(&hit.path)
                .components()
                .all(|c| matches!(c, std::path::Component::Normal(_)))
            || sandbox_join(repo, &hit.path).is_err()
        {
            return Ok(None);
        }
        if hit.line.contains('\u{FFFD}') {
            return Ok(None);
        }
        if hit.line_truncated && !options.fixed_strings && pattern_has_unescaped_end_anchor(pattern)
        {
            return Ok(None);
        }
        let col_idx = if let Some(re) = &col_regex {
            re.find(hit.line.as_bytes()).map(|m| m.start())
        } else {
            find_literal_byte_pos(hit.line.as_bytes(), pattern.as_bytes())
        };
        let Some(col_idx) = col_idx else {
            return Ok(None);
        };
        let Some(column) = u64::try_from(col_idx + 1).ok() else {
            return Ok(None);
        };
        let (text, clipped_here) = clip_match_text(&hit.line);
        distinct_files.insert(hit.path.clone());
        matches.push(ContentMatch {
            path: hit.path,
            line: hit.line_number,
            column,
            text,
            text_clipped: hit.line_truncated || clipped_here,
        });
    }

    Ok(Some(ContentSearchReport {
        matches,
        files: distinct_files.len(),
        truncated: reply.truncated,
        truncated_reason: reply.truncated.then(|| "match_limit".to_string()),
        revision: None,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::content_search;
    use crate::engine::git_reader::GitReader;
    use crate::test_support::env::{bind_env, env_serial};
    use tempfile::TempDir;

    fn git_in(dir: &Path, args: &[&str]) {
        let cmd = process::build_capture_command("git", args, Some(dir), &[], None, None);
        let run = process::run_bounded_capped(
            cmd,
            "git test helper",
            Duration::from_secs(10),
            None,
            1024 * 1024,
        )
        .expect("run git");
        assert!(
            run.success,
            "git {args:?} failed: stdout={} stderr={}",
            String::from_utf8_lossy(&run.stdout),
            String::from_utf8_lossy(&run.stderr)
        );
        if args.first() == Some(&"init") {
            crate::test_support::trust_repo(dir);
        }
    }

    fn init_repo() -> TempDir {
        let dir = TempDir::new().unwrap();
        git_in(dir.path(), &["init", "-b", "main"]);
        git_in(dir.path(), &["config", "user.name", "Test"]);
        git_in(dir.path(), &["config", "user.email", "test@example.com"]);
        dir
    }

    fn write_exe(dir: &Path, name: &str, script: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        path
    }

    /// A launcher that answers `health` with `json` after `delay_secs`, exits 1
    /// for anything else, and writes `trace` whenever it starts. A shell script
    /// on Unix; Windows runs no `#!` script, so there it is a batch file.
    fn health_stub(dir: &Path, json: &str, delay_secs: u32, trace: Option<&Path>) -> PathBuf {
        #[cfg(unix)]
        {
            let trace = trace
                .map(|path| format!("echo spawned > '{}'\n", path.display()))
                .unwrap_or_default();
            let delay = if delay_secs > 0 {
                format!("  sleep {delay_secs}\n")
            } else {
                String::new()
            };
            write_exe(
                dir,
                "dcgrep",
                &format!(
                    "#!/bin/sh\n{trace}if [ \"$1\" = \"health\" ]; then\n{delay}  echo '{json}'\n  exit 0\nfi\nexit 1\n"
                ),
            )
        }
        #[cfg(not(unix))]
        {
            let trace = trace
                .map(|path| format!("echo spawned> \"{}\"\r\n", path.display()))
                .unwrap_or_default();
            // `ping -n N` waits N-1 seconds.
            let delay = if delay_secs > 0 {
                format!("  ping -n {} 127.0.0.1 >nul\r\n", delay_secs + 1)
            } else {
                String::new()
            };
            write_exe(
                dir,
                "dcgrep.cmd",
                &format!(
                    "@echo off\r\n{trace}if \"%1\"==\"health\" (\r\n{delay}  echo {json}\r\n  exit /b 0\r\n)\r\nexit /b 1\r\n"
                ),
            )
        }
    }

    const FULL_HEALTH: &str = r#"{"ok":true,"searcher":"dc-grep","schema_version":1,"capabilities":{"fixed_strings":true,"include_hidden":true,"preserve_whitespace":true,"include_oversized":true,"include_symlinks":true,"precompose_unicode":true,"include_devcouncil":true,"ignore_dot_ignore":true,"include_git_tracked":true}}"#;

    #[test]
    fn installed_dcgrep_handles_search_with_zero_git_grep_and_list_uses_git_ls_files() {
        let _serial = env_serial();
        if resolve_usable_binary().ok().flatten().is_none() {
            return;
        }
        let dir = init_repo();
        let root = dir.path();
        std::fs::write(root.join("src.rs"), "fn needle_one() {}\n").unwrap();
        git_in(root, &["add", "src.rs"]);
        git_in(root, &["commit", "-m", "init"]);
        std::fs::write(root.join("untracked.rs"), "fn needle_two() {}\n").unwrap();

        let root_str = root.to_string_lossy().into_owned();
        let canon = process::canonicalize_plain(root).unwrap();
        // Warm repository_trust directory resolution once before counting spawns.
        let _ = git_directories(&canon);

        let before = process::spawn_log::spawns_in(&canon).len();
        let files = GitReader::list_repo_files(&root_str).expect("list_repo_files");
        let report = content_search::search(
            &root_str,
            "needle_",
            &ContentSearchOptions::default(),
            &|| false,
        )
        .expect("content_search");
        let after: Vec<Vec<String>> = process::spawn_log::spawns_in(&canon)
            .into_iter()
            .skip(before)
            .collect();

        assert_eq!(files, vec!["src.rs", "untracked.rs"]);
        assert_eq!(report.matches.len(), 2);
        assert!(
            !after
                .iter()
                .any(|argv| argv.iter().any(|arg| arg == "grep")),
            "expected zero git grep spawns when dcgrep handles content search, got: {after:?}"
        );
        assert!(
            after
                .iter()
                .any(|argv| argv.iter().any(|arg| arg == "ls-files")),
            "expected git ls-files spawn for the listing: {after:?}"
        );
        assert!(
            !after
                .iter()
                .any(|argv| argv.first().map(String::as_str) == Some("files")),
            "listing must not spawn `dcgrep files`: {after:?}"
        );
        assert!(
            after
                .iter()
                .any(|argv| argv.first().map(String::as_str) == Some("search")),
            "expected dcgrep search spawn in log: {after:?}"
        );
    }

    #[test]
    fn dcgrep_and_git_fallback_match_across_edge_cases_and_linked_worktree() {
        let _serial = env_serial();
        let host_bin = resolve_usable_binary().ok().flatten();

        // 1. Primary repo exercising listing + content search edge cases.
        let dir = init_repo();
        let root = dir.path();
        git_in(root, &["config", "core.ignorecase", "true"]);
        std::fs::write(
            root.join(".gitignore"),
            "*.ignored\nAGENTS.md\nBUILD/\n.devcouncil/*\n!.devcouncil/config.yaml\n!.devcouncil/claude-plugin/\n",
        )
        .unwrap();
        // `.ignore` must NOT override `.gitignore` in File Explorer or Content Search.
        std::fs::write(root.join(".ignore"), "!secret.ignored\n!build/\n").unwrap();
        std::fs::create_dir_all(root.join(".github")).unwrap();
        std::fs::write(
            root.join(".github/ci.yml"),
            "  name: ci needle_match (a+)+\n",
        )
        .unwrap();
        std::fs::write(
            root.join("tracked.txt"),
            "  \tneedle_match here\r\nsecond NEEDLE_MATCH\n",
        )
        .unwrap();
        std::fs::write(root.join("deleted_unstaged.txt"), "gone\n").unwrap();
        std::fs::write(
            root.join("AGENTS.md"),
            "tracked but gitignored needle_match\n",
        )
        .unwrap();
        std::fs::create_dir_all(root.join(".devcouncil/claude-plugin/devcouncil/agents")).unwrap();
        std::fs::create_dir_all(root.join(".devcouncil/dcgrep")).unwrap();
        std::fs::write(root.join(".devcouncil/config.yaml"), "dc: needle_match\n").unwrap();
        std::fs::write(
            root.join(".devcouncil/claude-plugin/devcouncil/agents/a.md"),
            "plugin needle_match\n",
        )
        .unwrap();
        std::fs::write(
            root.join(".devcouncil/ledger.sqlite"),
            "ignored sqlite needle_match\n",
        )
        .unwrap();
        std::fs::write(
            root.join(".devcouncil/dcgrep/index.bin"),
            "cache needle_match\n",
        )
        .unwrap();
        std::fs::create_dir_all(root.join("build")).unwrap();
        std::fs::write(
            root.join("build/artifact.txt"),
            "case ignored needle_match\n",
        )
        .unwrap();
        std::fs::write(root.join("-dash file.txt"), "    (a+)+ needle_match\n").unwrap();
        std::fs::write(root.join("cafe\u{0301}.txt"), "needle_match\n").unwrap();
        std::fs::write(
            root.join("secret.ignored"),
            "needle_match should stay ignored\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            symlink("tracked.txt", root.join("link.txt")).unwrap();
            symlink("nowhere.txt", root.join("dangling.txt")).unwrap();
        }
        git_in(
            root,
            &[
                "add",
                "-f",
                "--",
                ".gitignore",
                ".ignore",
                ".github/ci.yml",
                ".devcouncil/config.yaml",
                "tracked.txt",
                "deleted_unstaged.txt",
                "AGENTS.md",
                "-dash file.txt",
                "cafe\u{0301}.txt",
            ],
        );
        #[cfg(unix)]
        git_in(root, &["add", "link.txt", "dangling.txt"]);
        git_in(root, &["commit", "-m", "fixture"]);

        // Deleted unstaged tracked file stays in `list_repo_files` via .git/index.
        std::fs::remove_file(root.join("deleted_unstaged.txt")).unwrap();
        // Untracked non-ignored + untracked gitignored.
        std::fs::write(
            root.join("untracked note.txt"),
            "\tneedle_match untracked\n",
        )
        .unwrap();
        std::fs::write(root.join("extra.ignored"), "needle_match\n").unwrap();

        // 2. Linked worktree with $GIT_COMMON_DIR/info/exclude.
        let wt_parent = TempDir::new().unwrap();
        let wt_path = wt_parent.path().join("wt");
        git_in(
            root,
            &[
                "worktree",
                "add",
                "-b",
                "wt-branch",
                wt_path.to_str().unwrap(),
            ],
        );
        crate::test_support::trust_repo(&wt_path);
        std::fs::create_dir_all(root.join(".git/info")).unwrap();
        std::fs::write(root.join(".git/info/exclude"), "*.wt_excluded\n").unwrap();
        std::fs::write(wt_path.join("wt_untracked.txt"), "  needle_match wt\n").unwrap();
        std::fs::write(wt_path.join("drop.wt_excluded"), "needle_match wt\n").unwrap();

        let root_str = root.to_string_lossy().into_owned();
        let wt_str = wt_path.to_string_lossy().into_owned();

        // Collect git fallback baseline first.
        let (
            git_files,
            git_wt_files,
            git_regex,
            git_tracked_ignored,
            git_literal,
            git_ws,
            git_wt_search,
        ) = {
            let _absent = TestDcgrepBinding::bind_absent_binary();
            (
                GitReader::list_repo_files(&root_str).unwrap(),
                GitReader::list_repo_files(&wt_str).unwrap(),
                content_search::search(
                    &root_str,
                    "needle_match",
                    &ContentSearchOptions {
                        ignore_case: true,
                        ..Default::default()
                    },
                    &|| false,
                )
                .unwrap(),
                content_search::search(
                    &root_str,
                    "tracked but gitignored",
                    &ContentSearchOptions::default(),
                    &|| false,
                )
                .unwrap(),
                content_search::search(
                    &root_str,
                    "(a+)+",
                    &ContentSearchOptions {
                        fixed_strings: true,
                        ..Default::default()
                    },
                    &|| false,
                )
                .unwrap(),
                content_search::search(
                    &root_str,
                    "    ",
                    &ContentSearchOptions {
                        fixed_strings: true,
                        ..Default::default()
                    },
                    &|| false,
                )
                .unwrap(),
                content_search::search(
                    &wt_str,
                    "needle_match wt",
                    &ContentSearchOptions {
                        fixed_strings: true,
                        ..Default::default()
                    },
                    &|| false,
                )
                .unwrap(),
            )
        };

        assert!(git_files.contains(&"AGENTS.md".to_string()));
        assert!(git_files.contains(&".devcouncil/config.yaml".to_string()));
        assert!(git_files.contains(&".devcouncil/claude-plugin/devcouncil/agents/a.md".to_string()));
        assert!(!git_files.contains(&".devcouncil/ledger.sqlite".to_string()));
        assert!(!git_files.contains(&".devcouncil/dcgrep/index.bin".to_string()));
        assert!(!git_files.contains(&"build/artifact.txt".to_string()));
        assert!(git_files.contains(&"deleted_unstaged.txt".to_string()));
        assert_eq!(git_tracked_ignored.matches.len(), 1);
        assert_eq!(git_tracked_ignored.matches[0].path, "AGENTS.md");
        assert!(!git_wt_files.contains(&"drop.wt_excluded".to_string()));
        assert!(git_wt_files.contains(&"wt_untracked.txt".to_string()));

        if let Some(bin) = host_bin.as_ref() {
            let _dc = TestDcgrepBinding::bind_test_binary(Path::new(bin));
            let dc_files = GitReader::list_repo_files(&root_str).unwrap();
            let dc_wt_files = GitReader::list_repo_files(&wt_str).unwrap();
            let dc_regex = content_search::search(
                &root_str,
                "needle_match",
                &ContentSearchOptions {
                    ignore_case: true,
                    ..Default::default()
                },
                &|| false,
            )
            .unwrap();
            let dc_tracked_ignored = content_search::search(
                &root_str,
                "tracked but gitignored",
                &ContentSearchOptions::default(),
                &|| false,
            )
            .unwrap();
            let dc_literal = content_search::search(
                &root_str,
                "(a+)+",
                &ContentSearchOptions {
                    fixed_strings: true,
                    ..Default::default()
                },
                &|| false,
            )
            .unwrap();
            let dc_ws = content_search::search(
                &root_str,
                "    ",
                &ContentSearchOptions {
                    fixed_strings: true,
                    ..Default::default()
                },
                &|| false,
            )
            .unwrap();
            let dc_wt_search = content_search::search(
                &wt_str,
                "needle_match wt",
                &ContentSearchOptions {
                    fixed_strings: true,
                    ..Default::default()
                },
                &|| false,
            )
            .unwrap();

            assert_eq!(dc_files, git_files);
            assert_eq!(dc_wt_files, git_wt_files);
            assert_eq!(dc_regex, git_regex);
            assert_eq!(dc_tracked_ignored, git_tracked_ignored);
            assert_eq!(dc_literal, git_literal);
            assert_eq!(dc_ws, git_ws);
            assert_eq!(dc_wt_search, git_wt_search);
        }

        // 3. Untracked nested Git repository and nested `.worktrees/*` checkout must NOT be descended.
        let nested_repo = root.join("nested_repo");
        std::fs::create_dir_all(&nested_repo).unwrap();
        git_in(&nested_repo, &["init", "-b", "main"]);
        std::fs::write(nested_repo.join("secret.rs"), "nested needle_match\n").unwrap();

        let nested_wt = root.join(".worktrees/wt_nested");
        std::fs::create_dir_all(root.join(".worktrees")).unwrap();
        git_in(
            root,
            &[
                "worktree",
                "add",
                "-b",
                "nested-wt-branch",
                nested_wt.to_str().unwrap(),
            ],
        );
        crate::test_support::trust_repo(&nested_wt);
        std::fs::write(nested_wt.join("secret.rs"), "nested wt needle_match\n").unwrap();

        let git_nested_search = {
            let _absent = TestDcgrepBinding::bind_absent_binary();
            content_search::search(
                &root_str,
                "needle_match",
                &ContentSearchOptions::default(),
                &|| false,
            )
            .unwrap()
        };
        assert!(
            git_nested_search
                .matches
                .iter()
                .all(|m| !m.path.contains("secret.rs")),
            "git grep descended into nested git dir: {git_nested_search:?}"
        );
        if let Some(bin) = host_bin.as_ref() {
            let _dc = TestDcgrepBinding::bind_test_binary(Path::new(bin));
            let dc_nested_search = content_search::search(
                &root_str,
                "needle_match",
                &ContentSearchOptions::default(),
                &|| false,
            )
            .unwrap();
            let dc_nested_files = GitReader::list_repo_files(&root_str).unwrap();
            assert_eq!(dc_nested_search, git_nested_search);
            assert!(
                dc_nested_files.iter().all(|p| !p.contains("secret.rs")),
                "dcgrep descended into nested git dir: {dc_nested_files:?}"
            );
        }
        git_in(
            root,
            &["worktree", "remove", "--force", nested_wt.to_str().unwrap()],
        );

        git_in(
            root,
            &["worktree", "remove", "--force", wt_path.to_str().unwrap()],
        );
    }

    #[test]
    fn capability_check_rejects_old_or_incomplete_dcgrep_and_falls_back_to_git() {
        let dir = init_repo();
        std::fs::write(dir.path().join("hit.txt"), "needle\n").unwrap();
        git_in(dir.path(), &["add", "hit.txt"]);
        git_in(dir.path(), &["commit", "-m", "init"]);

        let stub_dir = TempDir::new().unwrap();
        let stub = write_exe(
            stub_dir.path(),
            "dcgrep",
            "#!/bin/sh\nif [ \"$1\" = \"health\" ]; then\n  echo '{\"ok\":true,\"searcher\":\"dc-grep\",\"schema_version\":1,\"capabilities\":{\"fixed_strings\":true,\"include_hidden\":false,\"preserve_whitespace\":true,\"include_oversized\":true,\"include_symlinks\":true,\"precompose_unicode\":true}}'\n  exit 0\nfi\nexit 9\n",
        );

        let _binding = TestDcgrepBinding::bind_test_binary(&stub);
        let root_str = dir.path().to_string_lossy().into_owned();
        let canon = process::canonicalize_plain(dir.path()).unwrap();
        let _ = git_directories(&canon);
        let before = process::spawn_log::spawns_in(&canon).len();

        let files = GitReader::list_repo_files(&root_str).unwrap();
        let search = content_search::search(
            &root_str,
            "needle",
            &ContentSearchOptions::default(),
            &|| false,
        )
        .unwrap();

        let spawns: Vec<_> = process::spawn_log::spawns_in(&canon)
            .into_iter()
            .skip(before)
            .collect();
        assert_eq!(files, vec!["hit.txt"]);
        assert_eq!(search.matches.len(), 1);
        assert!(spawns.iter().any(|a| a.iter().any(|s| s == "ls-files")));
        assert!(spawns.iter().any(|a| a.iter().any(|s| s == "grep")));
    }

    /// Listing is `git ls-files` because it measured 2.2-2.5x faster than
    /// `dcgrep files` (scripts/bench-dcgrep.mjs). A healthy dcgrep that would
    /// answer `files` with a wrong list, and record that it was asked, must be
    /// neither asked nor believed.
    #[test]
    fn list_repo_files_never_asks_dcgrep() {
        let dir = init_repo();
        std::fs::write(dir.path().join("a.txt"), "x\n").unwrap();
        git_in(dir.path(), &["add", "a.txt"]);
        git_in(dir.path(), &["commit", "-m", "init"]);
        let root_str = dir.path().to_string_lossy().into_owned();

        let stub_dir = TempDir::new().unwrap();
        let marker = stub_dir.path().join("files-asked");
        let script = format!(
            "#!/bin/sh\nif [ \"$1\" = \"health\" ]; then\n  echo '{{\"ok\":true,\"id\":\"dcgrep\",\"component\":\"dc-grep\",\"version\":\"0.2.5\",\"searcher\":\"dc-grep\",\"schema_version\":1,\"capabilities\":{{\"fixed_strings\":true,\"include_hidden\":true,\"preserve_whitespace\":true,\"include_oversized\":true,\"include_symlinks\":true,\"precompose_unicode\":true,\"include_devcouncil\":true,\"ignore_dot_ignore\":true,\"include_git_tracked\":true}}}}'\n  exit 0\nfi\ntouch '{}'\necho '{{\"ok\":true,\"count\":1,\"paths\":[\"bogus-from-dcgrep.txt\"],\"truncated\":false,\"skipped\":{{}}}}'\n",
            marker.display()
        );
        let stub = write_exe(stub_dir.path(), "dcgrep", &script);
        let _binding = TestDcgrepBinding::bind_test_binary(&stub);

        let files = GitReader::list_repo_files(&root_str).expect("list_repo_files");
        assert!(!marker.exists(), "list_repo_files invoked `dcgrep files`");
        assert_eq!(files, vec!["a.txt"]);
    }

    #[test]
    fn explicit_gitpulse_dcgrep_bin_fails_closed_when_missing_or_non_executable() {
        let dir = init_repo();
        std::fs::write(dir.path().join("a.txt"), "needle\n").unwrap();
        git_in(dir.path(), &["add", "a.txt"]);
        git_in(dir.path(), &["commit", "-m", "init"]);

        let missing = dir.path().join("no-such-dcgrep");
        let root_str = dir.path().to_string_lossy().into_owned();
        let serial = env_serial();
        let _env = bind_env(&serial).set(GITPULSE_DCGREP_ENV, missing.as_os_str());
        invalidate_cache();

        assert_eq!(
            GitReader::list_repo_files(&root_str).unwrap(),
            vec!["a.txt"]
        );

        let search_err = content_search::search(
            &root_str,
            "needle",
            &ContentSearchOptions::default(),
            &|| false,
        )
        .expect_err("must fail closed");
        assert!(
            search_err.message().contains(GITPULSE_DCGREP_ENV),
            "{search_err:?}"
        );
    }

    #[test]
    fn revision_custom_config_and_split_index_fall_back_to_git() {
        let _serial = env_serial();
        if resolve_usable_binary().ok().flatten().is_none() {
            return;
        }
        let dir = init_repo();
        let root = dir.path();
        std::fs::write(root.join("a.txt"), "from_commit\n").unwrap();
        git_in(root, &["add", "a.txt"]);
        git_in(root, &["commit", "-m", "commit one"]);
        std::fs::write(root.join("a.txt"), "working_only\n").unwrap();

        let root_str = root.to_string_lossy().into_owned();
        let canon = process::canonicalize_plain(root).unwrap();
        let _ = git_directories(&canon);

        // Revision searches always run git grep directly.
        let before = process::spawn_log::spawns_in(&canon).len();
        let rev_report = content_search::search(
            &root_str,
            "from_commit",
            &ContentSearchOptions {
                revision: Some("HEAD".to_string()),
                ..Default::default()
            },
            &|| false,
        )
        .unwrap();
        assert_eq!(rev_report.matches.len(), 1);
        let spawns: Vec<_> = process::spawn_log::spawns_in(&canon)
            .into_iter()
            .skip(before)
            .collect();
        assert!(spawns.iter().any(|a| a.iter().any(|s| s == "grep")));

        // Custom core.excludesFile falls back cleanly to git for both search and list.
        let ignore_file = root.join("custom.ignore");
        std::fs::write(&ignore_file, "skip_me.txt\n").unwrap();
        std::fs::write(root.join("skip_me.txt"), "working_only\n").unwrap();
        git_in(
            root,
            &["config", "core.excludesFile", ignore_file.to_str().unwrap()],
        );
        let files = GitReader::list_repo_files(&root_str).unwrap();
        assert!(!files.contains(&"skip_me.txt".to_string()));
        let wt_search = content_search::search(
            &root_str,
            "working_only",
            &ContentSearchOptions::default(),
            &|| false,
        )
        .unwrap();
        assert_eq!(wt_search.matches.len(), 1);
        assert_eq!(wt_search.matches[0].path, "a.txt");

        // Split index falls back cleanly to git ls-files.
        git_in(root, &["config", "--unset", "core.excludesFile"]);
        git_in(root, &["update-index", "--split-index"]);
        let split_files = GitReader::list_repo_files(&root_str).unwrap();
        assert!(split_files.contains(&"a.txt".to_string()));

        // Whitespace-only regex falls back cleanly to git grep instead of hitting dcgrep's empty-regex refusal.
        let ws_regex =
            content_search::search(&root_str, "   ", &ContentSearchOptions::default(), &|| {
                false
            })
            .unwrap();
        assert_eq!(ws_regex.matches.len(), 0);

        // .gitmodules falls back cleanly to git.
        std::fs::write(root.join(".gitmodules"), "").unwrap();
        assert!(has_unsupported_repo_layout(root).is_none());
    }

    #[test]
    fn cancellation_truncation_exact_limit_and_invalid_regex_behavior() {
        let _serial = env_serial();
        if resolve_usable_binary().ok().flatten().is_none() {
            return;
        }
        let dir = init_repo();
        let root = dir.path();
        std::fs::write(root.join("lines.txt"), "hit1\nhit2\nhit3\n").unwrap();
        git_in(root, &["add", "lines.txt"]);
        git_in(root, &["commit", "-m", "hits"]);
        let root_str = root.to_string_lossy().into_owned();

        // Pre-cancelled query never returns non-truncated results.
        let cancelled =
            content_search::search(&root_str, "hit", &ContentSearchOptions::default(), &|| true)
                .unwrap();
        assert!(cancelled.truncated);
        assert_eq!(cancelled.truncated_reason.as_deref(), Some("cancelled"));

        // Match limit vs exact fit.
        let under = content_search::search(
            &root_str,
            "hit",
            &ContentSearchOptions {
                max_matches: Some(2),
                ..Default::default()
            },
            &|| false,
        )
        .unwrap();
        assert_eq!(under.matches.len(), 2);
        assert!(under.truncated);
        assert_eq!(under.truncated_reason.as_deref(), Some("match_limit"));

        let exact = content_search::search(
            &root_str,
            "hit",
            &ContentSearchOptions {
                max_matches: Some(3),
                ..Default::default()
            },
            &|| false,
        )
        .unwrap();
        assert_eq!(exact.matches.len(), 3);
        assert!(!exact.truncated);
        assert_eq!(exact.truncated_reason, None);

        // Invalid regex produces an error naming Search failed.
        let bad = content_search::search(
            &root_str,
            "[unclosed",
            &ContentSearchOptions::default(),
            &|| false,
        )
        .expect_err("invalid regex must error");
        assert!(bad.message().contains("Search failed"), "{bad:?}");
    }

    #[test]
    fn wide_line_column_falls_back_instead_of_fabricating_one_and_crlf_end_anchor_matches() {
        let _serial = env_serial();
        let Some(host_bin) = resolve_usable_binary().ok().flatten() else {
            return;
        };
        let _dc = TestDcgrepBinding::bind_test_binary(Path::new(&host_bin));

        let dir = init_repo();
        let root = dir.path();
        let wide_line = format!("{}needle\n", "x".repeat(18_000));
        std::fs::write(root.join("wide.txt"), &wide_line).unwrap();
        std::fs::write(
            root.join("wide_end.txt"),
            format!("{}a\n", "a".repeat(5_000)),
        )
        .unwrap();
        std::fs::write(root.join("crlf.txt"), "alpha endtok\r\nendtok beta\r\n").unwrap();
        git_in(root, &["add", "wide.txt", "wide_end.txt", "crlf.txt"]);
        git_in(root, &["commit", "-m", "wide and crlf"]);
        let root_str = root.to_string_lossy().into_owned();

        // Direct dcgrep call refuses to invent `column: 1` when match sits past max_line_bytes.
        let direct = search_working_tree(
            root,
            "needle",
            &ContentSearchOptions::default(),
            500,
            &|| false,
        )
        .unwrap();
        assert!(
            direct.is_none(),
            "wide match beyond clip must fall back cleanly, got {direct:?}"
        );

        // End-to-end content search falls back cleanly to `git grep` and reports real column 18001.
        let end_to_end = content_search::search(
            &root_str,
            "needle",
            &ContentSearchOptions::default(),
            &|| false,
        )
        .unwrap();
        assert_eq!(end_to_end.matches.len(), 1);
        assert_eq!(end_to_end.matches[0].column, 18_001);

        // Truncated line matching unescaped `$` must also fall back cleanly rather than matching at clip.
        let clip_dollar =
            search_working_tree(root, "a$", &ContentSearchOptions::default(), 500, &|| false)
                .unwrap();
        assert!(clip_dollar.is_none());
        let real_end_col =
            content_search::search(&root_str, "a$", &ContentSearchOptions::default(), &|| false)
                .unwrap();
        assert_eq!(real_end_col.matches.len(), 1);
        assert_eq!(real_end_col.matches[0].column, 5_001);

        // Short CRLF `$` anchor matches via dcgrep accurately without falling back.
        let crlf_res = search_working_tree(
            root,
            "endtok$",
            &ContentSearchOptions::default(),
            500,
            &|| false,
        )
        .unwrap()
        .expect("dcgrep must handle CRLF $ anchor directly");
        assert_eq!(crlf_res.matches.len(), 1);
        assert_eq!(crlf_res.matches[0].path, "crlf.txt");
        assert_eq!(crlf_res.matches[0].line, 1);
        assert_eq!(crlf_res.matches[0].column, 7);
    }

    #[test]
    fn max_matches_ceiling_with_long_lines_does_not_hit_output_cap() {
        let _serial = env_serial();
        let Some(host_bin) = resolve_usable_binary().ok().flatten() else {
            return;
        };
        let _dc = TestDcgrepBinding::bind_test_binary(Path::new(&host_bin));

        let dir = init_repo();
        let root = dir.path();
        let line = format!("needle_5k {}\n", "z".repeat(4500));
        let mut body = String::with_capacity(line.len() * 5000);
        for _ in 0..5000 {
            body.push_str(&line);
        }
        std::fs::write(root.join("many.txt"), body).unwrap();
        git_in(root, &["add", "many.txt"]);
        git_in(root, &["commit", "-m", "5k matches"]);

        let report = search_working_tree(
            root,
            "needle_5k",
            &ContentSearchOptions {
                fixed_strings: true,
                ..Default::default()
            },
            crate::engine::content_search::MAX_MATCHES_CEILING,
            &|| false,
        )
        .unwrap()
        .expect("dcgrep must return 5000 hits without OverCap false-empty");
        assert_eq!(report.matches.len(), 5000);
        assert!(!report.truncated);
        assert_eq!(report.truncated_reason, None);
    }

    #[test]
    fn explicit_gitpulse_dcgrep_bin_fails_closed_when_health_fails_and_locator_shared() {
        let dir = init_repo();
        std::fs::write(dir.path().join("a.txt"), "needle\n").unwrap();
        git_in(dir.path(), &["add", "a.txt"]);
        git_in(dir.path(), &["commit", "-m", "init"]);
        let root_str = dir.path().to_string_lossy().into_owned();

        let stub_dir = TempDir::new().unwrap();
        let bad_exe = write_exe(
            stub_dir.path(),
            "bad-dcgrep",
            "#!/bin/sh\necho '{\"ok\":false}'\nexit 1\n",
        );

        struct InvalidateOnDrop;
        impl Drop for InvalidateOnDrop {
            fn drop(&mut self) {
                crate::tool_capability::invalidate_all();
            }
        }
        let serial = env_serial();
        let _inv = InvalidateOnDrop;
        let _env = bind_env(&serial).set(GITPULSE_DCGREP_ENV, bad_exe.as_os_str());
        crate::tool_capability::invalidate_all();

        // Inventory locator shares GITPULSE_DCGREP_BIN precedence.
        assert_eq!(
            locate_binary_for_inventory().as_deref(),
            Some(bad_exe.to_string_lossy().as_ref())
        );

        // Failing `health` must fail closed naming GITPULSE_DCGREP_BIN in the search entry
        // point; listing is git's and never consults dcgrep.
        assert_eq!(
            GitReader::list_repo_files(&root_str).unwrap(),
            vec!["a.txt"]
        );

        let search_err = content_search::search(
            &root_str,
            "needle",
            &ContentSearchOptions::default(),
            &|| false,
        )
        .expect_err("must fail closed");
        assert!(
            search_err.message().contains(GITPULSE_DCGREP_ENV),
            "{search_err:?}"
        );

        // `tool_capability::invalidate_all()` and `suite_status()` clear `dcgrep` caches so a
        // later fixed binary at the same path succeeds immediately without waiting for TTL.
        std::fs::write(
            &bad_exe,
            "#!/bin/sh\nif [ \"$1\" = \"health\" ]; then\n  echo '{\"ok\":true,\"id\":\"dcgrep\",\"component\":\"dc-grep\",\"version\":\"0.2.5\",\"searcher\":\"dc-grep\",\"schema_version\":1,\"capabilities\":{\"fixed_strings\":true,\"include_hidden\":true,\"preserve_whitespace\":true,\"include_oversized\":true,\"include_symlinks\":true,\"precompose_unicode\":true,\"include_devcouncil\":true,\"ignore_dot_ignore\":true,\"include_git_tracked\":true}}'\n  exit 0\nfi\necho '{\"ok\":true,\"count\":0,\"paths\":[],\"matches\":[],\"skipped\":{}}'\n",
        )
        .unwrap();
        // Before invalidation, cached false fails closed; after suite_status(), probe reruns!
        let _ = crate::tool_install::components::suite_status();
        let files_after = GitReader::list_repo_files(&root_str).unwrap();
        assert_eq!(files_after, vec!["a.txt"]);
    }

    #[test]
    fn cancellation_during_health_probe_returns_cancelled_report_without_caching() {
        let dir = init_repo();
        std::fs::write(dir.path().join("a.txt"), "needle\n").unwrap();
        git_in(dir.path(), &["add", "a.txt"]);
        git_in(dir.path(), &["commit", "-m", "init"]);

        let stub_dir = TempDir::new().unwrap();
        let slow_health = health_stub(stub_dir.path(), FULL_HEALTH, 1, None);

        let _binding = TestDcgrepBinding::bind_test_binary(&slow_health);
        let calls = std::sync::atomic::AtomicUsize::new(0);
        // First call is uncancelled at entry, then cancels while `dcgrep health` runs!
        let report = search_working_tree(
            dir.path(),
            "needle",
            &ContentSearchOptions::default(),
            100,
            &|| calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) >= 2,
        )
        .unwrap()
        .expect("cancellation during health probe returns cancelled report");
        assert!(report.truncated);
        assert_eq!(report.truncated_reason.as_deref(), Some("cancelled"));

        // Cancelled probe must NOT be cached as unsupported: next uncancelled probe succeeds.
        assert_eq!(
            probe_capabilities(slow_health.to_str().unwrap(), &|| false)
                .ok()
                .map(|o| matches!(o, HealthProbeOutcome::Supported)),
            Some(true)
        );
    }

    #[test]
    fn cancellation_while_queued_at_saturated_spawn_gate_during_health_probe() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        static SATURATED_GATE: std::sync::LazyLock<process::SpawnGate> =
            std::sync::LazyLock::new(|| process::SpawnGate::new(1));

        struct ReadyObserver {
            ready: Arc<AtomicBool>,
        }
        impl ProcessObserver for ReadyObserver {
            fn cancelled(&self) -> bool {
                false
            }
            fn output(&mut self, _stream: OutputStream, bytes: &[u8]) {
                if bytes.windows(5).any(|w| w == b"READY") {
                    self.ready.store(true, Ordering::Release);
                }
            }
        }

        let dir = init_repo();
        std::fs::write(dir.path().join("a.txt"), "needle\n").unwrap();
        git_in(dir.path(), &["add", "a.txt"]);
        git_in(dir.path(), &["commit", "-m", "init"]);

        let stub_dir = TempDir::new().unwrap();
        let marker = stub_dir.path().join("health_ran");
        let stub_exe = health_stub(stub_dir.path(), FULL_HEALTH, 0, Some(&marker));

        struct ReleaseHolder(std::path::PathBuf);
        impl Drop for ReleaseHolder {
            fn drop(&mut self) {
                let _ = std::fs::write(&self.0, b"go");
            }
        }
        let release_path = stub_dir.path().join("release-holder");
        let release_guard = ReleaseHolder(release_path.clone());
        let holder_script = format!(
            "echo READY; while [ ! -f '{}' ]; do sleep 0.02; done",
            release_path.display()
        );
        let slot_held = Arc::new(AtomicBool::new(false));
        let slot_held_bg = Arc::clone(&slot_held);
        let holder_dir = stub_dir.path().to_path_buf();
        let holder = std::thread::spawn(move || {
            let mut cmd = process::build_capture_command(
                "sh",
                &["-c", &holder_script],
                Some(&holder_dir),
                &[],
                None,
                None,
            );
            let mut obs = ReadyObserver {
                ready: slot_held_bg,
            };
            let _ = process::run_with_gate(
                &mut cmd,
                "gate blocker",
                Duration::from_secs(5),
                None,
                1024,
                &mut obs,
                &SATURATED_GATE,
            );
        });

        assert!(
            crate::test_support::wait_until(Duration::from_secs(5), || {
                slot_held.load(Ordering::Acquire)
            }),
            "gate blocker did not acquire SpawnGate slot in time"
        );

        let _binding = TestDcgrepBinding::bind_test_binary(&stub_exe);
        invalidate_cache();

        let cancel_flag = Arc::new(AtomicBool::new(false));
        let cancel_flag_bg = Arc::clone(&cancel_flag);
        let canceller = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(45));
            cancel_flag_bg.store(true, Ordering::Release);
        });

        let wait_before = Instant::now();
        let result = search_working_tree_on_gate(
            dir.path(),
            "needle",
            &ContentSearchOptions::default(),
            100,
            &|| cancel_flag.load(Ordering::Acquire),
            &SATURATED_GATE,
        );
        let waited = wait_before.elapsed();
        canceller.join().unwrap();
        drop(release_guard);
        holder.join().unwrap();

        let report = result
            .unwrap()
            .expect("queued gate cancellation returns cancelled report");
        assert!(report.truncated);
        assert_eq!(report.truncated_reason.as_deref(), Some("cancelled"));
        assert!(
            waited >= Duration::from_millis(25),
            "expected probe to wait on saturated SpawnGate before cancelling, waited {waited:?}"
        );
        assert!(
            !marker.exists(),
            "dcgrep health must not spawn while queued and cancelled at SpawnGate"
        );

        // Probe must NOT be negatively cached when cancelled at the SpawnGate.
        assert_eq!(
            probe_capabilities_on_gate(stub_exe.to_str().unwrap(), &|| false, &SATURATED_GATE)
                .ok()
                .map(|o| matches!(o, HealthProbeOutcome::Supported)),
            Some(true)
        );
    }

    #[test]
    fn binary_locator_precedence_and_inventory_parity() {
        let _serial = env_serial();
        let temp_home = TempDir::new().unwrap();
        let path_dir = temp_home.path().join("path_bin");
        let explicit_dir = temp_home.path().join("explicit_bin");
        let manvi_dir = temp_home.path().join("manvi_bin");
        std::fs::create_dir_all(&path_dir).unwrap();
        std::fs::create_dir_all(&explicit_dir).unwrap();
        std::fs::create_dir_all(&manvi_dir).unwrap();

        let path_bin = write_exe(&path_dir, DCGREP_BIN_NAME, "#!/bin/sh\nexit 0\n");
        let manvi_custom_bare = write_exe(&path_dir, "my-custom-dcgrep", "#!/bin/sh\nexit 0\n");
        let manvi_explicit = write_exe(&manvi_dir, "manvi-dcgrep", "#!/bin/sh\nexit 0\n");
        let gitpulse_explicit = write_exe(&explicit_dir, DCGREP_BIN_NAME, "#!/bin/sh\nexit 0\n");

        let joined_path = {
            let mut entries = vec![path_dir.clone()];
            if let Some(existing) = std::env::var_os("PATH") {
                entries.extend(std::env::split_paths(&existing));
            }
            std::env::join_paths(entries).unwrap()
        };

        // 1. Lowest tier: PATH (with HOME redirected so host app_bin_dir doesn't shadow PATH).
        {
            let _env = bind_env(&_serial)
                .remove(GITPULSE_DCGREP_ENV)
                .remove(MANVI_GREP_ENV)
                .set("HOME", temp_home.path().as_os_str())
                .set("XDG_DATA_HOME", temp_home.path().as_os_str())
                .set("PATH", &joined_path)
                .invalidating(crate::tool_capability::invalidate_all);

            let resolved = resolve_candidate_binary().unwrap().map(|(p, _)| p);
            let app_dir_moved = app_bin_dir_path()
                .map(|d| d.starts_with(temp_home.path()))
                .unwrap_or(false);
            if app_dir_moved {
                assert_eq!(resolved.as_deref(), Some(path_bin.to_str().unwrap()));
                assert_eq!(locate_binary_for_inventory(), resolved);

                // 2. app_bin_dir_path() beats PATH when no env overrides are set.
                let app_dir = app_bin_dir_path().unwrap();
                std::fs::create_dir_all(&app_dir).unwrap();
                let bundled_bin = write_exe(&app_dir, DCGREP_BIN_NAME, "#!/bin/sh\nexit 0\n");
                crate::tool_capability::invalidate_all();
                let resolved_app = resolve_candidate_binary().unwrap().map(|(p, _)| p);
                assert_eq!(resolved_app.as_deref(), Some(bundled_bin.to_str().unwrap()));
                assert_eq!(locate_binary_for_inventory(), resolved_app);
            }
        }

        // 3. MANVI_GREP_BINARY (bare name on PATH & explicit path) beats app_bin_dir and PATH.
        {
            let _env = bind_env(&_serial)
                .remove(GITPULSE_DCGREP_ENV)
                .set(MANVI_GREP_ENV, "my-custom-dcgrep")
                .set("PATH", &joined_path)
                .invalidating(crate::tool_capability::invalidate_all);
            let resolved = resolve_candidate_binary().unwrap().map(|(p, _)| p);
            assert_eq!(
                resolved.as_deref(),
                Some(manvi_custom_bare.to_str().unwrap())
            );
            assert_eq!(locate_binary_for_inventory(), resolved);
        }

        // 4. GITPULSE_DCGREP_BIN beats MANVI_GREP_BINARY, app_bin_dir, and PATH.
        {
            let _env = bind_env(&_serial)
                .set(GITPULSE_DCGREP_ENV, gitpulse_explicit.as_os_str())
                .set(MANVI_GREP_ENV, manvi_explicit.as_os_str())
                .set("PATH", &joined_path)
                .invalidating(crate::tool_capability::invalidate_all);
            let resolved = resolve_candidate_binary().unwrap();
            assert_eq!(
                resolved,
                Some((gitpulse_explicit.to_string_lossy().into_owned(), true))
            );
            assert_eq!(
                locate_binary_for_inventory().as_deref(),
                Some(gitpulse_explicit.to_str().unwrap())
            );
        }
    }

    #[test]
    fn wide_line_with_z_anchors_falls_back_cleanly_while_escaped_z_uses_dcgrep() {
        assert!(pattern_has_unescaped_end_anchor(r"needle\z"));
        assert!(pattern_has_unescaped_end_anchor(r"needle\Z"));
        assert!(pattern_has_unescaped_end_anchor(r"needle\\\z"));
        assert!(!pattern_has_unescaped_end_anchor(r"needle\\z"));

        let _serial = env_serial();
        let Some(_) = resolve_usable_binary().ok().flatten() else {
            return;
        };
        let dir = init_repo();
        let root = dir.path();
        let padding = "x".repeat(DCGREP_MAX_LINE_BYTES + 128);
        std::fs::write(root.join("wide.txt"), format!("needle_z{padding}")).unwrap();
        git_in(root, &["add", "wide.txt"]);
        git_in(root, &["commit", "-m", "init"]);

        // `max_line_bytes` clipped wide line with `\z` must return Ok(None) fallback
        // rather than accepting a clipped-end `\z` match.
        let res = search_working_tree(
            root,
            r"needle_z.*\z",
            &ContentSearchOptions::default(),
            100,
            &|| false,
        )
        .unwrap();
        assert!(
            res.is_none(),
            "expected fallback (Ok(None)) for needle_z.*\\z on clipped wide line, got: {res:?}"
        );

        // Literal `\z` with double backslash `\\z` is NOT an end anchor and works on wide line.
        std::fs::write(root.join("escaped.txt"), format!("needle\\z{padding}")).unwrap();
        git_in(root, &["add", "escaped.txt"]);
        git_in(root, &["commit", "-m", "add escaped"]);
        let report = search_working_tree(
            root,
            r"needle\\z",
            &ContentSearchOptions::default(),
            100,
            &|| false,
        )
        .unwrap()
        .expect("dcgrep should handle non-anchor pattern even on wide line");
        assert_eq!(report.matches.len(), 1);
        assert_eq!(report.matches[0].path, "escaped.txt");
        assert_eq!(report.matches[0].column, 1);
        assert!(report.matches[0].text_clipped);
    }

    #[cfg(unix)]
    #[test]
    fn chmod_zero_on_tracked_ignored_file_falls_back_to_git_grep() {
        use std::os::unix::fs::PermissionsExt;

        let _serial = env_serial();
        let Some(_) = resolve_usable_binary().ok().flatten() else {
            return;
        };
        let dir = init_repo();
        let root = dir.path();
        std::fs::write(root.join(".gitignore"), "secret.txt\n").unwrap();
        std::fs::write(root.join("secret.txt"), "needle hidden\n").unwrap();
        std::fs::write(root.join("visible.txt"), "needle visible\n").unwrap();
        git_in(root, &["add", ".gitignore", "visible.txt"]);
        git_in(root, &["add", "-f", "secret.txt"]);
        git_in(root, &["commit", "-m", "init"]);

        let secret = root.join("secret.txt");
        let orig_mode = std::fs::metadata(&secret).unwrap().permissions().mode();
        std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o000)).unwrap();
        let blocked = std::fs::read(&secret).is_err();

        let dcgrep_res = search_working_tree(
            root,
            "needle",
            &ContentSearchOptions::default(),
            100,
            &|| false,
        )
        .unwrap();
        let root_str = root.to_string_lossy().into_owned();
        let search_res = content_search::search(
            &root_str,
            "needle",
            &ContentSearchOptions::default(),
            &|| false,
        );

        let _ = std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(orig_mode));

        if blocked {
            assert!(
                dcgrep_res.is_none(),
                "unreadable tracked-ignored file must trigger clean fallback instead of partial complete result: {dcgrep_res:?}"
            );
            let err = search_res.expect_err(
                "git fallback must also fail closed on unreadable tracked-ignored file",
            );
            assert!(
                err.message().contains("Permission denied")
                    || err.message().contains("Search failed"),
                "unexpected fallback error: {err:?}"
            );
        }
    }
}
