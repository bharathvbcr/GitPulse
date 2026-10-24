//! Dependency health for the opened repository.
//!
//! GitPulse does not install packages or apply fixes. It inventories manifests,
//! flags lockfile / engine / install-script issues locally, and — when the
//! matching CLI is on PATH — runs read-only audits: `npm audit` for an npm
//! lockfile, `bun audit` for `bun.lock`, `npm outdated` when npm is on PATH,
//! `cargo audit`, `cargo deny --offline` when `deny.toml` is present,
//! `cargo crev verify` when cargo-crev is installed, `cargo audit bin` for
//! release binaries (the cargo-auditable embed), `pip-audit --no-deps`
//! (pinned requirements files only),
//! `govulncheck -json`, `composer audit --locked`, and `bundler-audit check`.
//! A missing CLI is reported as such — and a CLI that exists but fails to run
//! (wrong interpreter, non-zero exit, timeout) is reported with its real cause,
//! never mislabeled as missing. It is never treated as a clean bill of
//! health. Scanner advisory databases are never refreshed implicitly — a stale
//! DB is the user's call, not a background write outside the repo.

#[allow(unused_imports)]
use crate::analyzer::cargo_supply::{
    self, classify_bin_audit, crev_summary, file_contains_auditable_section,
    parse_cargo_crev_jsonl, parse_cargo_deny_sarif, BinAuditKind,
};
use crate::analyzer::language::LanguageDetector;
use crate::deadline::{ScanDeadline, AUDIT_TIMEOUT};
use crate::engine::git_cli::{
    capture_command_scrubbed, capture_command_with_env, classify_tool_probe, git_text,
    is_timed_out, resolve_spawn_program_in, sandbox_join, sandbox_join_canonical, validate_repo,
    CapturedOutput, PathSource, ToolProbe,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

const MAX_SOURCE_FILES: usize = 10_000;
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
const MAX_MANIFESTS: usize = 12;
const MAX_VULNS: usize = 200;
const MAX_OUTDATED: usize = 200;
const MAX_ISSUES: usize = 48;
const MAX_NPM_ROOTS: usize = 6;
const MAX_CARGO_LOCKS: usize = 6;
const MAX_DENY_CONFIGS: usize = 6;
const MAX_AUDITABLE_BINS: usize = 4;
const MAX_AUDITABLE_BIN_BYTES: u64 = 100 * 1024 * 1024;
const MAX_PY_REQUIREMENTS: usize = 6;
const MAX_GO_MODS: usize = 4;
const MAX_COMPOSER_LOCKS: usize = 6;
const MAX_ECOSYSTEM_MANIFESTS: usize = 24;
/// pip-audit / govulncheck / composer findings carry no CVSS-style severity.
/// They are counted in their own bucket instead of masquerading as "info".
pub(crate) const SEVERITY_UNKNOWN: &str = "unknown";
const VERSION_TIMEOUT: Duration = Duration::from_secs(5);

const NPM_SAFE_ENV: &[(&str, &str)] = &[
    ("npm_config_ignore_scripts", "true"),
    ("npm_config_fund", "false"),
    ("npm_config_update_notifier", "false"),
    ("npm_config_progress", "false"),
    ("npm_config_loglevel", "error"),
    ("CI", "1"),
];

const LIFECYCLE_SCRIPTS: &[&str] = &[
    "preinstall",
    "install",
    "postinstall",
    "preuninstall",
    "uninstall",
    "postuninstall",
    "prepublish",
    "prepublishOnly",
    "prepare",
    "prepack",
    "postpack",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthIssue {
    pub severity: String,
    pub code: String,
    pub message: String,
    pub path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EcosystemHint {
    pub family: String,
    pub manifests: Vec<String>,
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NpmManifest {
    pub path: String,
    pub name: String,
    pub version: String,
    pub private: bool,
    pub license: Option<String>,
    pub engines_node: Option<String>,
    pub package_manager: String,
    pub lockfile: Option<String>,
    /// Directory (repo-relative, empty for the root) that holds `lockfile`.
    /// Differs from this manifest's own directory for a workspace member that
    /// shares its workspace root's lockfile. `serde(default)` keeps older
    /// serialized reports loadable.
    #[serde(default)]
    pub lockfile_dir: Option<String>,
    pub has_workspaces: bool,
    pub dep_count: usize,
    pub dev_dep_count: usize,
    pub optional_dep_count: usize,
    pub peer_dep_count: usize,
    pub lifecycle_scripts: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Vulnerability {
    pub name: String,
    pub severity: String,
    pub is_direct: bool,
    pub title: String,
    pub url: String,
    pub range: String,
    pub fix_available: String,
    pub via: Vec<String>,
    pub ecosystem: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuditSummary {
    pub info: u32,
    pub low: u32,
    pub moderate: u32,
    pub high: u32,
    pub critical: u32,
    /// Findings whose source publishes no severity (pip-audit, govulncheck).
    /// Kept in their own bucket so an unrated finding can never deflate the
    /// total or masquerade as "informational".
    pub unknown: u32,
    pub total: u32,
}

impl AuditSummary {
    fn from_counts(
        info: u32,
        low: u32,
        moderate: u32,
        high: u32,
        critical: u32,
        unknown: u32,
    ) -> Self {
        Self {
            info,
            low,
            moderate,
            high,
            critical,
            unknown,
            total: info + low + moderate + high + critical + unknown,
        }
    }

    fn from_vulns(vulns: &[Vulnerability]) -> Self {
        let mut info = 0;
        let mut low = 0;
        let mut moderate = 0;
        let mut high = 0;
        let mut critical = 0;
        let mut unknown = 0;
        for v in vulns {
            match v.severity.as_str() {
                "critical" => critical += 1,
                "high" => high += 1,
                "moderate" | "medium" => moderate += 1,
                "low" => low += 1,
                SEVERITY_UNKNOWN => unknown += 1,
                _ => info += 1,
            }
        }
        Self::from_counts(info, low, moderate, high, critical, unknown)
    }
}

impl Default for AuditSummary {
    fn default() -> Self {
        Self::from_counts(0, 0, 0, 0, 0, 0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutdatedPackage {
    pub name: String,
    pub current: String,
    pub wanted: String,
    pub latest: String,
    pub dep_type: String,
    pub location: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanLimitNotice {
    pub resource: String,
    pub kept: usize,
    pub total: usize,
    /// Only inventory paths were withheld; audit targets were collected separately.
    /// Older notices default to coverage-affecting, never an assumed full audit.
    #[serde(default)]
    pub inventory_only: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DepsHealthReport {
    pub node_version: Option<String>,
    pub npm_version: Option<String>,
    pub npm_cli_present: bool,
    /// `bun` is on PATH and this checkout has a `bun.lock`. Absence is not a
    /// clean audit of that lockfile. `serde(default)` keeps older reports loadable.
    #[serde(default)]
    pub bun_cli_present: bool,
    pub cargo_audit_present: bool,
    /// `cargo-deny` is on PATH and this checkout has at least one `deny.toml`.
    /// Absence is not a clean policy check. `serde(default)` keeps older
    /// serialized reports loadable.
    #[serde(default)]
    pub cargo_deny_present: bool,
    /// `cargo-crev` is on PATH. Reviews are checked when it is; a checkout
    /// with no local web of trust still gets an explicit summary.
    #[serde(default)]
    pub cargo_crev_present: bool,
    pub pip_audit_present: bool,
    pub govulncheck_present: bool,
    pub composer_present: bool,
    pub bundler_audit_present: bool,
    /// Scanners that ran a real CLI command and produced output this scan
    /// could parse (`npm`, `bun`, `cargo`, `cargo-deny`, `cargo-crev`,
    /// `pip-audit`, `govulncheck`, `composer`, `bundler-audit`). A scanner that
    /// was dispatched but exited without a usable result is an issue, not an
    /// entry here. The `*_present` flags say what COULD run; this says what
    /// DID. `serde(default)` keeps older serialized reports loadable.
    #[serde(default)]
    pub scanners_ran: Vec<String>,
    /// True only when every discovered, supported audit target was scanned
    /// successfully and no coverage-affecting safety cap was hit.
    #[serde(default)]
    pub audit_complete: bool,
    /// Audit targets found versus audited, with the ones that were not. Both
    /// numbers travel so "complete" is never read off a sample.
    /// `serde(default)` keeps older serialized reports loadable.
    #[serde(default)]
    pub audit_coverage: AuditCoverage,
    /// Keys of the targets whose scanner ran and produced a parsed result.
    #[serde(skip)]
    audited_targets: Vec<String>,
    pub manifests: Vec<NpmManifest>,
    pub ecosystems: Vec<EcosystemHint>,
    pub issues: Vec<HealthIssue>,
    pub vulnerabilities: Vec<Vulnerability>,
    pub audit: AuditSummary,
    pub outdated: Vec<OutdatedPackage>,
    pub truncated: bool,
    /// Exact retained/observed counts for every safety budget that fired.
    #[serde(default)]
    pub limit_notices: Vec<ScanLimitNotice>,
}

/// How much of what was found got audited. `found` counts every audit target
/// the walk saw — including ecosystems no scanner here can read — and
/// `audited` only those whose scanner ran and parsed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditCoverage {
    pub found: usize,
    pub audited: usize,
    /// `family:path` for each found target that was not audited.
    pub unaudited: Vec<String>,
}

/// Ecosystem artifacts collected while listing tracked files; the CLI enrichers
/// consume these so probes only run when something is actually there to scan.
#[derive(Debug, Default)]
struct ScanTargets {
    cargo_locks: Vec<String>,
    deny_configs: Vec<String>,
    /// requirements*.txt files (pinned lists pip-audit can audit without pip).
    py_requirements: Vec<String>,
    go_mods: Vec<String>,
    composer_locks: Vec<String>,
    gemfile_locks: Vec<String>,
    /// Python lockfiles and project files pip-audit is not wired to read
    /// here (`uv.lock`, `poetry.lock`, `pyproject.toml`, …), and artifacts of
    /// families with no scanner at all. Each is `(family, path)`.
    unaudited: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub struct ScanOptions {
    /// When false, skip npm audit / outdated and cargo audit. Local inventory
    /// still runs. Tests use this so a missing lockfile does not hit the registry.
    pub run_cli: bool,
    /// Test seam: when set, replaces the process `PATH` for every probe and
    /// spawn this scan performs, and is the whole search path — the GUI-launch
    /// fallback dirs are not added to it, so a tool absent from it is absent.
    /// `None` inherits the real environment, fallback dirs included (app
    /// behavior).
    pub path_var: Option<std::ffi::OsString>,
    /// Test seam: when set, replaces the process `HOME` for every probe and
    /// spawn this scan performs. Under an inherited PATH it also roots the
    /// home-relative fallback dirs (`~/.cargo/bin`, `~/go/bin`, …); under an
    /// explicit [`ScanOptions::path_var`] no fallback dir is searched.
    pub home: Option<std::ffi::OsString>,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            run_cli: true,
            path_var: None,
            home: None,
        }
    }
}

/// The concrete environment view a scan runs its CLI probes and spawns in.
///
/// Production builds it from the process environment ([`ScanOptions`] seams
/// unset); tests build it from a simulated Finder/Dock-style minimal `PATH`
/// and a scratch `HOME`. `None` genuinely means "variable unset", matching
/// the semantics of `build_capture_command`'s parameters.
#[derive(Debug, Clone)]
struct ScanEnv {
    path_var: Option<std::ffi::OsString>,
    /// Whether `path_var` is the override or the process value; only the
    /// latter is widened by the GUI-launch fallback dirs.
    path_source: PathSource,
    home: Option<std::ffi::OsString>,
    /// The scan's one deadline and what it cut. `None` for a probe or test
    /// that is bounded only by its own per-command timeout.
    scan: Option<Arc<ScanClock>>,
}

/// What a refused spawn's error starts with, so a probe can tell "the deadline
/// would not let it start" from a tool that is absent.
const DEADLINE_REFUSED: &str = "the scan deadline passed before ";

/// The one deadline of a dependency scan and the record of what it cut.
///
/// Every spawn goes through [`capture_scanner_command`], which asks the clock
/// for the command's timeout: the command's own timeout, shortened to what is
/// left of the deadline, or nothing at all once the deadline has passed. A
/// command already running at the deadline is therefore cut at it, so the
/// scan returns within the deadline plus the cost of killing one child.
/// Every auditor step that did not run, or was cut, is named here, so a
/// partial scan says which ecosystems it did not cover.
#[derive(Debug)]
struct ScanClock {
    deadline: ScanDeadline,
    state: Mutex<ClockState>,
}

#[derive(Debug, Default)]
struct ClockState {
    /// Steps that ran to the end without being cut.
    completed: usize,
    /// Steps that did not run or were cut, each named once.
    not_run: Vec<String>,
    /// Every time something was recorded as not run, repeats included. A step
    /// is complete when this did not move while it ran; the length of
    /// `not_run` cannot say so, because a repeated name does not grow it.
    cuts: usize,
    /// A version probe was refused or cut by the deadline, so a scanner's
    /// presence was never established either way.
    probes_unresolved: bool,
}

impl ScanClock {
    fn new(deadline: ScanDeadline) -> Self {
        Self {
            deadline,
            state: Mutex::new(ClockState::default()),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, ClockState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn note_not_run(&self, what: &str) {
        let mut state = self.state();
        state.cuts += 1;
        if !state.not_run.iter().any(|seen| seen == what) {
            state.not_run.push(what.to_string());
        }
    }

    /// True when a probe's timeout error happened with the deadline gone: the
    /// deadline shortened it, or it ran out at the same moment.
    fn deadline_cut_probe(&self, error: &str) -> bool {
        is_timed_out(error) && self.deadline.remaining().is_zero()
    }

    /// True once a version probe was refused or cut by the deadline.
    fn probes_unresolved(&self) -> bool {
        self.state().probes_unresolved
    }

    /// True, and `what` recorded as not run, once the deadline has passed.
    /// Checked before starting each auditor and each npm root.
    fn skip(&self, what: &str) -> bool {
        let expired = self.deadline.expired();
        if expired {
            self.note_not_run(what);
        }
        expired
    }

    /// The timeout for a command about to start: its own, shortened to what is
    /// left of the deadline, and whether the deadline shortened it. `None`
    /// once nothing is left.
    fn timeout_for(&self, requested: Duration, what: &str) -> Option<(Duration, bool)> {
        let left = self.deadline.remaining();
        if left.is_zero() {
            self.note_not_run(what);
            return None;
        }
        Some((requested.min(left), left < requested))
    }

    /// A command is cut by the deadline only if the deadline shortened its
    /// timeout and it then timed out. One that failed for its own reason —
    /// however close to the deadline — is reported by its own failure.
    fn note_finished<T>(&self, outcome: &Result<T, String>, what: &str, shortened: bool) {
        if shortened && outcome.as_ref().is_err_and(|e| is_timed_out(e)) {
            self.note_not_run(&format!("{what} (cut off at the deadline)"));
        }
    }

    /// Runs one auditor step unless the deadline has passed. A step with
    /// nothing to do is neither run nor counted.
    fn step<T>(&self, name: &str, has_work: bool, run: impl FnOnce() -> T) -> Option<T> {
        if !has_work || self.skip(name) {
            return None;
        }
        let before = self.state().cuts;
        let out = run();
        let mut state = self.state();
        if state.cuts == before {
            state.completed += 1;
        }
        Some(out)
    }
}

/// Puts what the scan deadline cut into the report: truncated, a limit
/// notice that blocks `audit_complete`, and one issue that names the steps.
fn record_deadline_cut(report: &mut DepsHealthReport, clock: &ScanClock) {
    let (completed, not_run) = {
        let state = clock.state();
        (state.completed, state.not_run.clone())
    };
    if not_run.is_empty() {
        return;
    }
    const NAMED: usize = 12;
    let mut names = not_run
        .iter()
        .take(NAMED)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    if not_run.len() > NAMED {
        names.push_str(&format!(" and {} more", not_run.len() - NAMED));
    }
    report.truncated = true;
    report.limit_notices.push(ScanLimitNotice {
        resource: "audit steps finished before the scan deadline".into(),
        kept: completed,
        total: completed + not_run.len(),
        inventory_only: false,
    });
    report.issues.insert(
        0,
        HealthIssue {
            severity: "error".into(),
            code: "scan_deadline".into(),
            message: format!(
                "The dependency scan reached its deadline. These did not run or were cut short: {names}. This is not a clean bill of health."
            ),
            path: None,
        },
    );
}

impl ScanEnv {
    /// Environment view implied by the options: an override wins wholesale,
    /// otherwise the process value is used as-is (`None` stays `None`).
    fn from_options(options: &ScanOptions) -> Self {
        let (path_var, path_source) = match &options.path_var {
            Some(path_var) => (Some(path_var.clone()), PathSource::Explicit),
            None => (std::env::var_os("PATH"), PathSource::Inherited),
        };
        Self {
            path_var,
            path_source,
            home: options
                .home
                .clone()
                .or_else(|| std::env::var_os("HOME"))
                .or_else(|| std::env::var_os("USERPROFILE")),
            scan: None,
        }
    }

    fn path(&self) -> Option<&std::ffi::OsStr> {
        self.path_var.as_deref()
    }

    fn home(&self) -> Option<&std::ffi::OsStr> {
        self.home.as_deref()
    }
}

pub struct DepsScanner;

impl DepsScanner {
    pub fn scan(repo_path: &str) -> Result<DepsHealthReport, String> {
        Self::scan_with(repo_path, ScanOptions::default())
    }

    /// One scan under one [`AUDIT_TIMEOUT`] deadline.
    pub fn scan_with(repo_path: &str, options: ScanOptions) -> Result<DepsHealthReport, String> {
        Self::scan_with_deadline(repo_path, options, ScanDeadline::audit())
    }

    /// One scan under a deadline of `budget` from now. See
    /// [`DepsScanner::scan_with_deadline`].
    pub fn scan_within(
        repo_path: &str,
        options: ScanOptions,
        budget: Duration,
    ) -> Result<DepsHealthReport, String> {
        Self::scan_with_deadline(repo_path, options, ScanDeadline::after(budget))
    }

    /// One scan under `deadline`: no auditor, ecosystem or npm root starts
    /// after it, and a command still running at it is cut at it. What was cut
    /// is named in the report, which is then never `audit_complete`.
    pub(crate) fn scan_with_deadline(
        repo_path: &str,
        options: ScanOptions,
        deadline: ScanDeadline,
    ) -> Result<DepsHealthReport, String> {
        let repo = validate_repo(repo_path)?;
        let clock = Arc::new(ScanClock::new(deadline));
        let mut env = ScanEnv::from_options(&options);
        env.scan = Some(Arc::clone(&clock));
        let (mut report, targets) = local_scan(&repo, &env)?;
        if options.run_cli {
            // A scanner is recorded only once it actually dispatched a
            // command — presence flags alone never imply execution.
            let mut ran: Vec<String> = Vec::new();
            let has_manifests = !report.manifests.is_empty();
            if let Some(js) = clock.step("npm audit", has_manifests, || {
                enrich_npm(&repo, &mut report, &env)
            }) {
                if js.npm {
                    ran.push("npm".into());
                }
                if js.bun {
                    ran.push("bun".into());
                }
            }
            // Not gated on the lock list: the auditable-binary scan inside
            // this step looks for release builds even when none is listed.
            let cargo_work = report.cargo_audit_present;
            if clock
                .step("cargo audit", cargo_work, || {
                    enrich_cargo(&repo, &targets, &mut report, &env)
                })
                .unwrap_or(false)
            {
                ran.push("cargo".into());
            }
            let deny_work = report.cargo_deny_present && !targets.deny_configs.is_empty();
            if clock
                .step("cargo-deny", deny_work, || {
                    enrich_cargo_deny(&repo, &targets, &mut report, &env)
                })
                .unwrap_or(false)
            {
                ran.push("cargo-deny".into());
            }
            let crev_work = report.cargo_crev_present && !targets.cargo_locks.is_empty();
            if clock
                .step("cargo-crev", crev_work, || {
                    enrich_cargo_crev(&repo, &targets, &mut report, &env)
                })
                .unwrap_or(false)
            {
                ran.push("cargo-crev".into());
            }
            let python_work = report.pip_audit_present && !targets.py_requirements.is_empty();
            if clock
                .step("pip-audit", python_work, || {
                    enrich_python(&repo, &targets, &mut report, &env)
                })
                .unwrap_or(false)
            {
                ran.push("pip-audit".into());
            }
            let go_work = report.govulncheck_present && !targets.go_mods.is_empty();
            if clock
                .step("govulncheck", go_work, || {
                    enrich_go(&repo, &targets, &mut report, &env)
                })
                .unwrap_or(false)
            {
                ran.push("govulncheck".into());
            }
            let php_work = report.composer_present && !targets.composer_locks.is_empty();
            if clock
                .step("composer audit", php_work, || {
                    enrich_php(&repo, &targets, &mut report, &env)
                })
                .unwrap_or(false)
            {
                ran.push("composer".into());
            }
            let ruby_work = report.bundler_audit_present && !targets.gemfile_locks.is_empty();
            if clock
                .step("bundler-audit", ruby_work, || {
                    enrich_ruby(&repo, &targets, &mut report, &env)
                })
                .unwrap_or(false)
            {
                ran.push("bundler-audit".into());
            }
            ran.sort();
            report.scanners_ran = ran;
        }
        record_deadline_cut(&mut report, &clock);
        report.audit = AuditSummary::from_vulns(&report.vulnerabilities);
        sort_report(&mut report);
        cap_report(&mut report);
        report.audit_coverage = audit_coverage(&report, &targets);
        report.audit_complete = audit_is_complete(&report, &targets, options.run_cli);
        Ok(report)
    }
}

fn local_scan(repo: &Path, env: &ScanEnv) -> Result<(DepsHealthReport, ScanTargets), String> {
    let (listed, file_limit) = list_repo_files(repo)?;
    let mut limit_notices = file_limit.into_iter().collect::<Vec<_>>();
    let mut other: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut issues = Vec::new();
    let mut manifests = Vec::new();
    let mut targets = ScanTargets::default();
    let mut npm_manifest_candidates = 0usize;

    for rel in &listed {
        let name = rel.rsplit('/').next().unwrap_or(rel.as_str());
        if let Some(family) = LanguageDetector::ecosystem_hint(rel) {
            if family != "npm" {
                other
                    .entry(family.to_string())
                    .or_default()
                    .push(rel.clone());
            }
        }
        if name != "package.json" {
            collect_side_target(name, rel, &mut targets);
            continue;
        }
        npm_manifest_candidates += 1;
        if manifests.len() >= MAX_MANIFESTS {
            continue;
        }
        match read_npm_manifest(repo, rel) {
            Ok(Some(m)) => {
                collect_manifest_issues(&m, &mut issues);
                manifests.push(m);
            }
            Ok(None) => {}
            Err(msg) => push_issue(
                &mut issues,
                "error",
                "manifest_unreadable",
                msg,
                Some(rel.clone()),
            ),
        }
    }

    if npm_manifest_candidates > MAX_MANIFESTS {
        limit_notices.push(ScanLimitNotice {
            resource: "npm manifests".into(),
            kept: MAX_MANIFESTS,
            total: npm_manifest_candidates,
            inventory_only: false,
        });
    }
    cap_scan_targets(&mut targets, &mut limit_notices);
    collect_unaudited_targets(&listed, &other, &mut targets);

    let node_probe = probe_tool_version(node_program(), &["-v"], env, VERSION_TIMEOUT);
    let npm_probe = probe_tool_version(npm_program(), &["--version"], env, VERSION_TIMEOUT);
    let node_version = node_probe.version().map(str::to_string);
    let npm_version = npm_probe.version().map(str::to_string);
    let npm_cli_present = npm_version.is_some();
    let cargo_audit_present = !targets.cargo_locks.is_empty() && cargo_audit_available(env);
    let cargo_deny_present = !targets.deny_configs.is_empty() && cargo_deny_available(env);
    let cargo_crev_installed = !targets.cargo_locks.is_empty() && cargo_crev_available(env);
    // `cargo crev verify` calls `Local::auto_create_or_open`, which writes a
    // new identity under the crev config dir when none exists. Presence means
    // the identity is already there; GitPulse does not create one.
    let cargo_crev_present = cargo_crev_installed && crev_identity_configured(env);
    if cargo_crev_installed && !cargo_crev_present {
        push_issue(
            &mut issues,
            "info",
            "cargo_crev_unconfigured",
            "cargo-crev is installed but has no local identity, so reviews were not checked. GitPulse does not create one.".into(),
            None,
        );
    }
    // Scanner probes are gated on artifacts: a pure-Node repo never pays for a
    // pip-audit / govulncheck / composer spawn.
    let pip_audit_present = pip_audit_present(&targets)
        && tool_version(pip_audit_program(), &["--version"], env).is_some();
    let govulncheck_present = govulncheck_present(&targets)
        && tool_version(govulncheck_program(), &["-version"], env).is_some();
    let composer_present = !targets.composer_locks.is_empty()
        && tool_version(composer_program(), &["--version"], env).is_some();
    let bundler_audit_present = !targets.gemfile_locks.is_empty()
        && tool_version(bundler_audit_program(), &["--version"], env).is_some();

    let needs_bun = manifests
        .iter()
        .any(|manifest| matches!(js_lock_kind(manifest.lockfile.as_deref()), JsLockKind::Bun));
    let bun_probe =
        needs_bun.then(|| probe_tool_version(bun_program(), &["--version"], env, VERSION_TIMEOUT));
    let bun_cli_present = bun_probe
        .as_ref()
        .and_then(|probe| probe.version())
        .is_some();
    if needs_bun && !bun_cli_present {
        let message = match bun_probe.as_ref() {
            Some(ToolProbe::FoundButFailed(detail)) => format!(
                "bun was found but could not be run: {detail}. bun.lock was not audited; npm audit cannot read a Bun lockfile."
            ),
            Some(ToolProbe::NotRun(detail)) => format!(
                "bun could not be checked: {detail}. bun.lock was not audited; npm audit cannot read a Bun lockfile."
            ),
            _ => "bun is not installed or not on PATH; bun.lock was not audited. npm audit cannot read a Bun lockfile.".to_string(),
        };
        push_issue(&mut issues, "warning", "bun_missing", message, None);
    }

    if !manifests.is_empty() && !npm_cli_present {
        let only_bun = manifests
            .iter()
            .all(|manifest| matches!(js_lock_kind(manifest.lockfile.as_deref()), JsLockKind::Bun));
        // A Bun lockfile is audited with `bun audit`. npm is only the outdated
        // check in that case, so the warning must not say the vulnerability
        // scan did not run.
        if only_bun {
            let cause = match &npm_probe {
                ToolProbe::FoundButFailed(detail) => {
                    format!("npm was found but could not be run: {detail}")
                }
                ToolProbe::NotRun(detail) => format!("npm could not be checked: {detail}"),
                ToolProbe::Present(_) | ToolProbe::NotFound(_) => {
                    "npm is not installed or not on PATH".to_string()
                }
            };
            push_issue(
                &mut issues,
                "info",
                "npm_missing",
                format!("{cause}; outdated checks did not run. Vulnerabilities in bun.lock are checked with bun audit when bun is on PATH."),
                None,
            );
        } else {
            push_issue(
                &mut issues,
                "warning",
                "npm_missing",
                npm_missing_message(&npm_probe, &node_probe, env),
                None,
            );
        }
    }

    for m in &manifests {
        if let (Some(spec), Some(current)) = (m.engines_node.as_deref(), node_version.as_deref()) {
            if let Some(msg) = engines_mismatch(spec, current) {
                push_issue(
                    &mut issues,
                    "warning",
                    "engines_node",
                    msg,
                    Some(m.path.clone()),
                );
            }
        }
    }

    let mut ecosystems: Vec<EcosystemHint> = Vec::new();
    for (family, mut files) in other {
        files.sort();
        files.dedup();
        if files.len() > MAX_ECOSYSTEM_MANIFESTS {
            limit_notices.push(ScanLimitNotice {
                resource: format!("{family} ecosystem artifacts"),
                kept: MAX_ECOSYSTEM_MANIFESTS,
                total: files.len(),
                inventory_only: true,
            });
            files.truncate(MAX_ECOSYSTEM_MANIFESTS);
        }
        let probes_unresolved = env
            .scan
            .as_ref()
            .is_some_and(|scan| scan.probes_unresolved());
        let note = match family.as_str() {
            // A scanner the deadline never let us look for is not a missing
            // one: no "install it" advice for what may well be installed.
            "cargo" | "go" | "python" | "php" | "ruby" if probes_unresolved => {
                "The scan deadline passed before the scanners for this ecosystem could be checked, so whether they are installed is unknown".into()
            }
            "cargo" => cargo_family_note(
                &files,
                cargo_audit_present,
                cargo_deny_present,
                cargo_crev_installed,
                cargo_crev_present,
            ),
            "go" => {
                if govulncheck_present {
                    "Go modules will be checked with govulncheck".into()
                } else {
                    "Install govulncheck (`go install golang.org/x/vuln/cmd/govulncheck@latest`) to scan Go modules".into()
                }
            }
            "python" => {
                if pip_audit_present {
                    "Pinned requirements*.txt files will be checked with pip-audit (--no-deps)"
                        .into()
                } else {
                    "Install pip-audit (`pipx install pip-audit`) to scan pinned requirements files"
                        .into()
                }
            }
            "php" => {
                if composer_present {
                    "composer.lock will be checked with composer audit --locked".into()
                } else {
                    "Install Composer 2.4+ to run `composer audit` against composer.lock".into()
                }
            }
            "ruby" => {
                if bundler_audit_present {
                    "Gemfile.lock will be checked with bundler-audit".into()
                } else {
                    "Install bundler-audit (`gem install bundler-audit`) to scan Gemfile.lock"
                        .into()
                }
            }
            _ => "Detected; no scanner wired".into(),
        };
        ecosystems.push(EcosystemHint {
            family,
            manifests: files,
            note,
        });
    }
    ecosystems.sort_by(|a, b| a.family.cmp(&b.family));

    Ok((
        DepsHealthReport {
            node_version,
            npm_version,
            npm_cli_present,
            bun_cli_present,
            cargo_audit_present,
            cargo_deny_present,
            cargo_crev_present,
            pip_audit_present,
            govulncheck_present,
            composer_present,
            bundler_audit_present,
            scanners_ran: Vec::new(),
            audit_complete: false,
            audit_coverage: AuditCoverage::default(),
            audited_targets: Vec::new(),
            manifests,
            ecosystems,
            issues,
            vulnerabilities: Vec::new(),
            audit: AuditSummary::default(),
            outdated: Vec::new(),
            truncated: !limit_notices.is_empty(),
            limit_notices,
        },
        targets,
    ))
}

/// Files that name Python dependencies but that this scan cannot audit.
fn python_unaudited_name(name: &str) -> bool {
    matches!(
        name,
        "uv.lock" | "poetry.lock" | "Pipfile.lock" | "pdm.lock" | "pyproject.toml" | "Pipfile"
    ) || name == "setup.py"
        || name == "setup.cfg"
}

/// Records artifacts that were found but have no audit path, so completeness
/// cannot be read off the families that happen to have one.
///
/// Python lockfiles are never readable by the wired pip-audit invocation
/// (`-r requirements.txt`); a `pyproject.toml`/`setup.py` directory is
/// unaudited unless a requirements file sits beside it. Families with no
/// scanner wired at all (JVM, Swift, Dart, Elixir, .NET) are unaudited as a
/// whole. Cargo, Go, npm, PHP and Ruby have their own checks, and `native`
/// build files declare no dependencies.
fn collect_unaudited_targets(
    listed: &[String],
    other: &BTreeMap<String, Vec<String>>,
    targets: &mut ScanTargets,
) {
    let requirement_dirs: std::collections::BTreeSet<String> =
        targets.py_requirements.iter().map(|r| dir_of(r)).collect();
    for rel in listed {
        let name = rel.rsplit('/').next().unwrap_or(rel.as_str());
        if !python_unaudited_name(name) {
            continue;
        }
        let always = matches!(
            name,
            "uv.lock" | "poetry.lock" | "Pipfile.lock" | "pdm.lock"
        );
        if always || !requirement_dirs.contains(&dir_of(rel)) {
            targets.unaudited.push(("python".into(), rel.clone()));
        }
    }
    for (family, files) in other {
        if matches!(
            family.as_str(),
            "jvm" | "swift" | "dart" | "elixir" | "dotnet"
        ) {
            for rel in files {
                targets.unaudited.push((family.clone(), rel.clone()));
            }
        }
    }
    targets.unaudited.sort();
    targets.unaudited.dedup();
}

/// Records non-npm scan inputs spotted during the tracked-file walk.
fn collect_side_target(name: &str, rel: &str, targets: &mut ScanTargets) {
    if name == "Cargo.lock" {
        targets.cargo_locks.push(rel.to_string());
        return;
    }
    if name.eq_ignore_ascii_case("deny.toml") {
        targets.deny_configs.push(rel.to_string());
        return;
    }
    if name == "go.mod" {
        targets.go_mods.push(rel.to_string());
        return;
    }
    if name == "composer.lock" {
        targets.composer_locks.push(rel.to_string());
        return;
    }
    if name == "Gemfile.lock" {
        targets.gemfile_locks.push(rel.to_string());
        return;
    }
    // requirements.txt, requirements-dev.txt, requirements/production.txt …
    if name.starts_with("requirements") && name.ends_with(".txt") || name == "constraints.txt" {
        targets.py_requirements.push(rel.to_string());
    }
}

fn cap_target_list(
    items: &mut Vec<String>,
    max: usize,
    resource: &str,
    notices: &mut Vec<ScanLimitNotice>,
) {
    if items.len() > max {
        notices.push(ScanLimitNotice {
            resource: resource.into(),
            kept: max,
            total: items.len(),
            inventory_only: false,
        });
        items.truncate(max);
    }
}

fn cap_scan_targets(targets: &mut ScanTargets, notices: &mut Vec<ScanLimitNotice>) {
    cap_target_list(
        &mut targets.cargo_locks,
        MAX_CARGO_LOCKS,
        "Cargo.lock audit targets",
        notices,
    );
    cap_target_list(
        &mut targets.deny_configs,
        MAX_DENY_CONFIGS,
        "deny.toml audit targets",
        notices,
    );
    cap_target_list(
        &mut targets.py_requirements,
        MAX_PY_REQUIREMENTS,
        "Python audit targets",
        notices,
    );
    cap_target_list(
        &mut targets.go_mods,
        MAX_GO_MODS,
        "Go audit targets",
        notices,
    );
    cap_target_list(
        &mut targets.composer_locks,
        MAX_COMPOSER_LOCKS,
        "Composer audit targets",
        notices,
    );
    cap_target_list(
        &mut targets.gemfile_locks,
        MAX_COMPOSER_LOCKS,
        "Bundler audit targets",
        notices,
    );
}

fn pip_audit_present(targets: &ScanTargets) -> bool {
    !targets.py_requirements.is_empty()
}

fn govulncheck_present(targets: &ScanTargets) -> bool {
    !targets.go_mods.is_empty()
}

fn pip_audit_program() -> &'static str {
    "pip-audit"
}

fn govulncheck_program() -> &'static str {
    "govulncheck"
}

fn composer_program() -> &'static str {
    if cfg!(windows) {
        "composer.bat"
    } else {
        "composer"
    }
}

fn bundler_audit_program() -> &'static str {
    if cfg!(windows) {
        "bundler-audit.bat"
    } else {
        "bundler-audit"
    }
}

fn collect_manifest_issues(manifest: &NpmManifest, issues: &mut Vec<HealthIssue>) {
    if manifest.lockfile.is_none() {
        push_issue(
            issues,
            "warning",
            "missing_lockfile",
            "No lockfile next to this package.json (package-lock.json, yarn.lock, pnpm-lock.yaml, or bun.lock). npm audit needs a lockfile.".into(),
            Some(manifest.path.clone()),
        );
    }
    if !manifest.lifecycle_scripts.is_empty() {
        push_issue(
            issues,
            "info",
            "lifecycle_scripts",
            format!(
                "Install-time scripts: {}. These run on npm install; review them before installing dependencies.",
                manifest.lifecycle_scripts.join(", ")
            ),
            Some(manifest.path.clone()),
        );
    }
}

/// Which JavaScript auditors actually dispatched a command.
struct JsCliDispatch {
    /// `npm audit` ran. `npm outdated` alone does not count: it is not an audit.
    npm: bool,
    /// `bun audit` ran.
    bun: bool,
}

/// Runs the JavaScript audit that can read each manifest's lockfile, then
/// `npm outdated` when npm is on PATH.
///
/// `npm audit` only reads an npm lockfile. Pointing it at `bun.lock` (or
/// `yarn.lock` / `pnpm-lock.yaml`) makes npm exit `ENOLOCK`, and recording
/// that as `audit_failed` pretends the audit ran. Bun lockfiles are audited
/// with `bun audit --json` instead. A root whose working directory cannot be
/// validated dispatches nothing and does not count.
fn enrich_npm(repo: &Path, report: &mut DepsHealthReport, env: &ScanEnv) -> JsCliDispatch {
    if report.manifests.is_empty() {
        return JsCliDispatch {
            npm: false,
            bun: false,
        };
    }
    let mut roots = npm_scan_roots(&report.manifests);
    if roots.len() > MAX_NPM_ROOTS {
        record_limit(report, "npm audit roots", MAX_NPM_ROOTS, roots.len());
        roots.truncate(MAX_NPM_ROOTS);
    }
    let mut npm = false;
    let mut bun = false;
    for rel_dir in roots {
        // Each root is two commands that can take AUDIT_TIMEOUT apiece; the
        // scan deadline is checked before starting each root.
        if env.scan.as_ref().is_some_and(|scan| {
            scan.skip(&format!(
                "npm audit and outdated in {}",
                package_label(&rel_dir)
            ))
        }) {
            continue;
        }
        let cwd = match npm_cwd(repo, &rel_dir) {
            Ok(p) => p,
            Err(msg) => {
                push_issue(&mut report.issues, "error", "audit_cwd", msg, Some(rel_dir));
                continue;
            }
        };
        let label = package_label(&rel_dir);
        // Neither npm nor bun can be told to ignore the repository's own
        // registry config, so a repository that sets one is refused, loudly.
        let lock_is_bun = report.manifests.iter().any(|manifest| {
            manifest
                .lockfile_dir
                .clone()
                .unwrap_or_else(|| dir_of(&manifest.path))
                == rel_dir
                && js_lock_kind(manifest.lockfile.as_deref()) == JsLockKind::Bun
        });
        if let Some((code, message)) = repo_config_refusal(repo, &rel_dir, lock_is_bun) {
            push_issue(&mut report.issues, "error", code, message, Some(label));
            continue;
        }
        // Owned before the dispatch: those calls borrow `report` mutably.
        let lock_name = report
            .manifests
            .iter()
            .find(|manifest| {
                manifest
                    .lockfile_dir
                    .clone()
                    .unwrap_or_else(|| dir_of(&manifest.path))
                    == rel_dir
                    && manifest.lockfile.is_some()
            })
            .and_then(|manifest| manifest.lockfile.clone());
        match js_lock_kind(lock_name.as_deref()) {
            JsLockKind::Npm if report.npm_cli_present => {
                if dispatch_npm_audit(&cwd, report, env, &label) {
                    npm = true;
                    mark_audited(report, format!("npm:{rel_dir}"));
                }
            }
            JsLockKind::Bun if report.bun_cli_present => {
                if dispatch_bun_audit(repo, &cwd, report, env, &label) {
                    bun = true;
                    mark_audited(report, format!("npm:{rel_dir}"));
                }
            }
            JsLockKind::Unsupported => {
                let name = lock_name.as_deref().unwrap_or("lockfile");
                push_issue(
                    &mut report.issues,
                    "error",
                    "js_lockfile_unaudited",
                    format!(
                        "npm audit cannot read {name}. This scan does not run a separate auditor for it, so the result is not a clean bill of health."
                    ),
                    Some(label.clone()),
                );
            }
            JsLockKind::Npm | JsLockKind::Bun | JsLockKind::Missing => {}
        }
        if report.npm_cli_present {
            dispatch_npm_outdated(repo, &cwd, report, env, &label);
        }
    }
    JsCliDispatch { npm, bun }
}

/// True when the audit ran and its report parsed without an error.
fn dispatch_npm_audit(
    cwd: &Path,
    report: &mut DepsHealthReport,
    env: &ScanEnv,
    label: &str,
) -> bool {
    let fail = |report: &mut DepsHealthReport, message: String| {
        push_issue(
            &mut report.issues,
            "error",
            "audit_failed",
            message,
            Some(label.to_string()),
        );
        false
    };
    match run_npm_json(cwd, &["audit", "--json"], env) {
        Ok(text) => match parse_npm_audit_json(&text) {
            Ok((mut vulns, err)) => {
                report.vulnerabilities.append(&mut vulns);
                match err {
                    Some(err) => fail(report, err),
                    None => true,
                }
            }
            Err(e) => fail(report, e),
        },
        Err(e) => fail(report, e),
    }
}

/// True when the audit ran and its report parsed.
fn dispatch_bun_audit(
    repo: &Path,
    cwd: &Path,
    report: &mut DepsHealthReport,
    env: &ScanEnv,
    label: &str,
) -> bool {
    let fail = |report: &mut DepsHealthReport, message: String| {
        push_issue(
            &mut report.issues,
            "error",
            "bun_audit_failed",
            message,
            Some(label.to_string()),
        );
        false
    };
    match run_json_cli(cwd, bun_program(), &["audit", "--json"], env) {
        Ok(text) => match parse_bun_audit_json(&text) {
            Ok(mut vulns) => {
                let sections = declared_sections_or_issue(repo, cwd, report);
                for vuln in &mut vulns {
                    vuln.is_direct = sections.contains_key(&vuln.name);
                }
                report.vulnerabilities.append(&mut vulns);
                true
            }
            Err(e) => fail(report, e),
        },
        Err(e) => fail(report, e),
    }
}

fn dispatch_npm_outdated(
    repo: &Path,
    cwd: &Path,
    report: &mut DepsHealthReport,
    env: &ScanEnv,
    label: &str,
) {
    match run_npm_json(cwd, &["outdated", "--json"], env) {
        Ok(text) => match parse_npm_outdated_json(&text) {
            Ok(mut rows) => {
                let sections = declared_sections_or_issue(repo, cwd, report);
                fill_dependency_types(&mut rows, &sections);
                report.outdated.append(&mut rows);
            }
            Err(e) => push_issue(
                &mut report.issues,
                "warning",
                "outdated_failed",
                e,
                Some(label.to_string()),
            ),
        },
        Err(e) => push_issue(
            &mut report.issues,
            "warning",
            "outdated_failed",
            e,
            Some(label.to_string()),
        ),
    }
}

/// Runs `cargo audit --json --file <path>` against every bounded Cargo.lock
/// target. Returns true when at least one command was dispatched.
fn enrich_cargo(
    repo: &Path,
    targets: &ScanTargets,
    report: &mut DepsHealthReport,
    env: &ScanEnv,
) -> bool {
    if !report.cargo_audit_present {
        return false;
    }
    let Some(audit_program) = cargo_audit_program(env) else {
        return false;
    };
    let mut ran = false;
    for rel in &targets.cargo_locks {
        let valid_lock = sandbox_join_canonical(repo, rel)
            .ok()
            .map(|path| path.is_file())
            .unwrap_or(false);
        if !valid_lock {
            push_issue(
                &mut report.issues,
                "error",
                "audit_cwd",
                "Cargo.lock could not be validated inside the repository".into(),
                Some(rel.clone()),
            );
            continue;
        }
        match capture_scanner_command(
            env,
            audit_program,
            &["audit", "--json", "--file", rel],
            Some(repo),
            AUDIT_TIMEOUT,
            &[("CARGO_TERM_COLOR", "never")],
        ) {
            Ok(out) => {
                let text = out.stdout_text();
                if text.trim().is_empty() && !out.success {
                    push_issue(
                        &mut report.issues,
                        "warning",
                        "cargo_audit_failed",
                        if out.stderr_text().is_empty() {
                            format!("cargo audit exited {}", out.status_code)
                        } else {
                            out.stderr_text()
                        },
                        Some(rel.clone()),
                    );
                    continue;
                }
                match parse_cargo_audit_json(&text) {
                    Ok((mut vulns, warnings)) => {
                        ran = true;
                        mark_audited(report, format!("cargo:{rel}"));
                        report.vulnerabilities.append(&mut vulns);
                        for mut warning in warnings {
                            warning.path = Some(rel.clone());
                            push_issue(
                                &mut report.issues,
                                &warning.severity,
                                &warning.code,
                                warning.message,
                                warning.path,
                            );
                        }
                    }
                    Err(e) => push_issue(
                        &mut report.issues,
                        "warning",
                        "cargo_audit_failed",
                        e,
                        Some(rel.clone()),
                    ),
                }
            }
            Err(e) => {
                push_issue(
                    &mut report.issues,
                    "warning",
                    "cargo_audit_failed",
                    e,
                    Some(rel.clone()),
                );
            }
        }
    }
    if scan_auditable_binaries(repo, &targets.cargo_locks, report, env, audit_program) {
        ran = true;
    }
    ran
}

fn cargo_family_note(
    files: &[String],
    audit: bool,
    deny: bool,
    crev_installed: bool,
    crev_ready: bool,
) -> String {
    let has = |name: &str| {
        files.iter().any(|file| {
            file.rsplit(['/', '\\'])
                .next()
                .unwrap_or(file.as_str())
                .eq_ignore_ascii_case(name)
        })
    };
    let has_lock = has("Cargo.lock");
    let has_deny = has("deny.toml");
    if !has_lock && !has_deny {
        return if audit {
            "Cargo.lock files will be checked with cargo audit".into()
        } else {
            "Install cargo-audit (`cargo install cargo-audit`) to scan Rust crates".into()
        };
    }
    let mut checks = Vec::new();
    if has_lock && audit {
        checks.push("cargo audit");
    }
    if has_deny && deny {
        checks.push("cargo deny");
    }
    if has_lock && crev_ready {
        checks.push("cargo crev");
    }
    let mut note = if checks.is_empty() {
        "Rust supply chain checks did not run".to_string()
    } else {
        format!("Rust supply chain checks: {}", checks.join(", "))
    };
    if has_lock && !audit {
        note.push_str(". Install cargo-audit (`cargo install cargo-audit`) to scan advisories");
    }
    if has_deny && !deny {
        note.push_str(
            ". Install cargo-deny (`cargo install --locked cargo-deny`) to enforce deny.toml",
        );
    }
    if has_lock && crev_installed && !crev_ready {
        note.push_str(
            ". cargo-crev is installed but has no local identity, so reviews were not checked",
        );
    } else if has_lock && !crev_installed {
        note.push_str(". Install cargo-crev (`cargo install cargo-crev`) to check review trust");
    }
    note.push_str(
        ". Release binaries are scanned with `cargo audit bin` when cargo-auditable embedded them",
    );
    note
}

/// `cargo deny check` for every `deny.toml`. Policy hits stay health issues;
/// vulnerability advisories join the vulnerability list and are deduped against
/// cargo-audit by advisory id. `--offline` refuses the network fetch cargo-deny
/// would otherwise perform. The `cargo-deny` binary is invoked directly so a
/// repository `[alias] deny` cannot turn the scan into a build.
fn enrich_cargo_deny(
    repo: &Path,
    targets: &ScanTargets,
    report: &mut DepsHealthReport,
    env: &ScanEnv,
) -> bool {
    if !report.cargo_deny_present {
        return false;
    }
    let Some(program) = cargo_deny_program(env) else {
        return false;
    };
    let mut ran = false;
    for rel in &targets.deny_configs {
        let manifest = match cargo_manifest_beside(repo, rel) {
            Ok(path) => path,
            Err(msg) => {
                push_issue(
                    &mut report.issues,
                    "error",
                    "cargo_deny_failed",
                    msg,
                    Some(rel.clone()),
                );
                continue;
            }
        };
        let manifest_arg = manifest.to_string_lossy().into_owned();
        let cwd = manifest.parent().unwrap_or(repo);
        match capture_scanner_command(
            env,
            program,
            &[
                "--offline",
                "--format",
                "sarif",
                "--manifest-path",
                &manifest_arg,
                "check",
            ],
            Some(cwd),
            AUDIT_TIMEOUT,
            &[("CARGO_TERM_COLOR", "never")],
        ) {
            Ok(out) => {
                ran = true;
                let text = out.stdout_text();
                if text.trim().is_empty() {
                    push_issue(
                        &mut report.issues,
                        "warning",
                        "cargo_deny_failed",
                        if out.stderr_text().is_empty() {
                            format!("cargo deny exited {}", out.status_code)
                        } else {
                            cargo_supply::bound_message(&out.stderr_text())
                        },
                        Some(rel.clone()),
                    );
                    continue;
                }
                match parse_cargo_deny_sarif(&text) {
                    Ok(scan) => {
                        for advisory in scan.advisories {
                            append_new_vuln(
                                report,
                                Vulnerability {
                                    name: advisory.package,
                                    severity: advisory.severity,
                                    is_direct: true,
                                    title: advisory.title,
                                    url: format!(
                                        "https://rustsec.org/advisories/{}",
                                        advisory.advisory_id
                                    ),
                                    range: advisory.advisory_id,
                                    fix_available: if advisory.version.is_empty() {
                                        "see advisory".into()
                                    } else {
                                        advisory.version
                                    },
                                    via: Vec::new(),
                                    ecosystem: "cargo".into(),
                                },
                            );
                        }
                        for lint in scan.lints {
                            push_issue(
                                &mut report.issues,
                                &lint.severity,
                                &lint.code,
                                lint.message,
                                Some(rel.clone()),
                            );
                        }
                    }
                    Err(err) => push_issue(
                        &mut report.issues,
                        "warning",
                        "cargo_deny_failed",
                        err,
                        Some(rel.clone()),
                    ),
                }
            }
            Err(err) => {
                ran = true;
                push_issue(
                    &mut report.issues,
                    "warning",
                    "cargo_deny_failed",
                    err,
                    Some(rel.clone()),
                );
            }
        }
    }
    ran
}

/// `cargo crev verify --json` against the local web of trust.
///
/// A non-zero exit means unverified crates, which is a finding, not a crashed
/// scanner. `CARGO_NET_OFFLINE` keeps the scan from fetching proofs or the
/// crate index. The binary is invoked directly so `[alias] crev` cannot run
/// repository code.
fn enrich_cargo_crev(
    repo: &Path,
    targets: &ScanTargets,
    report: &mut DepsHealthReport,
    env: &ScanEnv,
) -> bool {
    if !report.cargo_crev_present || targets.cargo_locks.is_empty() {
        return false;
    }
    let Some(program) = cargo_crev_program(env) else {
        return false;
    };
    let mut ran = false;
    let mut seen_dirs: Vec<PathBuf> = Vec::new();
    for rel in &targets.cargo_locks {
        let Ok(lock) = sandbox_join_canonical(repo, rel) else {
            continue;
        };
        let Some(dir) = lock.parent() else {
            continue;
        };
        if !dir.join("Cargo.toml").is_file() || seen_dirs.iter().any(|seen| seen == dir) {
            continue;
        }
        seen_dirs.push(dir.to_path_buf());
        match capture_scanner_command(
            env,
            program,
            &["crev", "verify", "--json"],
            Some(dir),
            AUDIT_TIMEOUT,
            &[("CARGO_TERM_COLOR", "never"), ("CARGO_NET_OFFLINE", "true")],
        ) {
            Ok(out) => {
                let stdout = out.stdout_text();
                let stderr = out.stderr_text();
                if stdout.trim().is_empty() {
                    let lowered = stderr.to_ascii_lowercase();
                    if lowered.contains("no trusted") || lowered.contains("crevid") {
                        push_issue(
                            &mut report.issues,
                            "info",
                            "cargo_crev_unconfigured",
                            cargo_supply::bound_message(&stderr),
                            Some(rel.clone()),
                        );
                    } else {
                        push_issue(
                            &mut report.issues,
                            "warning",
                            "cargo_crev_failed",
                            if stderr.is_empty() {
                                format!("cargo crev exited {}", out.status_code)
                            } else {
                                cargo_supply::bound_message(&stderr)
                            },
                            Some(rel.clone()),
                        );
                    }
                    continue;
                }
                match parse_cargo_crev_jsonl(&stdout) {
                    Ok(scan) => {
                        ran = true;
                        if let Some((severity, message)) = crev_summary(&scan) {
                            push_issue(
                                &mut report.issues,
                                &severity,
                                "cargo_crev_trust",
                                message,
                                Some(rel.clone()),
                            );
                        }
                    }
                    Err(err) => push_issue(
                        &mut report.issues,
                        "warning",
                        "cargo_crev_failed",
                        err,
                        Some(rel.clone()),
                    ),
                }
            }
            Err(err) => {
                push_issue(
                    &mut report.issues,
                    "warning",
                    "cargo_crev_failed",
                    err,
                    Some(rel.clone()),
                );
            }
        }
    }
    ran
}

/// Scans executables that already embed cargo-auditable's `.dep-v0` section.
///
/// A normal `cargo build` leaves binaries with no section, and sometimes a
/// partial list recovered from panic messages. That partial list is not
/// merged: cargo-audit says it is incomplete, and treating it as the binary's
/// dependency set would under-report. `--no-fetch` keeps the advisory
/// database from being updated as a side effect of opening the repository.
/// Binaries without the section are not passed to `cargo audit bin` at all.
fn scan_auditable_binaries(
    repo: &Path,
    locks: &[String],
    report: &mut DepsHealthReport,
    env: &ScanEnv,
    program: &str,
) -> bool {
    let configured = auditable_config_path(repo, locks);
    let executables = auditable_binaries(repo, locks);
    let mut embedded: Vec<(String, PathBuf)> = executables
        .iter()
        .filter(|(_, path)| file_contains_auditable_section(path))
        .cloned()
        .collect();
    if embedded.is_empty() {
        if let Some(path) = configured {
            if executables.is_empty() {
                push_issue(
                    &mut report.issues,
                    "info",
                    "cargo_auditable_unbuilt",
                    "cargo-auditable is configured, but no executable under target/release or target/debug was found to scan".into(),
                    Some(path),
                );
            } else {
                push_issue(
                    &mut report.issues,
                    "warning",
                    "cargo_auditable_missing",
                    format!(
                        "{} built executable(s) have no .dep-v0 cargo-auditable section. Build with `cargo auditable build` so `cargo audit bin` can read exact crate versions",
                        executables.len()
                    ),
                    Some(path),
                );
            }
        }
        return false;
    }
    if embedded.len() > MAX_AUDITABLE_BINS {
        record_limit(
            report,
            "cargo-auditable binaries",
            MAX_AUDITABLE_BINS,
            embedded.len(),
        );
        embedded.truncate(MAX_AUDITABLE_BINS);
    }
    let mut ran = false;
    for (label, path) in embedded {
        let arg = path.to_string_lossy().into_owned();
        match capture_scanner_command(
            env,
            program,
            &["audit", "--json", "--no-fetch", "bin", &arg],
            Some(repo),
            AUDIT_TIMEOUT,
            &[("CARGO_TERM_COLOR", "never")],
        ) {
            Ok(out) => {
                ran = true;
                let stdout = out.stdout_text();
                let stderr = out.stderr_text();
                match classify_bin_audit(&stdout, &stderr, out.success) {
                    BinAuditKind::NotAuditable => {
                        push_issue(
                            &mut report.issues,
                            "warning",
                            "cargo_auditable_missing",
                            format!(
                                "{label} has no complete cargo-auditable dependency list. A panic-message recovery is not merged into this report. Build with `cargo auditable build`"
                            ),
                            Some(label),
                        );
                    }
                    BinAuditKind::Failed => {
                        push_issue(
                            &mut report.issues,
                            "warning",
                            "cargo_audit_bin_failed",
                            if stderr.is_empty() {
                                format!("cargo audit bin exited {}", out.status_code)
                            } else {
                                cargo_supply::bound_message(&stderr)
                            },
                            Some(label),
                        );
                    }
                    BinAuditKind::Complete => match parse_cargo_audit_json(&stdout) {
                        Ok((vulns, warnings)) => {
                            for vuln in vulns {
                                append_new_vuln(report, vuln);
                            }
                            for mut warning in warnings {
                                warning.path = Some(label.clone());
                                push_issue(
                                    &mut report.issues,
                                    &warning.severity,
                                    &warning.code,
                                    warning.message,
                                    warning.path,
                                );
                            }
                        }
                        Err(err) => push_issue(
                            &mut report.issues,
                            "warning",
                            "cargo_audit_bin_failed",
                            err,
                            Some(label),
                        ),
                    },
                }
            }
            Err(err) => {
                ran = true;
                push_issue(
                    &mut report.issues,
                    "warning",
                    "cargo_audit_bin_failed",
                    err,
                    Some(label),
                );
            }
        }
    }
    ran
}

fn append_new_vuln(report: &mut DepsHealthReport, vuln: Vulnerability) {
    let duplicate = !vuln.range.is_empty()
        && report.vulnerabilities.iter().any(|existing| {
            existing.ecosystem == vuln.ecosystem
                && existing.name == vuln.name
                && existing.range == vuln.range
        });
    if !duplicate {
        report.vulnerabilities.push(vuln);
    }
}

fn cargo_manifest_beside(repo: &Path, deny_rel: &str) -> Result<PathBuf, String> {
    let deny = sandbox_join_canonical(repo, deny_rel)?;
    let Some(dir) = deny.parent() else {
        return Err(format!("{deny_rel} has no parent directory"));
    };
    let manifest = dir.join("Cargo.toml");
    if !manifest.is_file() {
        return Err(format!(
            "{deny_rel} has no Cargo.toml in the same directory"
        ));
    }
    let canon = manifest
        .canonicalize()
        .map_err(|err| format!("Cannot read Cargo.toml beside {deny_rel}: {err}"))?;
    if !canon.starts_with(repo) {
        return Err(format!(
            "Cargo.toml beside {deny_rel} escaped the repository"
        ));
    }
    Ok(canon)
}

fn auditable_binaries(repo: &Path, locks: &[String]) -> Vec<(String, PathBuf)> {
    let mut found = Vec::new();
    for rel in locks {
        let Ok(lock) = sandbox_join_canonical(repo, rel) else {
            continue;
        };
        let Some(dir) = lock.parent() else {
            continue;
        };
        let mut bins = executables_in(&dir.join("target").join("release"), repo);
        if bins.is_empty() {
            bins = executables_in(&dir.join("target").join("debug"), repo);
        }
        for bin in bins {
            found.push((repo_relative(repo, &bin), bin));
        }
    }
    found.sort();
    found.dedup();
    found
}

fn executables_in(dir: &Path, repo: &Path) -> Vec<PathBuf> {
    let Ok(canon_dir) = dir.canonicalize() else {
        return Vec::new();
    };
    if !canon_dir.starts_with(repo) {
        return Vec::new();
    }
    let Ok(entries) = std::fs::read_dir(&canon_dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !is_candidate_binary_name(&name) {
            continue;
        }
        let path = entry.path();
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if !meta.is_file() || meta.len() == 0 || meta.len() > MAX_AUDITABLE_BIN_BYTES {
            continue;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if meta.permissions().mode() & 0o111 == 0 {
                continue;
            }
        }
        let Ok(canon) = path.canonicalize() else {
            continue;
        };
        if canon.starts_with(repo) {
            out.push(canon);
        }
    }
    out
}

fn is_candidate_binary_name(name: &str) -> bool {
    if name.starts_with('.') {
        return false;
    }
    #[cfg(windows)]
    {
        let lower = name.to_ascii_lowercase();
        lower.ends_with(".exe") && !lower[..lower.len() - 4].contains('.')
    }
    #[cfg(not(windows))]
    {
        !name.contains('.')
    }
}

fn auditable_config_path(repo: &Path, locks: &[String]) -> Option<String> {
    for rel in locks {
        let Ok(lock) = sandbox_join_canonical(repo, rel) else {
            continue;
        };
        let Some(dir) = lock.parent() else {
            continue;
        };
        for name in [".cargo/config.toml", ".cargo/config"] {
            let path = dir.join(name);
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if !meta.is_file() || meta.len() > MAX_MANIFEST_BYTES {
                continue;
            }
            let Some(canon) = path.canonicalize().ok() else {
                continue;
            };
            if !canon.starts_with(repo) {
                continue;
            }
            // Bounded and non-following, like every manifest read: the size
            // gate above measured the entry, this measures what is read.
            let Some(text) =
                crate::fs_entry::read_bounded_nofollow(&canon, MAX_MANIFEST_BYTES as usize)
                    .ok()
                    .and_then(|bytes| String::from_utf8(bytes).ok())
            else {
                continue;
            };
            if text.to_ascii_lowercase().contains("auditable") {
                return Some(repo_relative(repo, &canon));
            }
        }
    }
    None
}

fn repo_relative(repo: &Path, path: &Path) -> String {
    path.strip_prefix(repo)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Audits pinned requirements files with `pip-audit --no-deps`.
///
/// Only exact pins are auditable without resolving an environment, so this is
/// deliberately narrower than a full dependency-tree audit; the ecosystems note
/// in the report says so. pip-audit still needs network for the advisory
/// service, exactly like npm audit. Returns true once at least one real
/// pip-audit command was dispatched.
fn enrich_python(
    repo: &Path,
    targets: &ScanTargets,
    report: &mut DepsHealthReport,
    env: &ScanEnv,
) -> bool {
    if !report.pip_audit_present {
        return false;
    }
    let mut ran = false;
    for rel in targets.py_requirements.iter().take(MAX_PY_REQUIREMENTS) {
        let cwd = match audit_cwd(repo, rel) {
            Ok(p) => p,
            Err(msg) => {
                push_issue(
                    &mut report.issues,
                    "error",
                    "audit_cwd",
                    msg,
                    Some(rel.clone()),
                );
                continue;
            }
        };
        // sandbox_join_canonical refuses a symlinked requirements file that
        // points outside the repository, same as every other artifact read.
        let abs_req = match sandbox_join_canonical(repo, rel) {
            Ok(p) => p,
            Err(msg) => {
                push_issue(
                    &mut report.issues,
                    "error",
                    "audit_failed",
                    msg,
                    Some(rel.clone()),
                );
                continue;
            }
        };
        let req_arg = abs_req.to_string_lossy().into_owned();
        let out = capture_scanner_command(
            env,
            pip_audit_program(),
            &[
                "--no-deps",
                "--disable-pip",
                "--progress-spinner",
                "off",
                "--format",
                "json",
                "-r",
                &req_arg,
            ],
            Some(&cwd),
            AUDIT_TIMEOUT,
            &[("NO_COLOR", "1"), ("PIP_AUDIT_PROGRESS_SPINNER", "off")],
        );
        let Some(text) = scanner_stdout(
            out,
            &mut report.issues,
            "pip_audit_failed",
            pip_audit_program(),
            Some(rel.clone()),
        ) else {
            continue;
        };
        match parse_pip_audit_json(&text) {
            Ok(mut vulns) => {
                ran = true;
                mark_audited(report, format!("pip:{rel}"));
                report.vulnerabilities.append(&mut vulns);
            }
            Err(e) => push_issue(
                &mut report.issues,
                "warning",
                "pip_audit_failed",
                e,
                Some(rel.clone()),
            ),
        }
    }
    ran
}

/// Runs `govulncheck -json ./...` at each go.mod root and folds the NDJSON
/// stream into module-level findings. Returns true once at least one real
/// govulncheck command was dispatched.
fn enrich_go(
    repo: &Path,
    targets: &ScanTargets,
    report: &mut DepsHealthReport,
    env: &ScanEnv,
) -> bool {
    if !report.govulncheck_present {
        return false;
    }
    let mut ran = false;
    for rel in targets.go_mods.iter().take(MAX_GO_MODS) {
        let dir = dir_of(rel);
        let cwd = match npm_cwd(repo, &dir) {
            Ok(p) => p,
            Err(msg) => {
                push_issue(
                    &mut report.issues,
                    "error",
                    "audit_cwd",
                    msg,
                    Some(rel.clone()),
                );
                continue;
            }
        };
        let out = capture_scanner_command(
            env,
            govulncheck_program(),
            &["-json", "./..."],
            Some(&cwd),
            AUDIT_TIMEOUT,
            // `GOTOOLCHAIN=local` is the same rule as composer's
            // `--no-plugins`: a scan reads dependency metadata, so the
            // checkout must not get to choose what runs. Loading packages
            // reads the module's own `go` and `toolchain` lines, and under the
            // default `auto` the go command downloads and executes the
            // toolchain those lines name — so a `toolchain` line alone turns a
            // Health scan into fetching and running a binary the repository
            // picked. `local` pins the installed toolchain; a module that
            // genuinely needs a newer one now fails loudly as
            // `govulncheck_failed` instead of quietly switching.
            &[
                ("GOFLAGS", "-mod=readonly"),
                ("GO111MODULE", "on"),
                ("GOTOOLCHAIN", "local"),
            ],
        );
        // `-json` exits 0 whether or not it finds anything; a non-zero exit
        // (killed, build failure, timeout) leaves a stream that may stop on a
        // message boundary and still parse, so it is never trusted.
        if let Ok(o) = &out {
            if !o.success {
                let err = o.stderr_text();
                push_issue(
                    &mut report.issues,
                    "warning",
                    "govulncheck_failed",
                    if err.is_empty() {
                        format!("govulncheck exited {}", o.status_code)
                    } else {
                        err
                    },
                    Some(rel.clone()),
                );
                continue;
            }
        }
        let Some(text) = scanner_stdout(
            out,
            &mut report.issues,
            "govulncheck_failed",
            govulncheck_program(),
            Some(rel.clone()),
        ) else {
            continue;
        };
        match parse_govulncheck_stream(&text) {
            Ok(mut vulns) => {
                ran = true;
                mark_audited(report, format!("go:{rel}"));
                report.vulnerabilities.append(&mut vulns);
            }
            Err(e) => push_issue(
                &mut report.issues,
                "warning",
                "govulncheck_failed",
                e,
                Some(rel.clone()),
            ),
        }
    }
    ran
}

/// Runs `composer audit --locked` against every composer.lock directory.
/// Returns true once at least one real composer command was dispatched.
fn enrich_php(
    repo: &Path,
    targets: &ScanTargets,
    report: &mut DepsHealthReport,
    env: &ScanEnv,
) -> bool {
    if !report.composer_present {
        return false;
    }
    let mut ran = false;
    for rel in targets.composer_locks.iter().take(MAX_COMPOSER_LOCKS) {
        let dir = dir_of(rel);
        let cwd = match npm_cwd(repo, &dir) {
            Ok(p) => p,
            Err(msg) => {
                push_issue(
                    &mut report.issues,
                    "error",
                    "audit_cwd",
                    msg,
                    Some(rel.clone()),
                );
                continue;
            }
        };
        let out = capture_scanner_command(
            env,
            composer_program(),
            // `--no-plugins` and `--no-scripts` are what make this a read of
            // composer.lock rather than a run of the repository. Composer
            // loads plugin packages from the project's own `vendor/` on every
            // startup, `audit` included, and the project's own `composer.json`
            // is what allow-lists them — so without these a Health scan
            // executes checked-in PHP. `npm_config_ignore_scripts` already
            // holds that line for npm; this is the same rule, same reason.
            &[
                "audit",
                "--format=json",
                "--locked",
                "--no-interaction",
                "--no-plugins",
                "--no-scripts",
            ],
            Some(&cwd),
            AUDIT_TIMEOUT,
            &[
                ("COMPOSER_NO_INTERACTION", "1"),
                ("COMPOSER_DISABLE_NETWORK", "0"),
            ],
        );
        let Some(text) = scanner_stdout(
            out,
            &mut report.issues,
            "composer_audit_failed",
            composer_program(),
            Some(rel.clone()),
        ) else {
            continue;
        };
        match parse_composer_audit_json(&text) {
            Ok(mut vulns) => {
                ran = true;
                mark_audited(report, format!("php:{rel}"));
                report.vulnerabilities.append(&mut vulns);
            }
            Err(e) => push_issue(
                &mut report.issues,
                "warning",
                "composer_audit_failed",
                e,
                Some(rel.clone()),
            ),
        }
    }
    ran
}

/// Resolves the directory that owns an audit artifact (repo root when the path
/// has no directory component).
fn audit_cwd(repo: &Path, rel: &str) -> Result<PathBuf, String> {
    let dir = dir_of(rel);
    npm_cwd(repo, &dir)
}

/// Runs `bundler-audit check --format json` against every Gemfile.lock.
///
/// The advisory database is never refreshed here: `bundler-audit update`
/// clones into the user's home directory, which is a state change outside the
/// repository this app has no business making implicitly. A missing DB fails
/// loudly as an issue instead. Returns true once at least one real
/// bundler-audit command was dispatched.
fn enrich_ruby(
    repo: &Path,
    targets: &ScanTargets,
    report: &mut DepsHealthReport,
    env: &ScanEnv,
) -> bool {
    if !report.bundler_audit_present {
        return false;
    }
    let mut ran = false;
    for rel in targets.gemfile_locks.iter().take(MAX_COMPOSER_LOCKS) {
        let dir = dir_of(rel);
        let cwd = match npm_cwd(repo, &dir) {
            Ok(p) => p,
            Err(msg) => {
                push_issue(
                    &mut report.issues,
                    "error",
                    "audit_cwd",
                    msg,
                    Some(rel.clone()),
                );
                continue;
            }
        };
        let out = capture_scanner_command(
            env,
            bundler_audit_program(),
            &["check", "--format", "json"],
            Some(&cwd),
            AUDIT_TIMEOUT,
            &[("NO_COLOR", "1")],
        );
        let Some(text) = scanner_stdout(
            out,
            &mut report.issues,
            "bundler_audit_failed",
            bundler_audit_program(),
            Some(rel.clone()),
        ) else {
            continue;
        };
        match parse_bundler_audit_json(&text) {
            Ok(mut vulns) => {
                ran = true;
                mark_audited(report, format!("ruby:{rel}"));
                report.vulnerabilities.append(&mut vulns);
            }
            Err(e) => push_issue(
                &mut report.issues,
                "warning",
                "bundler_audit_failed",
                e,
                Some(rel.clone()),
            ),
        }
    }
    ran
}

/// Parses `bundler-audit check --format json` output.
///
/// Schema verified against bundler-audit 0.9.3 live capture:
/// `{version, created_at?, results: [{type, gem: {name, version}, advisory:
/// {id, url, title, cvss_v2?, cvss_v3?, cve?, ghsa?, patched_versions[]}}]}`.
/// Severity derives from CVSS when published (`cvss_v3`, falling back to
/// `cvss_v2`) and is [`SEVERITY_UNKNOWN`] otherwise; `results` may be absent
/// or empty for a clean lockfile.
pub(crate) fn parse_bundler_audit_json(text: &str) -> Result<Vec<Vulnerability>, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    let value: Value =
        serde_json::from_str(trimmed).map_err(|e| format!("bundler-audit JSON: {e}"))?;
    let results = match value.get("results") {
        None => return Ok(Vec::new()),
        Some(r) => r
            .as_array()
            .ok_or_else(|| "bundler-audit JSON `results` must be an array".to_string())?,
    };
    let mut out = Vec::new();
    for result in results {
        if !result.is_object() {
            continue;
        }
        let name = result
            .get("gem")
            .and_then(|g| g.get("name"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let installed_version = result
            .get("gem")
            .and_then(|g| g.get("version"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let advisory = result.get("advisory");
        let id = advisory
            .and_then(|a| a.get("id"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        // CVSS v3 outranks v2; both are plain numbers in this schema, unlike
        // cargo audit's string form.
        let severity = ["cvss_v3", "cvss_v2"]
            .iter()
            .find_map(|key| {
                advisory
                    .and_then(|a| a.get(key))
                    .and_then(|v| v.as_f64())
                    .filter(|score| *score > 0.0)
                    .map(|score| cvss_to_severity(&score.to_string()))
            })
            .unwrap_or_else(|| SEVERITY_UNKNOWN.into());
        let title = advisory
            .and_then(|a| opt_json_str(a, "title"))
            .filter(|t| !t.is_empty())
            .or_else(|| (!id.is_empty()).then(|| id.clone()))
            .unwrap_or_else(|| {
                if name.is_empty() {
                    "dependency is vulnerable".to_string()
                } else {
                    format!("{name} is vulnerable")
                }
            });
        let url = opt_json_str(advisory.unwrap_or(&Value::Null), "url").unwrap_or_default();
        let patched: Vec<String> = advisory
            .and_then(|a| a.get("patched_versions"))
            .and_then(|v| v.as_array())
            .map(|rows| {
                rows.iter()
                    .filter_map(|r| r.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        let fix_available = if patched.is_empty() {
            "no".into()
        } else {
            patched.join(", ")
        };
        let mut via = Vec::new();
        if !id.is_empty() {
            via.push(id);
        }
        if let Some(ghsa) = advisory
            .and_then(|a| a.get("ghsa"))
            .and_then(|v| v.as_str())
            .filter(|g| !g.is_empty())
        {
            let ghsa_id = format!("GHSA-{ghsa}");
            if !via.contains(&ghsa_id) {
                via.push(ghsa_id);
            }
        }
        out.push(Vulnerability {
            name,
            severity,
            is_direct: false,
            title,
            url,
            range: format!("=={installed_version}"),
            fix_available,
            via,
            ecosystem: "ruby".into(),
        });
    }
    Ok(out)
}

/// Unwraps a scanner invocation into its stdout for parsing.
///
/// A check that could not run must never look like a check that ran clean:
/// a non-zero exit with no parsable output is recorded as an issue naming the
/// tool's stderr (or exit status when stderr is mute). Empty output is never
/// zero findings, even on exit 0: these scanners always emit at least `[]` or
/// a config message, so emptiness is itself a failed run.
fn scanner_stdout(
    out: Result<crate::engine::git_cli::CapturedOutput, String>,
    issues: &mut Vec<HealthIssue>,
    code: &str,
    program: &str,
    label: Option<String>,
) -> Option<String> {
    match out {
        Ok(o) => {
            let text = o.stdout_text();
            if text.trim().is_empty() {
                let err = o.stderr_text();
                let message = if !err.is_empty() {
                    err
                } else if o.success {
                    // Every wired scanner emits a document (at least `[]` or a
                    // config message) even for a clean tree, so silence is a
                    // run that produced nothing, not zero findings.
                    format!("{program} exited 0 but printed nothing to parse")
                } else {
                    format!("{program} exited {} with no output", o.status_code)
                };
                push_issue(issues, "warning", code, message, label);
                return None;
            }
            Some(text)
        }
        Err(e) => {
            push_issue(issues, "warning", code, e, label);
            None
        }
    }
}

/// Every directory whose npm/bun lockfile needs its own audit: the repository
/// root when it has a `package.json`, plus each distinct directory holding a
/// lockfile (a nested `docs/package-lock.json` is its own audit). Workspace
/// members that share their root's lockfile map to the root's directory, so
/// they do not add roots.
fn npm_scan_roots(manifests: &[NpmManifest]) -> Vec<String> {
    let mut roots: Vec<String> = Vec::new();
    if manifests.iter().any(|m| m.path == "package.json") {
        roots.push(String::new());
    }
    for manifest in manifests {
        if manifest.lockfile.is_none() {
            continue;
        }
        let dir = manifest
            .lockfile_dir
            .clone()
            .unwrap_or_else(|| dir_of(&manifest.path));
        if !roots.contains(&dir) {
            roots.push(dir);
        }
    }
    roots
}

fn dir_of(path: &str) -> String {
    match path.rsplit_once('/') {
        Some((dir, _)) => dir.to_string(),
        None => String::new(),
    }
}

fn package_label(rel_dir: &str) -> String {
    if rel_dir.is_empty() {
        "package.json".into()
    } else {
        format!("{rel_dir}/package.json")
    }
}

fn npm_cwd(repo: &Path, rel_dir: &str) -> Result<PathBuf, String> {
    if rel_dir.is_empty() {
        return Ok(repo.to_path_buf());
    }
    let dest = sandbox_join(repo, rel_dir)?;
    let canon = dest
        .canonicalize()
        .map_err(|e| format!("Cannot access package directory: {e}"))?;
    if !canon.starts_with(repo) {
        return Err("package directory escaped the repository".into());
    }
    if !canon.is_dir() {
        return Err(format!("Not a directory: {}", rel_dir));
    }
    Ok(canon)
}

fn run_npm_json(cwd: &Path, args: &[&str], env: &ScanEnv) -> Result<String, String> {
    run_json_cli(cwd, npm_program(), args, env)
}

/// Runs a CLI that prints JSON on stdout. A non-zero exit with a JSON body is
/// success: `npm audit` and `bun audit` both exit 1 when they find advisories.
/// Empty stdout on failure is an error, never a clean report.
fn run_json_cli(cwd: &Path, program: &str, args: &[&str], env: &ScanEnv) -> Result<String, String> {
    let pinned = if program == npm_program() {
        npm_pinned_config(env)
    } else {
        Vec::new()
    };
    let mut extra: Vec<(&str, &str)> = NPM_SAFE_ENV.to_vec();
    extra.extend(pinned.iter().map(|(k, v)| (*k, v.as_str())));
    let out = capture_scanner_command(env, program, args, Some(cwd), AUDIT_TIMEOUT, &extra)?;
    let stdout = out.stdout_text();
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        if out.success {
            return Ok("{}".into());
        }
        let err = out.stderr_text();
        if err.is_empty() {
            return Err(format!(
                "{program} {} produced no output",
                args.first().unwrap_or(&"")
            ));
        }
        return Err(err);
    }
    Ok(trimmed.to_string())
}

/// Probes `program args` under `env`'s PATH/home view and classifies the run.
///
/// Every spawn goes through [`capture_scanner_command`], so probes resolve
/// exactly like production spawns: PATH first, then the GUI-launch fallback
/// dirs. The classification is [`classify_tool_probe`]'s, shared with the
/// `gh` probe: collapsing "ran but failed" into "not found" is what made a
/// GUI-launched scan report "`npm is not installed`" while
/// `/opt/homebrew/bin/npm` existed with its shebang interpreter missing, and
/// collapsing "never started" into it blamed a busy machine on the toolchain.
fn probe_tool_version(program: &str, args: &[&str], env: &ScanEnv, timeout: Duration) -> ToolProbe {
    let result = capture_scanner_command(env, program, args, None, timeout, &[]);
    // A probe the scan deadline refused or cut never established whether the
    // tool exists. It is `NotRun`, never "not installed" and never "found but
    // failed": the report must not tell the reader to install what is there.
    if let (Some(scan), Err(error)) = (env.scan.as_ref(), result.as_ref()) {
        if error.starts_with(DEADLINE_REFUSED) || scan.deadline_cut_probe(error) {
            scan.state().probes_unresolved = true;
            return ToolProbe::NotRun(error.clone());
        }
    }
    classify_tool_probe(program, result)
}

/// First version line a probe reported, if any. Thin wrapper kept so the
/// presence flags below stay one-liners; diagnostics use [`ToolProbe`].
fn tool_version(program: &str, args: &[&str], env: &ScanEnv) -> Option<String> {
    probe_tool_version(program, args, env, VERSION_TIMEOUT)
        .version()
        .map(str::to_string)
}

/// Message for the `npm_missing` issue — honest about WHICH failure occurred.
///
/// The frontend keys off severity `warning` + code `npm_missing`; only the
/// wording differs between the two failure shapes:
/// - truly unresolved → the long-standing "not installed or not on PATH"
///   wording, unchanged;
/// - resolved but broken (missing node interpreter, exit 127, timeout) → says
///   npm WAS found, includes the resolved path when available, quotes the
///   underlying error, and — because npm cannot run without its Node
///   interpreter — mentions when `node` independently did not resolve while
///   npm itself did.
fn npm_missing_message(npm_probe: &ToolProbe, node_probe: &ToolProbe, env: &ScanEnv) -> String {
    let base = match npm_probe {
        ToolProbe::Present(_) | ToolProbe::NotFound(_) => {
            "npm is not installed or not on PATH; vulnerability and outdated checks did not run. Local lockfile and engine checks still apply.".to_string()
        }
        ToolProbe::FoundButFailed(detail) => {
            // Resolution re-runs the same PATH/fallback lookup the successful
            // spawn would have used; a bare-name passthrough means no absolute
            // location is known, and the message simply omits it.
            let resolved = resolve_spawn_program_in(
                npm_program(),
                env.path(),
                env.home(),
                env.path_source,
            );
            let location = if resolved.contains('/') || resolved.contains('\\') {
                format!(" at {resolved}")
            } else {
                String::new()
            };
            format!(
                "npm was found{location} but could not be run: {detail}. Vulnerability and outdated checks did not run; local lockfile and engine checks still apply."
            )
        }
        // Never started, so whether npm is installed is unknown — and saying
        // it is not would send the reader to fix a toolchain that is fine.
        ToolProbe::NotRun(detail) => format!(
            "npm could not be checked: {detail}. Vulnerability and outdated checks did not run; local lockfile and engine checks still apply."
        ),
    };
    let node_note = match node_probe {
        ToolProbe::NotFound(_) => " The Node.js interpreter (`node`) was not found either — npm is a script that needs `node` to run, so install Node.js."
            .to_string(),
        _ => String::new(),
    };
    format!("{base}{node_note}")
}

/// The programs whose children run with the allowlisted environment.
fn is_js_toolchain_program(program: &str) -> bool {
    matches!(
        program,
        "npm" | "npm.cmd" | "bun" | "bun.exe" | "node" | "node.exe"
    )
}

/// The npm config a scan pins explicitly, so a repository's `.npmrc` cannot
/// choose it: the user's own userconfig and globalconfig files, and the
/// registry the user-level config names (npm's default when it names none).
/// Environment variables outrank a project `.npmrc` in npm's precedence.
fn npm_pinned_config(env: &ScanEnv) -> Vec<(&'static str, String)> {
    let (userconfig, globalconfig) = user_npm_config_paths(env);
    let registry = userconfig
        .as_deref()
        .and_then(user_registry)
        .unwrap_or_else(|| NPM_DEFAULT_REGISTRY.to_string());
    let mut pinned = vec![("npm_config_registry", registry)];
    if let Some(path) = userconfig {
        pinned.push(("npm_config_userconfig", path.to_string_lossy().into_owned()));
    }
    if let Some(path) = globalconfig {
        pinned.push((
            "npm_config_globalconfig",
            path.to_string_lossy().into_owned(),
        ));
    }
    pinned
}

/// Names `${NAME}` references in the pinned user-level npmrc and globalconfig.
/// The user's own config is trusted, so a token it asks npm to expand
/// (`//my.registry/:_authToken=${NPM_TOKEN}`) is forwarded to the child;
/// nothing is taken from the repository's `.npmrc` or `bunfig.toml`, and a
/// variable no pinned file mentions stays scrubbed.
fn user_config_referenced_vars(env: &ScanEnv) -> Vec<String> {
    let (userconfig, globalconfig) = user_npm_config_paths(env);
    let mut names = Vec::new();
    for path in [userconfig, globalconfig].into_iter().flatten() {
        let Ok(bytes) = crate::fs_entry::read_bounded_nofollow(&path, MAX_MANIFEST_BYTES as usize)
        else {
            continue;
        };
        let text = String::from_utf8_lossy(&bytes);
        for line in text.lines().map(str::trim) {
            if line.starts_with(';') || line.starts_with('#') {
                continue;
            }
            let mut rest = line;
            while let Some((_, after)) = rest.split_once("${") {
                let name: String = after
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                    .collect();
                if !name.is_empty() && !names.contains(&name) {
                    names.push(name);
                }
                rest = after;
            }
        }
    }
    names
}

/// The user's own userconfig and globalconfig files: their environment
/// overrides, else `~/.npmrc` and `<node prefix>/etc/npmrc`.
fn user_npm_config_paths(env: &ScanEnv) -> (Option<PathBuf>, Option<PathBuf>) {
    let user_override = |key: &str| std::env::var_os(key).filter(|v| !v.is_empty());
    let userconfig = user_override("npm_config_userconfig")
        .map(PathBuf::from)
        .or_else(|| env.home().map(|home| Path::new(home).join(".npmrc")));
    // npm's builtin globalconfig is `<prefix>/etc/npmrc`, the prefix being two
    // levels above the node binary it runs under.
    let globalconfig = user_override("npm_config_globalconfig")
        .map(PathBuf::from)
        .or_else(|| {
            let node = resolve_spawn_program_in("node", env.path(), env.home(), env.path_source);
            let node = Path::new(&node).canonicalize().ok()?;
            Some(node.parent()?.parent()?.join("etc").join("npmrc"))
        });
    (userconfig, globalconfig)
}

const NPM_DEFAULT_REGISTRY: &str = "https://registry.npmjs.org/";

/// `registry=` from the user's own `.npmrc`, if it sets a plain URL.
fn user_registry(userconfig: &Path) -> Option<String> {
    let bytes =
        crate::fs_entry::read_bounded_nofollow(userconfig, MAX_MANIFEST_BYTES as usize).ok()?;
    let text = String::from_utf8(bytes).ok()?;
    parse_npmrc(&text)
        .into_iter()
        .rev()
        .find(|(key, _)| key == "registry")
        .map(|(_, value)| value)
        .filter(|value| value.starts_with("http") && !value.contains("${"))
}

/// Parses an `.npmrc` the way npm's `ini` reader does, for what the refusal
/// needs: keys, lower-cased and unquoted, with their raw values.
///
/// Lines split on `\r\n`, a lone `\r` and a lone `\n` (npm splits on
/// `[\r\n]+`); a leading BOM is dropped; a key wrapped in single or double
/// quotes is unquoted; a trailing `[]` (npm's array form) is dropped; a line
/// with no `=` is a key set to true; `;` and `#` start a comment line and
/// `[section]` headers carry no key.
fn parse_npmrc(text: &str) -> Vec<(String, String)> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    text.split(['\r', '\n'])
        .map(str::trim)
        .filter(|line| {
            !line.is_empty()
                && !line.starts_with(';')
                && !line.starts_with('#')
                && !(line.starts_with('[') && line.ends_with(']'))
        })
        .map(|line| {
            let (key, value) = line.split_once('=').unwrap_or((line, ""));
            let mut key = key.trim();
            for quote in ['"', '\''] {
                if let Some(inner) = key
                    .strip_prefix(quote)
                    .and_then(|rest| rest.strip_suffix(quote))
                {
                    key = inner.trim();
                    break;
                }
            }
            let key = key.strip_suffix("[]").unwrap_or(key);
            (
                key.trim().to_ascii_lowercase(),
                value.trim().trim_matches(['"', '\'']).to_string(),
            )
        })
        .collect()
}

/// True when a repository `.npmrc` sets repo-controlled transport or credential
/// config: a registry (default or scoped), the proxy family, TLS trust
/// (`strict-ssl`, `ca`, `cafile`, `certfile`, `keyfile`), or a credential
/// (`_auth*`, `_password`, `username`, a scoped `email`, `tokenHelper`).
/// Those decide where a scan sends the dependency tree and what rides along.
///
/// npm 11.20.0 contains no `tokenHelper` code (checked against the installed
/// package), so npm does not execute it; pnpm and yarn do, and a refusal costs
/// nothing here, so it stays.
fn npmrc_overrides_network(text: &str) -> bool {
    parse_npmrc(text).iter().any(|(key, _)| {
        let key = key.as_str();
        key.ends_with("registry")
            || matches!(
                key,
                "proxy"
                    | "https-proxy"
                    | "http-proxy"
                    | "noproxy"
                    | "strict-ssl"
                    | "ca"
                    | "username"
            )
            || [
                "cafile",
                "certfile",
                "keyfile",
                "tokenhelper",
                ":ca",
                ":username",
            ]
            .iter()
            .any(|suffix| key.ends_with(suffix))
            || (key.starts_with("//") && key.ends_with(":email"))
            || key.contains("_auth")
            || key.contains("_password")
    })
}

/// [`npmrc_overrides_network`] for `bunfig.toml`.
///
/// No TOML parser is a direct dependency of this crate, so this fails closed
/// instead of parsing: the text is lower-cased and stripped of whitespace and
/// quotes (so `[ install . scopes ]`, `"registry"` and dotted top-level keys
/// all normalise), and the file is refused if it mentions a registry, a scope,
/// a credential, a proxy or TLS trust anywhere, comments included.
fn bunfig_overrides_network(text: &str) -> bool {
    let flat: String = text
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '"' && *c != '\'')
        .flat_map(char::to_lowercase)
        .collect();
    const MENTIONS: [&str; 12] = [
        "registry", "scopes", "token", "password", "username", "proxy", "cafile", "certfile",
        "keyfile", "tls", "ssl", "email",
    ];
    if MENTIONS.iter().any(|needle| flat.contains(needle)) {
        return true;
    }
    // `ca = "..."` is too short to match by substring: look for it as a key.
    text.split(['\r', '\n', ',', '{'])
        .filter_map(|piece| piece.split_once('='))
        .any(|(key, _)| {
            let key: String = key
                .chars()
                .filter(|c| !c.is_whitespace() && *c != '"' && *c != '\'')
                .collect();
            key.rsplit('.')
                .next()
                .is_some_and(|k| k.eq_ignore_ascii_case("ca"))
        })
}

/// Why a package root's scan must not run: the repository's own config would
/// choose the registry, a credential, a proxy or TLS trust. `Some` carries the
/// code and message of the issue to report; the scan is never reported clean.
///
/// Every directory from the package dir up to the repo root is read (a
/// workspace root or an intermediate parent can hold the `.npmrc` npm honours),
/// through [`read_manifest_bytes`] (sandboxed, bounded, non-following). A config
/// that cannot be read is refused too: unread is not "sets nothing".
fn repo_config_refusal(repo: &Path, rel_dir: &str, bun: bool) -> Option<(&'static str, String)> {
    let mut files = vec![".npmrc"];
    if bun {
        files.push("bunfig.toml");
    }
    let mut dirs: Vec<String> = Vec::new();
    let mut current = rel_dir.trim_matches('/').trim_start_matches("./");
    while !current.is_empty() && current != "." {
        dirs.push(current.to_string());
        current = current.rsplit_once('/').map_or("", |(parent, _)| parent);
    }
    dirs.push(String::new());
    for dir in dirs {
        for name in &files {
            let rel = if dir.is_empty() {
                (*name).to_string()
            } else {
                format!("{dir}/{name}")
            };
            match read_manifest_bytes(repo, &rel) {
                Ok(None) => {}
                Ok(Some(bytes)) => {
                    let text = String::from_utf8_lossy(&bytes);
                    let overrides = if *name == "bunfig.toml" {
                        bunfig_overrides_network(&text)
                    } else {
                        npmrc_overrides_network(&text)
                    };
                    if overrides {
                        return Some(("registry_overridden", REGISTRY_OVERRIDDEN.to_string()));
                    }
                }
                Err(e) => {
                    return Some((
                        "repo_config_unreadable",
                        format!("{e}; the scan did not run because the repository config could not be checked"),
                    ))
                }
            }
        }
    }
    None
}

/// The issue text for a scan refused because of repository registry config.
const REGISTRY_OVERRIDDEN: &str = "registry overridden by repository config";

/// Single choke point for every dependency-scanner spawn so the injectable
/// PATH/home seams apply uniformly — probes and audits resolve identically.
fn capture_scanner_command(
    env: &ScanEnv,
    program: &str,
    args: &[&str],
    cwd: Option<&Path>,
    timeout: Duration,
    extra_env: &[(&str, &str)],
) -> Result<CapturedOutput, String> {
    // The command's own timeout, shortened to what is left of the scan
    // deadline: a command already running at the deadline is cut at it.
    let what = format!("{program} {}", args.first().copied().unwrap_or_default());
    let (timeout, shortened) = match env.scan.as_ref() {
        Some(scan) => match scan.timeout_for(timeout, what.trim_end()) {
            Some(clamped) => clamped,
            None => {
                return Err(format!(
                    "{DEADLINE_REFUSED}`{}` could start",
                    what.trim_end()
                ))
            }
        },
        None => (timeout, false),
    };
    // npm and bun read repository-controlled config and expand `${VAR}` from
    // their environment, so every JS-toolchain child gets an allowlisted one.
    let outcome = if is_js_toolchain_program(program) {
        capture_command_scrubbed(
            program,
            args,
            cwd,
            timeout,
            extra_env,
            env.path(),
            env.home(),
            env.path_source,
            &user_config_referenced_vars(env),
        )
    } else {
        capture_command_with_env(
            program,
            args,
            cwd,
            timeout,
            extra_env,
            env.path(),
            env.home(),
            env.path_source,
        )
    };
    if let Some(scan) = env.scan.as_ref() {
        scan.note_finished(&outcome, what.trim_end(), shortened);
    }
    outcome
}

/// The spelling that reaches cargo-audit, preferring the one a repository
/// cannot redirect.
///
/// `audit` is an external subcommand, not a built-in, so `cargo audit` is
/// resolved against `[alias]` in the checkout's own `.cargo/config.toml`
/// first: `alias.audit = ["run", …]` turns a Health scan into a build and run
/// of the repository's code. Cargo invokes the external binary as
/// `cargo-audit audit …`, so the arguments are identical and no alias table
/// sits in front of it. `cargo install cargo-audit` writes that binary beside
/// `cargo`, so the aliasable spelling is only a fallback for a host that
/// somehow has the subcommand without it.
fn cargo_audit_program(env: &ScanEnv) -> Option<&'static str> {
    ["cargo-audit", "cargo"].into_iter().find(|program| {
        capture_scanner_command(
            env,
            program,
            &["audit", "--version"],
            None,
            VERSION_TIMEOUT,
            &[("CARGO_TERM_COLOR", "never")],
        )
        .map(|o| o.success)
        .unwrap_or(false)
    })
}

fn cargo_audit_available(env: &ScanEnv) -> bool {
    cargo_audit_program(env).is_some()
}

/// Direct `cargo-deny` only. `cargo deny` would honor `[alias] deny` in the
/// checkout and could build or run repository code.
fn cargo_deny_program(env: &ScanEnv) -> Option<&'static str> {
    let ok = capture_scanner_command(
        env,
        "cargo-deny",
        &["--version"],
        None,
        VERSION_TIMEOUT,
        &[("CARGO_TERM_COLOR", "never")],
    )
    .map(|out| out.success)
    .unwrap_or(false);
    ok.then_some("cargo-deny")
}

fn cargo_deny_available(env: &ScanEnv) -> bool {
    cargo_deny_program(env).is_some()
}

/// Direct `cargo-crev` only, for the same alias reason as cargo-deny.
/// `--help` is the probe: this binary does not advertise a root `--version`.
fn cargo_crev_program(env: &ScanEnv) -> Option<&'static str> {
    let ok = capture_scanner_command(
        env,
        "cargo-crev",
        &["crev", "--help"],
        None,
        VERSION_TIMEOUT,
        &[("CARGO_TERM_COLOR", "never")],
    )
    .map(|out| out.success)
    .unwrap_or(false);
    ok.then_some("cargo-crev")
}

fn cargo_crev_available(env: &ScanEnv) -> bool {
    cargo_crev_program(env).is_some()
}

/// `directories::ProjectDirs` for the `crev` application, which is where
/// `cargo crev` reads `config.yaml`. Any of the platform layouts counts.
/// A missing file means verify must not run: the tool would create the
/// identity itself.
fn crev_identity_configured(env: &ScanEnv) -> bool {
    if let Some(root) = std::env::var_os("CARGO_CREV_ROOT_DIR_OVERRIDE") {
        let root = PathBuf::from(root);
        if root.join("config.yaml").is_file() || root.join("crev").join("config.yaml").is_file() {
            return true;
        }
    }
    let Some(home) = env.home() else {
        return false;
    };
    let home = Path::new(home);
    [
        home.join(".config").join("crev").join("config.yaml"),
        home.join("Library")
            .join("Application Support")
            .join("crev")
            .join("config.yaml"),
        home.join("AppData")
            .join("Roaming")
            .join("crev")
            .join("config.yaml"),
    ]
    .iter()
    .any(|path| path.is_file())
}

/// npm's binary name for this platform.
///
/// Canonical for the crate: [`crate::ci_local`] plans `npm` CI steps and calls
/// this rather than keeping a second copy — a fact spelled twice can disagree
/// on Windows without either side failing loudly.
pub(crate) fn npm_program() -> &'static str {
    if cfg!(windows) {
        "npm.cmd"
    } else {
        "npm"
    }
}

fn bun_program() -> &'static str {
    if cfg!(windows) {
        "bun.exe"
    } else {
        "bun"
    }
}

fn node_program() -> &'static str {
    if cfg!(windows) {
        "node.exe"
    } else {
        "node"
    }
}

fn list_repo_files(repo: &Path) -> Result<(Vec<String>, Option<ScanLimitNotice>), String> {
    let stdout = git_text(
        repo,
        &[
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ],
    )?;
    let mut out = Vec::new();
    for rel in stdout.split('\0') {
        let rel = LanguageDetector::normalize_rel_path(rel);
        if rel.is_empty() || skip_source(&rel) {
            continue;
        }
        out.push(rel);
    }
    let limit = if out.len() > MAX_SOURCE_FILES {
        let total = out.len();
        out.sort_by(|a, b| {
            let ea = LanguageDetector::ecosystem_hint(a).is_some();
            let eb = LanguageDetector::ecosystem_hint(b).is_some();
            eb.cmp(&ea).then_with(|| a.cmp(b))
        });
        out.truncate(MAX_SOURCE_FILES);
        Some(ScanLimitNotice {
            resource: "repository files".into(),
            kept: MAX_SOURCE_FILES,
            total,
            inventory_only: false,
        })
    } else {
        None
    };
    Ok((out, limit))
}

/// Reads a manifest the one safe way: resolve it inside the repository
/// (`sandbox_join_canonical`), then open the resolved path without following a
/// link, non-blocking, and read at most [`MAX_MANIFEST_BYTES`] from the opened
/// descriptor. The size gate therefore measures the file actually read (not a
/// symlink's own length), a FIFO or device is refused instead of parking the
/// caller, and a file that grows after the open is cut off.
///
/// `Ok(None)` is "no manifest here" (absent, or a directory). A file that is
/// present but refused (oversized, not regular, unreadable) is an `Err` the
/// caller reports, never an empty result.
fn read_manifest_bytes(repo: &Path, rel: &str) -> Result<Option<Vec<u8>>, String> {
    use crate::fs_entry::{read_bounded_nofollow, BoundedRead};
    let canon = sandbox_join_canonical(repo, rel)?;
    match read_bounded_nofollow(&canon, MAX_MANIFEST_BYTES as usize) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(BoundedRead::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(BoundedRead::NotRegular) if canon.is_dir() => Ok(None),
        Err(BoundedRead::NotRegular) => Err(format!("{rel} is not a regular file")),
        Err(BoundedRead::TooLarge | BoundedRead::Grew) => {
            Err(format!("{rel} exceeds {MAX_MANIFEST_BYTES} byte limit"))
        }
        Err(BoundedRead::Io(e)) => Err(format!("Cannot read {rel}: {e}")),
    }
}

fn read_npm_manifest(repo: &Path, rel: &str) -> Result<Option<NpmManifest>, String> {
    let Some(bytes) = read_manifest_bytes(repo, rel)? else {
        return Ok(None);
    };
    if bytes.contains(&0) {
        return Err(format!("{rel} is binary"));
    }
    let text = String::from_utf8_lossy(&bytes);
    let value: Value = serde_json::from_str(text.trim_start_matches('\u{feff}'))
        .map_err(|e| format!("{rel} is not valid JSON: {e}"))?;
    Ok(Some(manifest_from_json(rel, &value, repo)))
}

fn manifest_from_json(rel: &str, value: &Value, repo: &Path) -> NpmManifest {
    let dir = dir_of(rel);
    let (lockfile, lockfile_dir) = match resolve_lockfile(repo, &dir) {
        Some((name, lock_dir)) => (Some(name), Some(lock_dir)),
        None => (None, None),
    };
    let package_manager = declared_package_manager(value)
        .or_else(|| lockfile.as_deref().map(lockfile_to_manager))
        .unwrap_or("npm")
        .to_string();
    let scripts = value.get("scripts").and_then(|s| s.as_object());
    let mut lifecycle = Vec::new();
    if let Some(scripts) = scripts {
        for name in LIFECYCLE_SCRIPTS {
            if scripts.contains_key(*name) {
                lifecycle.push((*name).to_string());
            }
        }
    }
    NpmManifest {
        path: rel.to_string(),
        name: value
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        version: value
            .get("version")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        private: value
            .get("private")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        license: value
            .get("license")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        engines_node: value
            .get("engines")
            .and_then(|e| e.get("node"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        package_manager,
        lockfile,
        lockfile_dir,
        has_workspaces: value
            .get("workspaces")
            .map(|w| w.is_array() || w.is_object())
            .unwrap_or(false),
        dep_count: object_len(value.get("dependencies")),
        dev_dep_count: object_len(value.get("devDependencies")),
        optional_dep_count: object_len(value.get("optionalDependencies")),
        peer_dep_count: object_len(value.get("peerDependencies")),
        lifecycle_scripts: lifecycle,
    }
}

fn object_len(value: Option<&Value>) -> usize {
    value
        .and_then(|v| v.as_object())
        .map(|o| o.len())
        .unwrap_or(0)
}

fn declared_package_manager(value: &Value) -> Option<&'static str> {
    let raw = value.get("packageManager")?.as_str()?.to_ascii_lowercase();
    if raw.starts_with("pnpm") {
        Some("pnpm")
    } else if raw.starts_with("yarn") {
        Some("yarn")
    } else if raw.starts_with("bun") {
        Some("bun")
    } else if raw.starts_with("npm") {
        Some("npm")
    } else {
        None
    }
}

/// The lockfile that covers the package in `dir`, and the directory holding it.
///
/// A package's own directory wins. A workspace member without one is covered
/// by the nearest ancestor that declares workspaces (`package.json`
/// `workspaces`, or `pnpm-workspace.yaml`) and has a lockfile: npm, yarn,
/// pnpm and bun all keep one lockfile at the workspace root. An ancestor that
/// merely happens to hold a lockfile does not cover an unrelated package.
fn resolve_lockfile(repo: &Path, dir: &str) -> Option<(String, String)> {
    if let Some(name) = detect_lockfile(repo, dir) {
        return Some((name, dir.to_string()));
    }
    let mut current = dir.to_string();
    while !current.is_empty() {
        current = dir_of(&current);
        if !declares_workspaces(repo, &current) {
            continue;
        }
        if let Some(name) = detect_lockfile(repo, &current) {
            return Some((name, current));
        }
    }
    None
}

fn declares_workspaces(repo: &Path, dir: &str) -> bool {
    let join = |name: &str| {
        if dir.is_empty() {
            name.to_string()
        } else {
            format!("{dir}/{name}")
        }
    };
    if regular_file_in_repo(repo, &join("pnpm-workspace.yaml")) {
        return true;
    }
    // A refused manifest (oversized, not regular, outside the repository)
    // declares nothing here. This is a yes/no question with no issue list to
    // report into; a tracked ancestor manifest is itself a scan candidate, and
    // `read_npm_manifest` reports its refusal there.
    read_manifest_bytes(repo, &join("package.json"))
        .ok()
        .flatten()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .and_then(|text| serde_json::from_str::<Value>(text.trim_start_matches('\u{feff}')).ok())
        .and_then(|value| {
            value
                .get("workspaces")
                .map(|w| w.is_array() || w.is_object())
        })
        .unwrap_or(false)
}

fn detect_lockfile(repo: &Path, dir: &str) -> Option<String> {
    const CANDIDATES: &[&str] = &[
        "package-lock.json",
        "npm-shrinkwrap.json",
        "yarn.lock",
        "pnpm-lock.yaml",
        "bun.lock",
        "bun.lockb",
    ];
    for name in CANDIDATES {
        let rel = if dir.is_empty() {
            (*name).to_string()
        } else {
            format!("{dir}/{name}")
        };
        if regular_file_in_repo(repo, &rel) {
            return Some((*name).to_string());
        }
    }
    None
}

/// True when `rel` resolves, inside the repository, to a regular file. The
/// same resolution as [`read_manifest_bytes`] (`sandbox_join_canonical`, then a
/// non-blocking no-follow open that classifies the descriptor), without
/// reading: a link out of the repository, a FIFO or a socket never counts, so
/// a file outside the repo cannot change what a scan concludes.
fn regular_file_in_repo(repo: &Path, rel: &str) -> bool {
    sandbox_join_canonical(repo, rel)
        .is_ok_and(|canon| crate::fs_entry::open_regular_nofollow(&canon).is_ok())
}

fn lockfile_to_manager(name: &str) -> &'static str {
    match name {
        "yarn.lock" => "yarn",
        "pnpm-lock.yaml" => "pnpm",
        "bun.lock" | "bun.lockb" => "bun",
        _ => "npm",
    }
}

/// Which auditor can actually read this lockfile. npm's own audit exits
/// `ENOLOCK` for every other kind, including a missing lockfile.
#[derive(Clone, Copy, PartialEq, Eq)]
enum JsLockKind {
    Npm,
    Bun,
    Unsupported,
    Missing,
}

fn js_lock_kind(lockfile: Option<&str>) -> JsLockKind {
    match lockfile {
        Some("package-lock.json" | "npm-shrinkwrap.json") => JsLockKind::Npm,
        Some("bun.lock" | "bun.lockb") => JsLockKind::Bun,
        Some(_) => JsLockKind::Unsupported,
        None => JsLockKind::Missing,
    }
}

fn js_audits_satisfied(report: &DepsHealthReport, ran: &dyn Fn(&str) -> bool) -> bool {
    report.manifests.iter().all(|manifest| {
        // The audit that covers a manifest is the one over the directory that
        // holds its lockfile (its own, or its workspace root's), and it must
        // have parsed — not merely some audit having run somewhere.
        let lock_dir = manifest
            .lockfile_dir
            .clone()
            .unwrap_or_else(|| dir_of(&manifest.path));
        let audited = report.audited_targets.contains(&format!("npm:{lock_dir}"));
        match js_lock_kind(manifest.lockfile.as_deref()) {
            JsLockKind::Npm => report.npm_cli_present && ran("npm") && audited,
            JsLockKind::Bun => report.bun_cli_present && ran("bun") && audited,
            JsLockKind::Unsupported | JsLockKind::Missing => false,
        }
    })
}

fn skip_source(path: &str) -> bool {
    LanguageDetector::is_ignored_source_path(path)
}

fn push_issue(
    issues: &mut Vec<HealthIssue>,
    severity: &str,
    code: &str,
    message: String,
    path: Option<String>,
) {
    if issues
        .iter()
        .any(|i| i.code == code && i.path == path && i.message == message)
    {
        return;
    }
    issues.push(HealthIssue {
        severity: severity.to_string(),
        code: code.to_string(),
        message,
        path,
    });
}

fn record_limit(report: &mut DepsHealthReport, resource: &str, kept: usize, total: usize) {
    if total <= kept {
        return;
    }
    report.truncated = true;
    report.limit_notices.push(ScanLimitNotice {
        resource: resource.into(),
        kept,
        total,
        inventory_only: false,
    });
}

fn sort_report(report: &mut DepsHealthReport) {
    report.manifests.sort_by(|a, b| a.path.cmp(&b.path));
    report.vulnerabilities.sort_by(|a, b| {
        severity_rank(&a.severity)
            .cmp(&severity_rank(&b.severity))
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.title.cmp(&b.title))
    });
    report.outdated.sort_by(|a, b| a.name.cmp(&b.name));
}

fn cap_report(report: &mut DepsHealthReport) {
    if report.vulnerabilities.len() > MAX_VULNS {
        let total = report.vulnerabilities.len();
        record_limit(report, "vulnerabilities", MAX_VULNS, total);
        report.vulnerabilities.truncate(MAX_VULNS);
    }
    if report.outdated.len() > MAX_OUTDATED {
        let total = report.outdated.len();
        record_limit(report, "outdated npm packages", MAX_OUTDATED, total);
        report.outdated.truncate(MAX_OUTDATED);
    }
    if report.issues.len() > MAX_ISSUES {
        let total = report.issues.len();
        record_limit(report, "health issues", MAX_ISSUES, total);
        report.issues.truncate(MAX_ISSUES);
    }
}

/// Every audit target found, and which of them a scanner actually covered.
fn audit_coverage(report: &DepsHealthReport, targets: &ScanTargets) -> AuditCoverage {
    let mut found: Vec<String> = Vec::new();
    for dir in npm_scan_roots(&report.manifests) {
        found.push(format!("npm:{dir}"));
    }
    for (prefix, list) in [
        ("cargo", &targets.cargo_locks),
        ("pip", &targets.py_requirements),
        ("go", &targets.go_mods),
        ("php", &targets.composer_locks),
        ("ruby", &targets.gemfile_locks),
    ] {
        found.extend(list.iter().map(|rel| format!("{prefix}:{rel}")));
    }
    let audited = found
        .iter()
        .filter(|key| report.audited_targets.contains(key))
        .count();
    let mut unaudited: Vec<String> = found
        .iter()
        .filter(|key| !report.audited_targets.contains(key))
        .cloned()
        .collect();
    let total = found.len() + targets.unaudited.len();
    unaudited.extend(
        targets
            .unaudited
            .iter()
            .map(|(family, rel)| format!("{family}:{rel}")),
    );
    AuditCoverage {
        found: total,
        audited,
        unaudited,
    }
}

fn mark_audited(report: &mut DepsHealthReport, key: String) {
    if !report.audited_targets.contains(&key) {
        report.audited_targets.push(key);
    }
}

fn audit_is_complete(report: &DepsHealthReport, targets: &ScanTargets, run_cli: bool) -> bool {
    let coverage_limited = report
        .limit_notices
        .iter()
        .any(|notice| !notice.inventory_only)
        || (report.truncated && report.limit_notices.is_empty());
    if !run_cli || coverage_limited {
        return false;
    }
    let ran = |name: &str| report.scanners_ran.iter().any(|scanner| scanner == name);
    let requirements = [
        (
            !report.manifests.is_empty(),
            js_audits_satisfied(report, &ran),
        ),
        (
            !targets.cargo_locks.is_empty(),
            report.cargo_audit_present && ran("cargo"),
        ),
        (
            !targets.deny_configs.is_empty(),
            report.cargo_deny_present && ran("cargo-deny"),
        ),
        (
            !targets.py_requirements.is_empty(),
            report.pip_audit_present && ran("pip-audit"),
        ),
        (
            !targets.go_mods.is_empty(),
            report.govulncheck_present && ran("govulncheck"),
        ),
        (
            !targets.composer_locks.is_empty(),
            report.composer_present && ran("composer"),
        ),
        (
            !targets.gemfile_locks.is_empty(),
            report.bundler_audit_present && ran("bundler-audit"),
        ),
    ];
    let has_target = requirements.iter().any(|(expected, _)| *expected);
    if !has_target
        || requirements
            .iter()
            .any(|(expected, completed)| *expected && !*completed)
    {
        return false;
    }
    // Both numbers must agree: a found target nobody audited — a second npm
    // lockfile, a uv.lock, a family with no scanner — blocks completeness even
    // though every family that did get a scanner ran.
    if !report.audit_coverage.unaudited.is_empty()
        || report.audit_coverage.audited != report.audit_coverage.found
    {
        return false;
    }
    let failure_codes = [
        "audit_cwd",
        "audit_failed",
        "bun_audit_failed",
        "js_lockfile_unaudited",
        "cargo_audit_failed",
        "cargo_deny_failed",
        "cargo_crev_failed",
        "cargo_audit_bin_failed",
        "pip_audit_failed",
        "govulncheck_failed",
        "composer_audit_failed",
        "bundler_audit_failed",
    ];
    !report
        .issues
        .iter()
        .any(|issue| failure_codes.contains(&issue.code.as_str()))
}

pub(crate) fn severity_rank(severity: &str) -> u8 {
    match severity {
        "critical" => 0,
        "high" => 1,
        "moderate" | "medium" => 2,
        "low" => 3,
        _ => 4,
    }
}

/// Parses `npm audit --json` (report version 2, version 1, and npm error objects).
pub(crate) fn parse_npm_audit_json(
    text: &str,
) -> Result<(Vec<Vulnerability>, Option<String>), String> {
    let value: Value =
        serde_json::from_str(text.trim()).map_err(|e| format!("npm audit JSON: {e}"))?;
    if let Some(err) = npm_error_message(&value) {
        return Ok((Vec::new(), Some(err)));
    }
    if value
        .get("vulnerabilities")
        .and_then(|v| v.as_object())
        .is_some()
    {
        let vulns = parse_npm_audit_v2(&value);
        // npm states its own count in `metadata`. Finding nothing while npm
        // says there is something means this parser stopped understanding the
        // format — and silently reporting a clean audit is the one outcome a
        // security check must never produce by accident. Only the
        // unambiguous direction is flagged: npm counts advisories and this
        // counts affected packages, so the two need not be equal, but "npm
        // found some, we found none" cannot be a counting difference.
        let reported_total = value
            .pointer("/metadata/vulnerabilities/total")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        if reported_total > 0 && vulns.is_empty() {
            return Ok((
                vulns,
                Some(format!(
                    "npm audit reported {reported_total} vulnerabilities but none could be read from the report; treat this as unscanned, not clean"
                )),
            ));
        }
        return Ok((vulns, None));
    }
    if value
        .get("advisories")
        .and_then(|v| v.as_object())
        .is_some()
    {
        return Ok((parse_npm_audit_v1(&value), None));
    }
    if value.is_object() && value.as_object().map(|o| o.is_empty()).unwrap_or(false) {
        return Ok((Vec::new(), None));
    }
    Err("unrecognised npm audit JSON".into())
}

/// Parses `bun audit --json`.
///
/// A clean audit is `{}`. Findings are an object keyed by package name whose
/// values are advisory arrays (`id`, `url`, `title`, `severity`,
/// `vulnerable_versions`), captured from Bun 1.4.2 against the npm advisory
/// endpoint. Any other shape is an error: an empty list must never be how a
/// report this parser stopped understanding comes back.
pub(crate) fn parse_bun_audit_json(text: &str) -> Result<Vec<Vulnerability>, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err("bun audit produced no output".into());
    }
    let value: Value = serde_json::from_str(trimmed).map_err(|e| format!("bun audit JSON: {e}"))?;
    let Some(map) = value.as_object() else {
        return Err("bun audit JSON must be an object keyed by package name".into());
    };
    let mut out = Vec::new();
    for (name, advisories) in map {
        let Some(rows) = advisories.as_array() else {
            return Err(format!(
                "bun audit entry for {name} is not an advisory list; treat this as unscanned, not clean"
            ));
        };
        for row in rows {
            let Some(row) = row.as_object() else {
                return Err(format!(
                    "bun audit advisory for {name} is not an object; treat this as unscanned, not clean"
                ));
            };
            let title = row
                .get("title")
                .and_then(|v| v.as_str())
                .filter(|title| !title.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| format!("{name} is vulnerable"));
            let severity = row
                .get("severity")
                .and_then(|v| v.as_str())
                .filter(|severity| !severity.is_empty())
                .unwrap_or(SEVERITY_UNKNOWN)
                .to_string();
            let url = row
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let range = row
                .get("vulnerable_versions")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let id = match row.get("id") {
                Some(Value::Number(n)) => n.to_string(),
                Some(Value::String(s)) => s.clone(),
                _ => String::new(),
            };
            out.push(Vulnerability {
                name: name.clone(),
                severity,
                is_direct: false,
                title,
                url,
                range,
                fix_available: "unknown".into(),
                via: if id.is_empty() { Vec::new() } else { vec![id] },
                ecosystem: "npm".into(),
            });
        }
    }
    Ok(out)
}

fn npm_error_message(value: &Value) -> Option<String> {
    let err = value.get("error")?;
    let summary = err.get("summary").and_then(|v| v.as_str());
    let detail = err.get("detail").and_then(|v| v.as_str());
    let code = err.get("code").and_then(|v| v.as_str());
    let mut parts = Vec::new();
    if let Some(c) = code {
        parts.push(c.to_string());
    }
    if let Some(s) = summary {
        parts.push(s.to_string());
    }
    if let Some(d) = detail {
        parts.push(d.to_string());
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" — "))
    }
}

fn parse_npm_audit_v2(value: &Value) -> Vec<Vulnerability> {
    let Some(map) = value.get("vulnerabilities").and_then(|v| v.as_object()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (name, entry) in map {
        let severity = entry
            .get("severity")
            .and_then(|v| v.as_str())
            .unwrap_or("info")
            .to_string();
        let is_direct = entry
            .get("isDirect")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let range = entry
            .get("range")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let (title, url, via_names) = via_details(entry.get("via"));
        let title = if title.is_empty() {
            format!("{name} is vulnerable")
        } else {
            title
        };
        out.push(Vulnerability {
            name: entry
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or(name)
                .to_string(),
            severity,
            is_direct,
            title,
            url,
            range,
            fix_available: format_fix_available(entry.get("fixAvailable")),
            via: via_names,
            ecosystem: "npm".into(),
        });
    }
    out
}

fn via_details(via: Option<&Value>) -> (String, String, Vec<String>) {
    let mut title = String::new();
    let mut url = String::new();
    let mut names = Vec::new();
    let Some(via) = via.and_then(|v| v.as_array()) else {
        return (title, url, names);
    };
    for item in via {
        match item {
            Value::String(s) => {
                if !names.iter().any(|n| n == s) {
                    names.push(s.clone());
                }
            }
            Value::Object(obj) => {
                if title.is_empty() {
                    if let Some(t) = obj.get("title").and_then(|v| v.as_str()) {
                        title = t.to_string();
                    }
                }
                if url.is_empty() {
                    if let Some(u) = obj.get("url").and_then(|v| v.as_str()) {
                        url = u.to_string();
                    }
                }
                if let Some(n) = obj.get("name").and_then(|v| v.as_str()) {
                    if !names.iter().any(|x| x == n) {
                        names.push(n.to_string());
                    }
                }
            }
            _ => {}
        }
    }
    (title, url, names)
}

fn format_fix_available(value: Option<&Value>) -> String {
    match value {
        Some(Value::Bool(true)) => "yes".into(),
        Some(Value::Bool(false)) | None => "no".into(),
        Some(Value::Object(obj)) => {
            let name = obj.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let version = obj.get("version").and_then(|v| v.as_str()).unwrap_or("");
            let major = obj
                .get("isSemVerMajor")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if name.is_empty() {
                "yes".into()
            } else if major {
                format!("{name}@{version} (breaking)")
            } else {
                format!("{name}@{version}")
            }
        }
        Some(_) => "unknown".into(),
    }
}

fn parse_npm_audit_v1(value: &Value) -> Vec<Vulnerability> {
    let Some(map) = value.get("advisories").and_then(|v| v.as_object()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (_id, entry) in map {
        let findings = entry.get("findings").and_then(|v| v.as_array());
        let is_direct = findings
            .and_then(|rows| rows.first())
            .and_then(|f| f.get("paths"))
            .and_then(|p| p.as_array())
            .and_then(|p| p.first())
            .and_then(|p| p.as_str())
            .map(|p| !p.contains('>'))
            .unwrap_or(false);
        out.push(Vulnerability {
            name: entry
                .get("module_name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            severity: entry
                .get("severity")
                .and_then(|v| v.as_str())
                .unwrap_or("info")
                .to_string(),
            is_direct,
            title: entry
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            url: entry
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            range: entry
                .get("vulnerable_versions")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            fix_available: entry
                .get("patched_versions")
                .and_then(|v| v.as_str())
                .filter(|s| *s != "<0.0.0")
                .map(|s| s.to_string())
                .unwrap_or_else(|| "no".into()),
            via: Vec::new(),
            ecosystem: "npm".into(),
        });
    }
    out
}

/// Parses `npm outdated --json`. An empty object means everything is current.
pub(crate) fn parse_npm_outdated_json(text: &str) -> Result<Vec<OutdatedPackage>, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    let value: Value =
        serde_json::from_str(trimmed).map_err(|e| format!("npm outdated JSON: {e}"))?;
    if let Some(err) = npm_error_message(&value) {
        return Err(err);
    }
    let Some(map) = value.as_object() else {
        return Err("npm outdated JSON must be an object".into());
    };
    let mut out = Vec::new();
    for (name, entry) in map {
        if !entry.is_object() {
            continue;
        }
        out.push(OutdatedPackage {
            name: name.clone(),
            current: json_str(entry, "current"),
            wanted: json_str(entry, "wanted"),
            latest: json_str(entry, "latest"),
            dep_type: json_str(entry, "type"),
            location: json_str(entry, "location"),
        });
    }
    Ok(out)
}

/// The package.json sections that declare a dependency, keyed by package name.
///
/// npm 7 removed the `type` field from `npm outdated --json` — it reports
/// `dependent` instead — so from npm 7 onward the JSON simply does not say
/// what kind of dependency a package is. Every npm since 2020 is affected, and
/// the manifest is the only local source left. Read here rather than derived
/// from the `location` path, which says where a package was installed and not
/// how it was asked for.
///
/// An absent or unparsable manifest yields an empty map, leaving `dep_type`
/// blank exactly as it was before — degrading to "unknown", never to a guess.
/// One that is present but refused (oversized, not a regular file, outside the
/// repository) is an `Err` the caller reports.
fn declared_dependency_sections(
    repo: &Path,
    manifest_path: &Path,
) -> Result<BTreeMap<String, String>, String> {
    const SECTIONS: [&str; 4] = [
        "dependencies",
        "devDependencies",
        "optionalDependencies",
        "peerDependencies",
    ];
    let mut sections = BTreeMap::new();
    let rel = repo_relative(repo, manifest_path);
    let Some(bytes) = read_manifest_bytes(repo, &rel)? else {
        return Ok(sections);
    };
    let Ok(value) = serde_json::from_slice::<Value>(&bytes) else {
        return Ok(sections);
    };
    for section in SECTIONS {
        let Some(entries) = value.get(section).and_then(|v| v.as_object()) else {
            continue;
        };
        for name in entries.keys() {
            // First section wins, so a package listed in both `dependencies`
            // and `devDependencies` reports the stronger of the two rather
            // than whichever was visited last.
            sections
                .entry(name.clone())
                .or_insert_with(|| section.to_string());
        }
    }
    Ok(sections)
}

/// The declared-section map for the manifest in `cwd`, with a refused manifest
/// reported as an issue (and the map left empty) instead of read.
///
/// The issue is the one `local_scan` reports for the same manifest: same code,
/// same severity (`error`: a manifest was refused and what depends on it is
/// unexamined, not merely blank), and the manifest's own repo-relative path,
/// so `push_issue` collapses it with that report and across the two
/// dispatchers instead of repeating it per call.
fn declared_sections_or_issue(
    repo: &Path,
    cwd: &Path,
    report: &mut DepsHealthReport,
) -> BTreeMap<String, String> {
    let manifest = cwd.join("package.json");
    declared_dependency_sections(repo, &manifest).unwrap_or_else(|message| {
        push_issue(
            &mut report.issues,
            "error",
            "manifest_unreadable",
            message,
            Some(repo_relative(repo, &manifest)),
        );
        BTreeMap::new()
    })
}

/// Fill in `dep_type` for rows npm did not label.
///
/// Kept separate from parsing so the parser stays a pure function of the JSON:
/// an npm 6 payload that still carries `type` keeps its own value, and this
/// only supplies what the newer format dropped.
fn fill_dependency_types(rows: &mut [OutdatedPackage], sections: &BTreeMap<String, String>) {
    for row in rows.iter_mut() {
        if row.dep_type.is_empty() {
            if let Some(section) = sections.get(&row.name) {
                row.dep_type = section.clone();
            }
        }
    }
}

fn json_str(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

fn opt_json_str(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// First present, non-empty string field among `keys`.
fn first_non_empty(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|k| opt_json_str(value, k).filter(|s| !s.is_empty()))
}

pub(crate) fn parse_cargo_audit_json(
    text: &str,
) -> Result<(Vec<Vulnerability>, Vec<HealthIssue>), String> {
    let value: Value =
        serde_json::from_str(text.trim()).map_err(|e| format!("cargo audit JSON: {e}"))?;
    // A document without a vulnerabilities list (`{"error": ...}`, a schema
    // change) is a run that reported nothing, not a clean tree.
    let list = value
        .pointer("/vulnerabilities/list")
        .and_then(|v| v.as_array())
        .cloned()
        .ok_or_else(|| {
            let detail = value
                .get("error")
                .and_then(|e| e.as_str())
                .map(|e| format!(": {e}"))
                .unwrap_or_default();
            format!("cargo audit JSON has no vulnerabilities list{detail}")
        })?;
    if let Some(count) = value
        .pointer("/vulnerabilities/count")
        .and_then(|c| c.as_u64())
    {
        if count != list.len() as u64 {
            return Err(format!(
                "cargo audit JSON reports {count} vulnerabilities but lists {}",
                list.len()
            ));
        }
    }
    let mut out = Vec::new();
    for item in list {
        let advisory = item.get("advisory").unwrap_or(&item);
        let pkg = advisory
            .get("package")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let id = advisory
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let title = advisory
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or(id.as_str())
            .to_string();
        let url = advisory
            .get("url")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| {
                if id.is_empty() {
                    String::new()
                } else {
                    format!("https://rustsec.org/advisories/{id}")
                }
            });
        let severity = advisory
            .get("cvss")
            .and_then(|v| v.as_str())
            .map(cvss_to_severity)
            .unwrap_or_else(|| SEVERITY_UNKNOWN.into());
        out.push(Vulnerability {
            name: pkg,
            severity,
            is_direct: true,
            title,
            url,
            range: id,
            fix_available: item
                .pointer("/versions/patched")
                .and_then(|v| v.as_array())
                .and_then(|a| a.first())
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| "see advisory".into()),
            via: Vec::new(),
            ecosystem: "cargo".into(),
        });
    }
    let mut warnings = Vec::new();
    if let Some(groups) = value
        .get("warnings")
        .and_then(|warnings| warnings.as_object())
    {
        for (kind, entries) in groups {
            let Some(entries) = entries.as_array() else {
                continue;
            };
            for entry in entries {
                let advisory = entry.get("advisory").unwrap_or(entry);
                let package = entry
                    .pointer("/package/name")
                    .and_then(|value| value.as_str())
                    .or_else(|| advisory.get("package").and_then(|value| value.as_str()))
                    .unwrap_or("unknown crate");
                let version = entry
                    .pointer("/package/version")
                    .and_then(|value| value.as_str())
                    .unwrap_or("");
                let id = json_str(advisory, "id");
                let title = json_str(advisory, "title");
                let url = json_str(advisory, "url");
                let mut message = format!(
                    "{package}{version_suffix}",
                    version_suffix = if version.is_empty() {
                        String::new()
                    } else {
                        format!(" {version}")
                    }
                );
                if !title.is_empty() {
                    message.push_str(&format!(": {title}"));
                }
                if !id.is_empty() {
                    message.push_str(&format!(" ({id})"));
                }
                if !url.is_empty() {
                    message.push_str(&format!(" — {url}"));
                }
                warnings.push(HealthIssue {
                    severity: "warning".into(),
                    code: format!("cargo_audit_{}", kind.replace('-', "_")),
                    message,
                    path: None,
                });
            }
        }
    }
    Ok((out, warnings))
}

/// Maps a CVSS score (`"7.5"`) or a CVSS v3 vector string
/// (`"CVSS:3.1/AV:N/..."`, what RustSec publishes) to a severity bucket.
///
/// Anything that cannot be scored — a v4 vector, a malformed vector, a zero
/// score — is [`SEVERITY_UNKNOWN`], never a guessed rating.
fn cvss_to_severity(score: &str) -> String {
    let n = match score.trim().parse::<f64>() {
        Ok(n) => n,
        Err(_) => cvss3_base_score(score).unwrap_or(0.0),
    };
    if n >= 9.0 {
        "critical".into()
    } else if n >= 7.0 {
        "high".into()
    } else if n >= 4.0 {
        "moderate".into()
    } else if n > 0.0 {
        "low".into()
    } else {
        SEVERITY_UNKNOWN.into()
    }
}

/// CVSS v3.0/v3.1 base score from a vector string (FIRST specification,
/// section 7.1), or `None` when the vector is not a complete v3 base vector.
fn cvss3_base_score(vector: &str) -> Option<f64> {
    let mut parts = vector.trim().split('/');
    let head = parts.next()?;
    if head != "CVSS:3.0" && head != "CVSS:3.1" {
        return None;
    }
    let (mut av, mut ac, mut pr, mut ui, mut scope, mut c, mut i, mut a) =
        (None, None, None, None, None, None, None, None);
    for metric in parts {
        let (key, value) = metric.split_once(':')?;
        let slot = match key {
            "AV" => &mut av,
            "AC" => &mut ac,
            "PR" => &mut pr,
            "UI" => &mut ui,
            "S" => &mut scope,
            "C" => &mut c,
            "I" => &mut i,
            "A" => &mut a,
            // Temporal and environmental metrics do not move the base score.
            _ => continue,
        };
        *slot = Some(value);
    }
    let changed = match scope? {
        "U" => false,
        "C" => true,
        _ => return None,
    };
    let av = match av? {
        "N" => 0.85,
        "A" => 0.62,
        "L" => 0.55,
        "P" => 0.2,
        _ => return None,
    };
    let ac = match ac? {
        "L" => 0.77,
        "H" => 0.44,
        _ => return None,
    };
    let pr = match (pr?, changed) {
        ("N", _) => 0.85,
        ("L", false) => 0.62,
        ("L", true) => 0.68,
        ("H", false) => 0.27,
        ("H", true) => 0.5,
        _ => return None,
    };
    let ui = match ui? {
        "N" => 0.85,
        "R" => 0.62,
        _ => return None,
    };
    let cia = |v: Option<&str>| match v? {
        "H" => Some(0.56),
        "L" => Some(0.22),
        "N" => Some(0.0),
        _ => None,
    };
    let iss: f64 = 1.0 - (1.0 - cia(c)?) * (1.0 - cia(i)?) * (1.0 - cia(a)?);
    let impact = if changed {
        7.52 * (iss - 0.029) - 3.25 * (iss - 0.02).powi(15)
    } else {
        6.42 * iss
    };
    if impact <= 0.0 {
        return Some(0.0);
    }
    let exploitability = 8.22 * av * ac * pr * ui;
    let raw: f64 = if changed {
        1.08 * (impact + exploitability)
    } else {
        impact + exploitability
    };
    // Roundup per the specification's integer formulation, immune to float
    // noise such as 4.000000000000001.
    let scaled = (raw.min(10.0) * 100_000.0).round() as i64;
    Some(if scaled % 10_000 == 0 {
        scaled as f64 / 100_000.0
    } else {
        ((scaled / 10_000) + 1) as f64 / 10.0
    })
}

/// Parses `pip-audit --format json` output.
///
/// Two root shapes exist in the wild: current releases (verified against
/// 2.10.1) wrap the rows as `{"dependencies": [{name, version, vulns[]}]}`,
/// while the published examples show a bare array of the same rows. Both are
/// accepted; anything else fails loudly rather than reading as "clean".
///
/// pip-audit publishes no severity, so findings are reported as
/// [`SEVERITY_UNKNOWN`] and counted in their own summary bucket. Every listed
/// requirement is a direct dependency by construction (`--no-deps`).
pub(crate) fn parse_pip_audit_json(text: &str) -> Result<Vec<Vulnerability>, String> {
    let trimmed = text.trim();
    let value: Value = serde_json::from_str(trimmed).map_err(|e| format!("pip-audit JSON: {e}"))?;
    let rows: &[Value] = if let Some(array) = value.as_array() {
        array
    } else if let Some(deps) = value.get("dependencies") {
        deps.as_array()
            .ok_or_else(|| "pip-audit JSON `dependencies` must be an array".to_string())?
    } else {
        // An object with no dependencies key is not a report we understand.
        return Err("pip-audit JSON must be an array or contain `dependencies`".into());
    };
    let mut out = Vec::new();
    for row in rows {
        let name = json_str(row, "name");
        if name.is_empty() {
            continue;
        }
        let version = json_str(row, "version");
        let Some(vulns) = row.get("vulns").and_then(|v| v.as_array()) else {
            continue;
        };
        for vuln in vulns {
            let id = json_str(vuln, "id");
            let aliases = vuln
                .get("aliases")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(|s| s.to_string()))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            // The advisory id (or first alias) is the closest thing pip-audit
            // has to a title; the description is prose and can be very long.
            let title = aliases.first().cloned().unwrap_or_else(|| {
                if id.is_empty() {
                    format!("{name} is vulnerable")
                } else {
                    id.clone()
                }
            });
            let fix_versions = vuln
                .get("fix_versions")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();
            let mut via = aliases;
            if !id.is_empty() && !via.contains(&id) {
                via.push(id);
            }
            out.push(Vulnerability {
                name: name.clone(),
                severity: SEVERITY_UNKNOWN.into(),
                is_direct: true,
                title,
                url: String::new(),
                range: format!("=={version}"),
                fix_available: if fix_versions.is_empty() {
                    "no".into()
                } else {
                    fix_versions
                },
                via,
                ecosystem: "pypi".into(),
            });
        }
    }
    Ok(out)
}

/// Parses `govulncheck -json ./...` output as a stream of JSON messages, each
/// carrying exactly one of `config`, `progress`, `SBOM`, `osv`, or `finding`.
///
/// Physical framing varies by release (verified against v1.7.0): messages are
/// pretty-printed objects simply concatenated — NOT one-object-per-line
/// NDJSON, though compact framing parses identically. The input is therefore
/// read as a whitespace-tolerant sequence of JSON values via
/// [`serde_json::Deserializer::into_iter`]; an unparsable tail segment is
/// skipped, while output containing no message objects at all fails loudly.
///
/// Findings repeat at module → package → symbol granularity; this keeps only
/// the coarsest entry per (OSV id, module) — the "module required" tier — so a
/// vulnerability that exists in the module graph but is never reached is still
/// reported once, without one row per call site. OSV advisory records in the
/// stream are used only for titles/URLs: there are far more of them than
/// actual findings, so keying off `osv` would fabricate results.
///
/// govulncheck publishes no severity, so findings are reported as
/// [`SEVERITY_UNKNOWN`].
pub(crate) fn parse_govulncheck_stream(text: &str) -> Result<Vec<Vulnerability>, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err("govulncheck output was empty".into());
    }
    struct ModuleFinding {
        name: String,
        version: String,
        fixed_version: String,
    }
    let mut osv_titles: BTreeMap<String, String> = BTreeMap::new();
    let mut osv_urls: BTreeMap<String, String> = BTreeMap::new();
    let mut seen: BTreeMap<(String, String), ModuleFinding> = BTreeMap::new();
    let mut parsed_any = false;
    let mut saw_config = false;

    let deserializer = serde_json::Deserializer::from_str(trimmed);
    for item in deserializer.into_iter::<Value>() {
        // A corrupt segment means findings after it were never read; an
        // incomplete stream must not report the findings before it as the
        // whole answer.
        let value = item.map_err(|e| format!("govulncheck stream is truncated or corrupt: {e}"))?;
        if !value.is_object() {
            continue;
        }
        parsed_any = true;
        saw_config |= value.get("config").is_some();
        if let Some(osv) = value.get("osv") {
            let id = json_str(osv, "id");
            if id.is_empty() {
                continue;
            }
            let summary = json_str(osv, "summary");
            let details = json_str(osv, "details");
            osv_titles.insert(
                id.clone(),
                if !summary.is_empty() {
                    summary
                } else if !details.is_empty() {
                    details.chars().take(120).collect()
                } else {
                    id.clone()
                },
            );
            if let Some(url) = osv
                .get("references")
                .and_then(|r| r.as_array())
                .and_then(|refs| refs.iter().find_map(|r| r.get("url")))
                .and_then(|u| u.as_str())
            {
                osv_urls.insert(id, url.to_string());
            }
            continue;
        }
        let Some(finding) = value.get("finding") else {
            // config / progress / SBOM / unknown future message kinds.
            continue;
        };
        let id = json_str(finding, "osv");
        let trace = finding.get("trace").and_then(|t| t.as_array());
        // Module-level findings have a single frame with no package/function;
        // symbol-level frames carry a function. Keep only the coarsest tier.
        let top = trace.and_then(|t| t.first());
        let is_coarse = match top {
            None => true,
            Some(frame) => frame.get("function").is_none() && frame.get("package").is_none(),
        };
        if !is_coarse || id.is_empty() {
            continue;
        }
        let module = top.map(|f| json_str(f, "module")).unwrap_or_default();
        let key = (id.clone(), module.clone());
        let entry = seen.entry(key).or_insert_with(|| ModuleFinding {
            version: top.map(|f| json_str(f, "version")).unwrap_or_default(),
            fixed_version: json_str(finding, "fixed_version"),
            name: if module.is_empty() {
                id.clone()
            } else {
                module
            },
        }); // A later duplicate may know the fixed version even when the first
            // did not (multi-range OSV reports); keep whichever is more useful.
        let fixed = json_str(finding, "fixed_version");
        if entry.fixed_version.is_empty() && !fixed.is_empty() {
            entry.fixed_version = fixed;
        }
    }

    if !parsed_any {
        return Err("govulncheck output contained no JSON messages".into());
    }
    if !saw_config {
        // govulncheck -json opens every stream with a config message; without
        // it this is not a complete run's output.
        return Err("govulncheck stream has no config message".into());
    }
    let mut out = Vec::new();
    for ((id, _module), mf) in seen {
        let title = osv_titles.get(&id).cloned().unwrap_or_else(|| id.clone());
        let url = osv_urls.get(&id).cloned().unwrap_or_else(|| {
            if id.starts_with("GO-") {
                format!("https://pkg.go.dev/vuln/{id}")
            } else {
                String::new()
            }
        });
        out.push(Vulnerability {
            name: mf.name,
            severity: SEVERITY_UNKNOWN.into(),
            is_direct: false,
            title,
            url,
            range: mf.version,
            fix_available: if mf.fixed_version.is_empty() {
                "no".into()
            } else {
                mf.fixed_version
            },
            via: vec![id],
            ecosystem: "go".into(),
        });
    }
    Ok(out)
}

/// Parses `composer audit --format=json --locked`: advisories keyed by package
/// name, each an array of advisory objects (`advisoryId`, `title`, `cve`,
/// `affectedVersions`, `link`, optional `severity`).
///
/// Packagist omits `severity` on some advisories; those default to moderate
/// rather than being dropped or downgraded to informational.
pub(crate) fn parse_composer_audit_json(text: &str) -> Result<Vec<Vulnerability>, String> {
    let trimmed = text.trim();
    let value: Value =
        serde_json::from_str(trimmed).map_err(|e| format!("composer audit JSON: {e}"))?;
    if let Some(msg) = composer_error_message(&value) {
        return Err(msg);
    }
    // Verified against Composer 2.10.x captured output: findings come keyed by
    // package name; a clean lockfile reports `"advisories": []` — an empty
    // ARRAY, not an object — which must read as zero findings.
    let advisories = value
        .get("advisories")
        .ok_or_else(|| "composer audit JSON must contain an advisories object".to_string())?;
    let Some(map) = advisories.as_object() else {
        if advisories.as_array().is_some_and(|a| a.is_empty()) {
            return Ok(Vec::new());
        }
        return Err("composer audit JSON `advisories` must be an object".into());
    };
    let mut out = Vec::new();
    for (package, entries) in map {
        let Some(entries) = entries.as_array() else {
            continue;
        };
        for entry in entries {
            if !entry.is_object() {
                continue;
            }
            let advisory_id = json_str(entry, "advisoryId");
            let cve = first_non_empty(entry, &["cve", "CVE"]).unwrap_or_default();
            let title = first_non_empty(entry, &["title", "description"])
                .or_else(|| (!advisory_id.is_empty()).then(|| advisory_id.clone()))
                .or_else(|| (!cve.is_empty()).then(|| cve.clone()))
                .unwrap_or_else(|| format!("{package} is vulnerable"));
            let url = first_non_empty(entry, &["link", "reference"]).unwrap_or_else(|| {
                if cve.is_empty() {
                    String::new()
                } else {
                    format!("https://www.cve.org/CVERecord?id={cve}")
                }
            });
            // Packagist omits severity on some advisories; default to moderate
            // rather than dropping the finding or calling it informational.
            let raw_severity = json_str(entry, "severity").to_ascii_lowercase();
            let severity = if raw_severity.is_empty() {
                "moderate".to_string()
            } else {
                raw_severity
            };
            let affected = first_non_empty(entry, &["affectedVersions", "affected versions"])
                .unwrap_or_default();
            let name = opt_json_str(entry, "packageName")
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| package.clone());
            let mut via = Vec::new();
            if !advisory_id.is_empty() {
                via.push(advisory_id);
            }
            out.push(Vulnerability {
                name,
                severity,
                is_direct: false,
                title,
                url,
                range: affected,
                fix_available: "see advisory".into(),
                via,
                ecosystem: "composer".into(),
            });
        }
    }
    Ok(out)
}

fn composer_error_message(value: &Value) -> Option<String> {
    let err = value.get("error")?;
    let mut parts = Vec::new();
    if let Some(code) = err.as_str() {
        return Some(code.to_string());
    }
    for key in ["code", "summary", "detail", "message"] {
        if let Some(part) = opt_json_str(err, key).filter(|s| !s.is_empty()) {
            parts.push(part);
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" — "))
    }
}

/// Conservative engines.node check: flag only when the running Node version
/// satisfies NONE of the spec's `||` alternatives. Each alternative is an AND
/// of its comparators, compared on full `major.minor.patch` (so `>18.5.0`
/// admits 18.20.0 and `<20.1` bounds the minor); unparsable fragments are
/// skipped rather than allowed to discard their neighbours, and unparseable
/// specs produce no warning.
pub(crate) fn engines_mismatch(spec: &str, node_version: &str) -> Option<String> {
    let current = parse_node_version(node_version)?;
    let spec = spec.trim();
    if spec.is_empty() || spec == "*" {
        return None;
    }
    let mut parsed_any = false;
    let mut satisfied_any = false;
    for clause in spec.split("||") {
        let Some(bounds) = parse_engine_bounds(clause) else {
            continue;
        };
        parsed_any = true;
        let above_min = bounds.min.is_none_or(|min| current >= min);
        let below_max = bounds.max_exclusive.is_none_or(|max| current < max);
        if above_min && below_max {
            satisfied_any = true;
        }
    }
    if parsed_any && !satisfied_any {
        return Some(format!(
            "package.json engines.node is `{spec}` but this machine is Node {node_version}"
        ));
    }
    None
}

type Version = [u64; 3];

/// Inclusive lower and exclusive upper bound of one `||`-free clause.
#[derive(Default)]
struct EngineBounds {
    min: Option<Version>,
    max_exclusive: Option<Version>,
}

impl EngineBounds {
    fn raise_min(&mut self, v: Version) {
        self.min = Some(self.min.map_or(v, |m| m.max(v)));
    }
    fn lower_max(&mut self, v: Version) {
        self.max_exclusive = Some(self.max_exclusive.map_or(v, |m| m.min(v)));
    }
}

fn parse_node_version(version: &str) -> Option<Version> {
    let (parts, _) = parse_partial(version)?;
    Some([parts[0]?, parts[1].unwrap_or(0), parts[2].unwrap_or(0)])
}

/// `18`, `18.5`, `v18.5.0`, `18.x`, `18.5.0-rc.1` → the numeric parts given
/// (wildcards and missing parts are `None`) and how many were given.
fn parse_partial(raw: &str) -> Option<([Option<u64>; 3], usize)> {
    let raw = raw.trim().trim_start_matches('v').trim_start_matches('=');
    // Prerelease and build tags do not move the numeric bounds.
    let raw = raw.split(['-', '+']).next().unwrap_or("");
    let mut parts = [None; 3];
    let mut given = 0;
    for (i, piece) in raw.split('.').enumerate() {
        if i >= 3 {
            return None;
        }
        match piece {
            "x" | "X" | "*" | "" => break,
            digits => {
                parts[i] = Some(digits.parse().ok()?);
                given = i + 1;
            }
        }
    }
    parts[0]?;
    Some((parts, given))
}

fn padded(parts: [Option<u64>; 3]) -> Version {
    [
        parts[0].unwrap_or(0),
        parts[1].unwrap_or(0),
        parts[2].unwrap_or(0),
    ]
}

/// The first version above everything the partial `parts` (of `given` numbers)
/// stands for: `18` → 19.0.0, `18.5` → 18.6.0, `18.5.2` → 18.5.3.
fn next_after(parts: [Option<u64>; 3], given: usize) -> Version {
    let mut v = padded(parts);
    let idx = given.clamp(1, 3) - 1;
    v[idx] += 1;
    for slot in v.iter_mut().skip(idx + 1) {
        *slot = 0;
    }
    v
}

/// Parses ONE `||`-free clause. Comparators AND together; an operator may be
/// separated from its operand by whitespace (`< 20`); a hyphen range
/// (`16.4.0 - 18.2.0`, spaced or glued) is `>=A <=B`; unknown fragments are
/// skipped instead of discarding the bounds already parsed.
fn parse_engine_bounds(clause: &str) -> Option<EngineBounds> {
    let clause = clause.trim();
    if clause.is_empty() || clause == "*" {
        return None;
    }
    // Uniform tokens: split glued hyphen ranges, then glue each bare operator
    // to the operand that follows it.
    let mut raw_tokens: Vec<String> = Vec::new();
    for raw in clause.split_whitespace() {
        match find_hyphen_range_split(raw) {
            Some(idx) => {
                raw_tokens.push(raw[..idx].to_string());
                raw_tokens.push("-".to_string());
                raw_tokens.push(raw[idx + 1..].to_string());
            }
            None => raw_tokens.push(raw.to_string()),
        }
    }
    let mut tokens: Vec<String> = Vec::new();
    let mut pending_op = String::new();
    for token in raw_tokens {
        let token = token.trim_matches(',').to_string();
        if !token.is_empty()
            && token
                .chars()
                .all(|c| matches!(c, '<' | '>' | '=' | '^' | '~'))
        {
            pending_op.push_str(&token);
            continue;
        }
        tokens.push(format!("{pending_op}{token}"));
        pending_op.clear();
    }

    let mut bounds = EngineBounds::default();
    let mut expect_upper = false;
    for (index, token) in tokens.iter().enumerate() {
        let token = token.as_str();
        if token.is_empty() || token == "||" {
            continue;
        }
        if token == "-" {
            expect_upper = true;
            continue;
        }
        if expect_upper {
            // Upper bound of a hyphen range: inclusive of what it names.
            if let Some((parts, given)) = parse_partial(token) {
                bounds.lower_max(next_after(parts, given));
            }
            expect_upper = false;
            continue;
        }
        if let Some(rest) = token.strip_prefix(">=") {
            if let Some((parts, _)) = parse_partial(rest) {
                bounds.raise_min(padded(parts));
            }
        } else if let Some(rest) = token.strip_prefix('>') {
            if let Some((parts, given)) = parse_partial(rest) {
                bounds.raise_min(next_after(parts, given));
            }
        } else if let Some(rest) = token.strip_prefix("<=") {
            if let Some((parts, given)) = parse_partial(rest) {
                bounds.lower_max(next_after(parts, given));
            }
        } else if let Some(rest) = token.strip_prefix('<') {
            if let Some((parts, _)) = parse_partial(rest) {
                bounds.lower_max(padded(parts));
            }
        } else if let Some(rest) = token.strip_prefix('^') {
            if let Some((parts, given)) = parse_partial(rest) {
                let v = padded(parts);
                bounds.raise_min(v);
                let at = if v[0] > 0 || given <= 1 {
                    0
                } else if v[1] > 0 || given <= 2 {
                    1
                } else {
                    2
                };
                bounds.lower_max(next_at(v, at));
            }
        } else if let Some(rest) = token.strip_prefix('~') {
            if let Some((parts, given)) = parse_partial(rest) {
                bounds.raise_min(padded(parts));
                bounds.lower_max(next_at(padded(parts), if given >= 2 { 1 } else { 0 }));
            }
        } else if token
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_digit() || c == 'v')
        {
            // A bare version: `18`/`18.x` is the whole major, `18.5.2` itself.
            if let Some((parts, given)) = parse_partial(token) {
                bounds.raise_min(padded(parts));
                // The lower end of `A - B` is only a minimum.
                let starts_range = tokens.get(index + 1).is_some_and(|t| t == "-");
                if given < 3 && !starts_range {
                    bounds.lower_max(next_after(parts, given));
                }
            }
        }
    }
    if bounds.min.is_none() && bounds.max_exclusive.is_none() {
        None
    } else {
        Some(bounds)
    }
}

/// Bumps `v[at]` and zeroes what follows it.
fn next_at(mut v: Version, at: usize) -> Version {
    v[at] += 1;
    for slot in v.iter_mut().skip(at + 1) {
        *slot = 0;
    }
    v
}

/// Finds the split index of a hyphen-range operator inside a single token:
/// a `-` preceded by a digit and followed by a digit or `v`-digit.
fn find_hyphen_range_split(token: &str) -> Option<usize> {
    if token.starts_with('>')
        || token.starts_with('<')
        || token.starts_with('^')
        || token.starts_with('~')
        || token.starts_with('=')
    {
        return None;
    }
    let bytes = token.as_bytes();
    for i in 1..bytes.len().saturating_sub(1) {
        if bytes[i] != b'-' {
            continue;
        }
        let prefix = &token[..i];
        let suffix = &token[i + 1..];
        // If prefix has >=2 dots (e.g. 18.0.0), it is a prerelease tag (e.g. 18.0.0-1)
        // unless the suffix also looks like a full version with dots or 'v' (e.g. 16.4.0-18.2.0)
        if prefix.matches('.').count() >= 2 && !suffix.contains('.') && !suffix.starts_with('v') {
            continue;
        }
        let prev_is_digit = bytes[i - 1].is_ascii_digit();
        let next = bytes[i + 1];
        let next_starts_version = next.is_ascii_digit() || (next == b'v');
        if prev_is_digit && next_starts_version {
            return Some(i);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::procguard::LockedSpawn;
    // `Command` is used by every case; `fs` and `TempDir` only by the
    // `#[cfg(unix)]` ones, so ungated they are unused imports on Windows.
    #[cfg(unix)]
    use std::fs;
    use std::process::Command;
    #[cfg(unix)]
    use tempfile::TempDir;

    use crate::test_support::{git_repo, write};

    fn git_add(dir: &Path, rel: &str) {
        let status = Command::new("git")
            .args(["add", rel])
            .current_dir(dir)
            .status_locked()
            .expect("git add");
        assert!(status.success());
    }

    #[test]
    fn parse_audit_v2_extracts_direct_high_and_via_title() {
        let json = r#"{
          "auditReportVersion": 2,
          "vulnerabilities": {
            "lodash": {
              "name": "lodash",
              "severity": "high",
              "isDirect": true,
              "via": [
                {
                  "source": 1,
                  "name": "lodash",
                  "title": "Prototype Pollution in lodash",
                  "url": "https://github.com/advisories/GHSA-xxxx",
                  "severity": "high",
                  "range": "<4.17.21"
                }
              ],
              "effects": [],
              "range": "<4.17.21",
              "nodes": ["node_modules/lodash"],
              "fixAvailable": true
            },
            "leftpad": {
              "name": "leftpad",
              "severity": "moderate",
              "isDirect": false,
              "via": ["lodash"],
              "range": "*",
              "fixAvailable": {
                "name": "leftpad",
                "version": "2.0.0",
                "isSemVerMajor": true
              }
            }
          }
        }"#;
        let (vulns, err) = parse_npm_audit_json(json).expect("parse");
        assert!(err.is_none());
        assert_eq!(vulns.len(), 2);
        let lodash = vulns.iter().find(|v| v.name == "lodash").unwrap();
        assert_eq!(lodash.severity, "high");
        assert!(lodash.is_direct);
        assert_eq!(lodash.title, "Prototype Pollution in lodash");
        assert_eq!(lodash.url, "https://github.com/advisories/GHSA-xxxx");
        assert_eq!(lodash.fix_available, "yes");
        let left = vulns.iter().find(|v| v.name == "leftpad").unwrap();
        assert_eq!(left.fix_available, "leftpad@2.0.0 (breaking)");
        assert_eq!(left.via, vec!["lodash"]);
    }

    #[test]
    fn parse_audit_v1_and_enolock_error() {
        let v1 = r#"{
          "advisories": {
            "577": {
              "module_name": "minimist",
              "severity": "low",
              "title": "Prototype Pollution",
              "url": "https://npmjs.com/advisories/577",
              "vulnerable_versions": "<1.2.3",
              "patched_versions": ">=1.2.3",
              "findings": [{"paths": ["app>minimist"]}]
            }
          }
        }"#;
        let (vulns, err) = parse_npm_audit_json(v1).unwrap();
        assert!(err.is_none());
        assert_eq!(vulns[0].name, "minimist");
        assert_eq!(vulns[0].fix_available, ">=1.2.3");
        assert!(!vulns[0].is_direct);

        let enolock = r#"{
          "error": {
            "code": "ENOLOCK",
            "summary": "This command requires an existing lockfile.",
            "detail": "Try creating one first with: npm i --package-lock-only"
          }
        }"#;
        let (vulns, err) = parse_npm_audit_json(enolock).unwrap();
        assert!(vulns.is_empty());
        let err = err.expect("enolock");
        assert!(err.contains("ENOLOCK"));
        assert!(err.contains("lockfile"));
    }

    /// Captured from `bun audit --json` on Bun 1.4.2. A clean audit is `{}`,
    /// not an npm-shaped report, and findings are advisory arrays keyed by
    /// package name.
    #[test]
    fn parse_bun_audit_reads_advisories_and_refuses_to_call_a_foreign_shape_clean() {
        assert!(parse_bun_audit_json("{}").unwrap().is_empty());
        let json = r#"{
          "lodash": [{
            "id": 1106913,
            "url": "https://github.com/advisories/GHSA-35jh-r3h4-6jhm",
            "title": "Command Injection in lodash",
            "severity": "high",
            "vulnerable_versions": "<4.17.21"
          }]
        }"#;
        let vulns = parse_bun_audit_json(json).unwrap();
        assert_eq!(vulns.len(), 1);
        assert_eq!(vulns[0].name, "lodash");
        assert_eq!(vulns[0].severity, "high");
        assert_eq!(vulns[0].title, "Command Injection in lodash");
        assert_eq!(vulns[0].range, "<4.17.21");
        assert_eq!(vulns[0].via, vec!["1106913".to_string()]);
        assert_eq!(vulns[0].ecosystem, "npm");
        assert!(!vulns[0].is_direct, "the parser does not guess directness");

        let foreign = parse_bun_audit_json(r#"{"error":{"code":"ENOLOCK"}}"#)
            .expect_err("an error envelope is not a clean audit");
        assert!(foreign.contains("not an advisory list"), "{foreign}");
        assert!(parse_bun_audit_json("").is_err());
        assert!(parse_bun_audit_json("[]").is_err());
    }

    #[test]
    fn parse_outdated_and_empty_object() {
        let json = r#"{
          "typescript": {
            "current": "5.6.0",
            "wanted": "5.9.2",
            "latest": "5.9.2",
            "dependent": "gitpulse",
            "location": "/tmp/node_modules/typescript",
            "type": "devDependencies"
          },
          "lucide-svelte": {
            "current": "0.475.0",
            "wanted": "0.500.0",
            "latest": "0.540.0",
            "type": "dependencies"
          }
        }"#;
        let rows = parse_npm_outdated_json(json).unwrap();
        assert_eq!(rows.len(), 2);
        let ts = rows.iter().find(|r| r.name == "typescript").unwrap();
        assert_eq!(ts.current, "5.6.0");
        assert_eq!(ts.wanted, "5.9.2");
        assert_eq!(ts.latest, "5.9.2");
        assert_eq!(ts.dep_type, "devDependencies");
        assert!(parse_npm_outdated_json("{}").unwrap().is_empty());
        assert!(parse_npm_outdated_json("").unwrap().is_empty());
    }

    /// Captured verbatim from `npm outdated --json` on npm 11.17.0. The
    /// hand-written fixture above still carries a `type` field, which npm 7
    /// removed in 2020 — so that test kept passing while the real format had
    /// no such key, and `dep_type` came out empty on every install anyone
    /// actually has. The Health panel's Type column has rendered "—" ever
    /// since, and the markdown report has said "dep".
    ///
    /// A fixture from the tool beats a fixture from memory: this one would
    /// have failed the day npm dropped the field.
    const NPM11_OUTDATED: &str = r#"{
      "@sveltejs/vite-plugin-svelte": {
        "current": "5.1.1",
        "wanted": "5.1.1",
        "latest": "7.3.0",
        "dependent": "GitPulse",
        "location": "/repo/node_modules/@sveltejs/vite-plugin-svelte"
      },
      "some-transitive-thing": {
        "current": "1.0.0",
        "wanted": "1.0.1",
        "latest": "2.0.0",
        "dependent": "GitPulse",
        "location": "/repo/node_modules/some-transitive-thing"
      }
    }"#;

    /// Captured verbatim from `npm audit --json` on npm 11.17.0 with a clean
    /// tree. `vulnerabilities` is an empty object, not an absent key and not
    /// an empty array, and the report carries `auditReportVersion: 2`.
    const NPM11_AUDIT_CLEAN: &str = r#"{
      "auditReportVersion": 2,
      "vulnerabilities": {},
      "metadata": {
        "vulnerabilities": { "info": 0, "low": 0, "moderate": 0, "high": 0, "critical": 0, "total": 0 },
        "dependencies": { "prod": 26, "dev": 261, "optional": 65, "peer": 0, "peerOptional": 0, "total": 286 }
      }
    }"#;

    /// `via` is npm's most shape-shifting field: an array mixing plain
    /// package-name strings (the chain a vulnerability arrives through) with
    /// advisory objects carrying title and url. Both forms appear in one
    /// array, order is not guaranteed, and a package can repeat.
    #[test]
    fn via_handles_both_shapes_in_one_array() {
        let via = serde_json::json!([
            "lodash",
            { "title": "Prototype Pollution", "url": "https://example.invalid/1", "name": "minimist" },
            "lodash",
            { "title": "A later advisory", "url": "https://example.invalid/2", "name": "minimist" }
        ]);
        let (title, url, names) = via_details(Some(&via));
        // First advisory wins for the human-facing fields, so the headline
        // does not change depending on array order downstream.
        assert_eq!(title, "Prototype Pollution");
        assert_eq!(url, "https://example.invalid/1");
        // Names are deduplicated across both shapes.
        assert_eq!(names, vec!["lodash", "minimist"]);
    }

    #[test]
    fn via_survives_shapes_it_was_not_built_for() {
        // Absent, not an array, and an array of neither strings nor objects:
        // npm has changed this field before, and none of these may panic.
        assert_eq!(
            via_details(None),
            (String::new(), String::new(), Vec::new())
        );
        let scalar = serde_json::json!("lodash");
        assert_eq!(
            via_details(Some(&scalar)),
            (String::new(), String::new(), Vec::new())
        );
        let odd = serde_json::json!([1, true, null, { "no_title": "x" }]);
        let (title, url, names) = via_details(Some(&odd));
        assert!(title.is_empty() && url.is_empty() && names.is_empty());
    }

    /// `fixAvailable` is `true`, `false`, or an object describing the upgrade
    /// — and the object form is the one that tells a user whether the fix is
    /// breaking. Reporting a major-version bump as a plain "yes" would send
    /// someone into an unexpected breaking upgrade.
    #[test]
    fn fix_available_distinguishes_a_breaking_upgrade() {
        use serde_json::json;
        assert_eq!(format_fix_available(Some(&json!(true))), "yes");
        assert_eq!(format_fix_available(Some(&json!(false))), "no");
        assert_eq!(format_fix_available(None), "no");
        assert_eq!(
            format_fix_available(Some(
                &json!({ "name": "vite", "version": "5.0.0", "isSemVerMajor": false })
            )),
            "vite@5.0.0"
        );
        assert_eq!(
            format_fix_available(Some(
                &json!({ "name": "vite", "version": "7.0.0", "isSemVerMajor": true })
            )),
            "vite@7.0.0 (breaking)",
            "a major bump must say so"
        );
        // An object with no name says nothing useful about which package to
        // upgrade, so it degrades to the bare affirmative rather than "@".
        assert_eq!(
            format_fix_available(Some(&json!({ "version": "1.0.0" }))),
            "yes"
        );
        // A shape from neither format is named unknown rather than guessed at
        // as either yes or no.
        assert_eq!(format_fix_available(Some(&json!("maybe"))), "unknown");
    }

    #[test]
    fn outdated_rejects_shapes_it_cannot_read_and_accepts_emptiness() {
        // npm prints nothing at all when everything is current.
        assert_eq!(parse_npm_outdated_json("").unwrap().len(), 0);
        assert_eq!(parse_npm_outdated_json("   \n ").unwrap().len(), 0);
        // A JSON array is not the keyed object this format promises.
        assert!(parse_npm_outdated_json("[]").is_err());
        // An npm error payload surfaces as an error, not as "nothing outdated".
        let err = parse_npm_outdated_json(
            r#"{"error": {"code": "ENOLOCK", "summary": "no lockfile", "detail": "run npm install"}}"#,
        )
        .expect_err("an npm error is not an empty result");
        assert!(err.contains("ENOLOCK"), "{err}");
        // Non-object entries are skipped rather than aborting the whole list.
        let rows = parse_npm_outdated_json(
            r#"{ "good": { "current": "1.0.0", "latest": "2.0.0" }, "junk": "not an object" }"#,
        )
        .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "good");
    }

    #[test]
    fn a_clean_npm11_audit_reads_as_clean() {
        let (vulns, err) = parse_npm_audit_json(NPM11_AUDIT_CLEAN).expect("parses");
        assert!(vulns.is_empty());
        assert_eq!(err, None, "a clean audit is not an error");
    }

    /// The invariant a security check lives or dies on: a report this parser
    /// cannot read must never produce the same answer as a report it read and
    /// found clean. Both yield zero rows; only one of them is a fact.
    #[test]
    fn an_unreadable_audit_report_is_never_mistaken_for_a_clean_one() {
        // A shape from neither format — e.g. a future npm emitting a list.
        let err = parse_npm_audit_json(r#"{"vulnerabilities": [], "auditReportVersion": 3}"#)
            .expect_err("an unknown shape must fail loudly");
        assert!(err.contains("unrecognised"), "{err}");

        // And the subtler one: the right shape, but nothing could be read out
        // of it while npm itself says there were findings.
        let (vulns, note) = parse_npm_audit_json(
            r#"{
              "auditReportVersion": 2,
              "vulnerabilities": {},
              "metadata": { "vulnerabilities": { "total": 7 } }
            }"#,
        )
        .expect("parses");
        assert!(vulns.is_empty());
        let note = note.expect("a count mismatch must be reported, not swallowed");
        assert!(note.contains("7"), "{note}");
        assert!(note.contains("unscanned"), "{note}");
    }

    #[test]
    fn npm11_outdated_carries_no_type_field() {
        // Pin the premise. If a future npm restores `type`, this fails and the
        // manifest fallback below can be reconsidered rather than left to rot.
        let rows = parse_npm_outdated_json(NPM11_OUTDATED).unwrap();
        assert_eq!(rows.len(), 2);
        for row in &rows {
            assert!(
                row.dep_type.is_empty(),
                "npm 11 does not report a dependency type; got {:?}",
                row.dep_type
            );
            assert!(!row.latest.is_empty(), "the other fields still parse");
        }
    }

    #[test]
    fn dependency_types_are_recovered_from_the_manifest() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        std::fs::write(
            dir.path().join("package.json"),
            r#"{
              "dependencies": { "runtime-thing": "^1.0.0" },
              "devDependencies": { "@sveltejs/vite-plugin-svelte": "^5.0.0" },
              "optionalDependencies": { "maybe-thing": "^1.0.0" },
              "peerDependencies": { "peer-thing": "^1.0.0" }
            }"#,
        )
        .expect("write manifest");
        let sections = declared_dependency_sections(dir.path(), &dir.path().join("package.json"))
            .expect("sections");
        assert_eq!(
            sections
                .get("@sveltejs/vite-plugin-svelte")
                .map(String::as_str),
            Some("devDependencies")
        );
        assert_eq!(
            sections.get("runtime-thing").map(String::as_str),
            Some("dependencies")
        );
        assert_eq!(
            sections.get("maybe-thing").map(String::as_str),
            Some("optionalDependencies")
        );
        assert_eq!(
            sections.get("peer-thing").map(String::as_str),
            Some("peerDependencies")
        );

        let mut rows = parse_npm_outdated_json(NPM11_OUTDATED).unwrap();
        fill_dependency_types(&mut rows, &sections);
        let plugin = rows
            .iter()
            .find(|r| r.name == "@sveltejs/vite-plugin-svelte")
            .expect("row");
        assert_eq!(
            plugin.dep_type, "devDependencies",
            "the Type column should say what the manifest says"
        );
        // A package no section declares stays blank rather than being guessed.
        let unknown = rows
            .iter()
            .find(|r| r.name == "some-transitive-thing")
            .unwrap();
        assert_eq!(unknown.dep_type, "");
    }

    #[test]
    fn npm6_type_field_wins_over_the_manifest() {
        // Where npm still labels the row itself, its answer is authoritative:
        // the fallback fills gaps, it does not override.
        let mut rows = parse_npm_outdated_json(
            r#"{ "typescript": { "current": "1.0.0", "wanted": "1.0.0", "latest": "2.0.0", "type": "devDependencies" } }"#,
        )
        .unwrap();
        let mut sections = BTreeMap::new();
        sections.insert("typescript".to_string(), "dependencies".to_string());
        fill_dependency_types(&mut rows, &sections);
        assert_eq!(rows[0].dep_type, "devDependencies");
    }

    #[test]
    fn an_unreadable_manifest_leaves_types_blank_rather_than_wrong() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        // No manifest at all, then one that is not JSON.
        assert!(
            declared_dependency_sections(dir.path(), &dir.path().join("package.json"))
                .expect("sections")
                .is_empty()
        );
        std::fs::write(dir.path().join("package.json"), "{ not json").expect("write");
        assert!(
            declared_dependency_sections(dir.path(), &dir.path().join("package.json"))
                .expect("sections")
                .is_empty()
        );
    }

    /// A canonical in-repo fixture root: the manifest reads compare canonical
    /// targets against the repo path, as `local_scan` supplies it.
    #[cfg(unix)]
    fn canonical_repo() -> (TempDir, PathBuf) {
        let dir = TempDir::new().expect("tempdir");
        let root = dir.path().canonicalize().expect("canonical root");
        (dir, root)
    }

    /// E-integrations-04: the size gate used to measure the symlink itself
    /// (a few bytes) and then read the whole target.
    #[cfg(unix)]
    #[test]
    fn a_package_json_symlinked_to_a_large_in_repo_file_is_refused() {
        let (_guard, repo) = canonical_repo();
        fs::write(repo.join("big.json"), vec![b' '; 5 * 1024 * 1024]).expect("write big");
        std::os::unix::fs::symlink("big.json", repo.join("package.json")).expect("symlink");
        let err = read_npm_manifest(&repo, "package.json").expect_err("oversized target");
        assert!(err.contains("exceeds"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn a_package_json_symlinked_outside_the_repo_is_refused_by_declares_workspaces() {
        let (_guard, repo) = canonical_repo();
        let (_outside_guard, outside) = canonical_repo();
        let workspaces = r#"{ "workspaces": ["packages/*"] }"#;
        fs::write(outside.join("package.json"), workspaces).expect("write outside");
        std::os::unix::fs::symlink(outside.join("package.json"), repo.join("package.json"))
            .expect("symlink");
        assert!(
            !declares_workspaces(&repo, ""),
            "a link out of the repository must not be opened"
        );
        // Control: the same bytes in the repository are honoured.
        fs::remove_file(repo.join("package.json")).expect("unlink");
        fs::write(repo.join("package.json"), workspaces).expect("write inside");
        assert!(declares_workspaces(&repo, ""));
    }

    /// A FIFO named package.json (reached directly or through an in-repo
    /// link) parked `std::fs::read` forever; it must be refused promptly.
    #[cfg(unix)]
    #[test]
    fn a_fifo_named_package_json_is_refused_without_blocking() {
        let (_guard, repo) = canonical_repo();
        let status = Command::new("mkfifo")
            .arg(repo.join("fifo.json"))
            .status_locked()
            .expect("mkfifo");
        assert!(status.success());
        std::os::unix::fs::symlink("fifo.json", repo.join("package.json")).expect("symlink");
        let (tx, rx) = std::sync::mpsc::channel();
        let worker_repo = repo.clone();
        std::thread::spawn(move || {
            let _ = tx.send((
                read_npm_manifest(&worker_repo, "package.json"),
                declares_workspaces(&worker_repo, ""),
            ));
        });
        let (read, workspaces) = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("manifest reads must not block on a FIFO");
        let err = read.expect_err("a FIFO is not a manifest");
        assert!(err.contains("not a regular file"), "{err}");
        assert!(!workspaces);
    }

    #[cfg(unix)]
    #[test]
    fn a_normal_manifest_still_parses() {
        let (_guard, repo) = canonical_repo();
        fs::write(
            repo.join("package.json"),
            r#"{ "name": "ok", "workspaces": ["packages/*"] }"#,
        )
        .expect("write");
        let manifest = read_npm_manifest(&repo, "package.json")
            .expect("readable")
            .expect("present");
        assert_eq!(manifest.path, "package.json");
        assert!(declares_workspaces(&repo, ""));
        assert!(read_npm_manifest(&repo, "missing/package.json")
            .expect("absent is not an error")
            .is_none());
    }

    /// The declared-section read (after an audit finds vulnerabilities) had no
    /// cap or link check at all; a refused manifest is now an error the
    /// dispatcher turns into an issue.
    #[cfg(unix)]
    #[test]
    fn declared_sections_refuse_an_oversized_or_escaping_manifest() {
        let (_guard, repo) = canonical_repo();
        let manifest = repo.join("package.json");
        fs::write(repo.join("big.json"), vec![b' '; 5 * 1024 * 1024]).expect("write big");
        std::os::unix::fs::symlink("big.json", &manifest).expect("symlink");
        let err = declared_dependency_sections(&repo, &manifest).expect_err("oversized");
        assert!(err.contains("exceeds"), "{err}");

        let (_outside_guard, outside) = canonical_repo();
        fs::write(
            outside.join("package.json"),
            r#"{"dependencies":{"x":"1"}}"#,
        )
        .expect("outside");
        fs::remove_file(&manifest).expect("unlink");
        std::os::unix::fs::symlink(outside.join("package.json"), &manifest).expect("symlink");
        let err = declared_dependency_sections(&repo, &manifest).expect_err("escaping");
        assert!(err.contains("escapes the repository"), "{err}");

        fs::remove_file(&manifest).expect("unlink");
        fs::write(&manifest, r#"{"dependencies":{"x":"1"}}"#).expect("inside");
        let sections = declared_dependency_sections(&repo, &manifest).expect("sections");
        assert_eq!(sections.get("x").map(String::as_str), Some("dependencies"));
    }

    /// Adversarial review: `body` padded with spaces to exactly `len` bytes.
    #[cfg(unix)]
    fn padded_manifest(len: usize) -> Vec<u8> {
        let mut bytes = br#"{"name":"pad"}"#.to_vec();
        bytes.resize(len, b' ');
        bytes
    }

    #[cfg(unix)]
    #[test]
    fn adv_manifest_size_boundary_is_exact() {
        let (_guard, repo) = canonical_repo();
        let cap = MAX_MANIFEST_BYTES as usize;
        fs::write(repo.join("package.json"), padded_manifest(cap)).expect("write");
        assert!(read_npm_manifest(&repo, "package.json")
            .expect("exactly the cap is readable")
            .is_some());
        fs::write(repo.join("package.json"), padded_manifest(cap + 1)).expect("write");
        let err = read_npm_manifest(&repo, "package.json").expect_err("cap + 1");
        assert!(err.contains("exceeds"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn adv_symlink_chain_inside_the_repo_resolves() {
        let (_guard, repo) = canonical_repo();
        fs::write(repo.join("real.json"), br#"{"name":"chain"}"#).expect("write");
        std::os::unix::fs::symlink("real.json", repo.join("b.json")).expect("b");
        std::os::unix::fs::symlink("b.json", repo.join("a.json")).expect("a");
        std::os::unix::fs::symlink("a.json", repo.join("package.json")).expect("p");
        let manifest = read_npm_manifest(&repo, "package.json")
            .expect("chain is readable")
            .expect("present");
        assert_eq!(manifest.name, "chain");
    }

    #[cfg(unix)]
    #[test]
    fn adv_symlinked_parent_directory_escape_is_refused_everywhere() {
        let (_guard, repo) = canonical_repo();
        let (_outside_guard, outside) = canonical_repo();
        fs::write(
            outside.join("package.json"),
            br#"{"name":"out","workspaces":["x"],"dependencies":{"d":"1"}}"#,
        )
        .expect("write");
        std::os::unix::fs::symlink(&outside, repo.join("sub")).expect("symlink dir");
        let err = read_npm_manifest(&repo, "sub/package.json").expect_err("escape");
        assert!(err.contains("escape"), "{err}");
        assert!(!declares_workspaces(&repo, "sub"));
        let err = declared_dependency_sections(&repo, &repo.join("sub/package.json"))
            .expect_err("escape");
        assert!(err.contains("escape"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn adv_socket_named_package_json_is_refused() {
        let (_guard, repo) = canonical_repo();
        let _listener =
            std::os::unix::net::UnixListener::bind(repo.join("package.json")).expect("bind");
        read_npm_manifest(&repo, "package.json").expect_err("a socket is refused");
        assert!(!declares_workspaces(&repo, ""));
    }

    /// Break (wording): open(2) on a socket fails with EOPNOTSUPP before the
    /// descriptor can be classified, so the issue says "Operation not
    /// supported on socket" instead of "not a regular file".
    #[cfg(unix)]
    #[test]
    fn adv_socket_refusal_names_the_file_kind() {
        let (_guard, repo) = canonical_repo();
        let _listener =
            std::os::unix::net::UnixListener::bind(repo.join("package.json")).expect("bind");
        let err = read_npm_manifest(&repo, "package.json").expect_err("socket");
        assert!(err.contains("not a regular file"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn adv_dangling_and_looping_links_are_errors_not_empty() {
        let (_guard, repo) = canonical_repo();
        std::os::unix::fs::symlink("nowhere.json", repo.join("package.json")).expect("dangling");
        assert!(read_npm_manifest(&repo, "package.json").is_err());
        fs::remove_file(repo.join("package.json")).expect("unlink");
        std::os::unix::fs::symlink("package.json", repo.join("package.json")).expect("loop");
        assert!(read_npm_manifest(&repo, "package.json").is_err());
    }

    /// A file appended to while it is read never yields more than the cap.
    #[cfg(unix)]
    #[test]
    fn adv_a_growing_manifest_never_returns_more_than_the_cap() {
        use std::io::Write;
        let (_guard, repo) = canonical_repo();
        let cap = MAX_MANIFEST_BYTES as usize;
        let path = repo.join("package.json");
        fs::write(&path, padded_manifest(cap - 64)).expect("write");
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = stop.clone();
        let grow_path = path.clone();
        let grower = std::thread::spawn(move || {
            while !flag.load(std::sync::atomic::Ordering::Relaxed) {
                if let Ok(mut f) = fs::OpenOptions::new().append(true).open(&grow_path) {
                    let _ = f.write_all(&[b' '; 4096]);
                }
            }
        });
        for _ in 0..200 {
            match read_manifest_bytes(&repo, "package.json") {
                Ok(Some(bytes)) => assert!(bytes.len() <= cap, "{} bytes", bytes.len()),
                Ok(None) => panic!("file exists"),
                Err(message) => assert!(message.contains("exceeds"), "{message}"),
            }
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        grower.join().expect("grower");
    }

    /// Break: a hardlink is a regular file with a second name, so it is read
    /// even when its other name is outside the repository.
    #[cfg(unix)]
    #[test]
    #[ignore = "accepted limitation: a hardlink is indistinguishable from a regular file by path; see ft-bdb253cf review"]
    fn adv_hardlink_to_an_outside_file_is_refused() {
        let (_guard, repo) = canonical_repo();
        let (_outside_guard, outside) = canonical_repo();
        fs::write(outside.join("secret.json"), br#"{"name":"secret"}"#).expect("write");
        fs::hard_link(outside.join("secret.json"), repo.join("package.json")).expect("hardlink");
        assert!(read_npm_manifest(&repo, "package.json").is_err());
    }

    /// Non-UTF-8 bodies: `read_npm_manifest` decodes lossily (as before);
    /// the other two readers treat them as unparsable, never as an error.
    #[cfg(unix)]
    #[test]
    fn adv_non_utf8_body_degrades_without_error() {
        let (_guard, repo) = canonical_repo();
        fs::write(
            repo.join("package.json"),
            b"{\"name\":\"caf\xe9\",\"workspaces\":[\"a\"]}",
        )
        .expect("write");
        assert!(read_npm_manifest(&repo, "package.json")
            .expect("lossy read")
            .is_some());
        assert!(!declares_workspaces(&repo, ""));
        assert!(
            declared_dependency_sections(&repo, &repo.join("package.json"))
                .expect("not an error")
                .is_empty()
        );
    }

    /// APFS is case- and normalization-insensitive by default; the read must
    /// neither fail nor be judged an escape because the on-disk spelling
    /// differs from the requested one.
    #[cfg(target_os = "macos")]
    #[test]
    fn adv_case_and_normalization_variants_are_not_escapes() {
        let (_guard, repo) = canonical_repo();
        fs::write(repo.join("Package.json"), br#"{"name":"case"}"#).expect("write");
        if repo.join("package.json").exists() {
            assert!(read_npm_manifest(&repo, "package.json")
                .expect("case-insensitive read")
                .is_some());
        }
        let nfc = "caf\u{e9}";
        let nfd = "cafe\u{301}";
        fs::create_dir(repo.join(nfc)).expect("mkdir");
        fs::write(repo.join(nfc).join("package.json"), br#"{"name":"u"}"#).expect("write");
        if repo.join(nfd).exists() {
            let rel = format!("{nfd}/package.json");
            assert!(read_npm_manifest(&repo, &rel)
                .expect("normalization-insensitive read")
                .is_some());
        }
    }

    /// The refusal reaches the report as one `manifest_unreadable` error no
    /// matter how many dispatchers asked, and an unreadable manifest leaves
    /// the map empty.
    #[cfg(unix)]
    #[test]
    fn adv_declared_sections_refusal_becomes_a_report_issue() {
        let repo = git_repo();
        let root = repo.path().canonicalize().expect("canonical");
        fs::write(root.join("big.json"), vec![b' '; 5 * 1024 * 1024]).expect("write big");
        std::os::unix::fs::symlink("big.json", root.join("package.json")).expect("symlink");
        let env = ScanEnv::from_options(&ScanOptions {
            run_cli: false,
            path_var: None,
            home: None,
        });
        let (mut report, _) = local_scan(&root, &env).unwrap();
        // Both dispatchers (and `local_scan`, if it saw the manifest) report
        // the one refusal: a single issue, at one severity.
        assert!(declared_sections_or_issue(&root, &root, &mut report).is_empty());
        assert!(declared_sections_or_issue(&root, &root, &mut report).is_empty());
        let refusals: Vec<_> = report
            .issues
            .iter()
            .filter(|i| i.code == "manifest_unreadable")
            .collect();
        assert_eq!(refusals.len(), 1, "{refusals:?}");
        assert_eq!(refusals[0].severity, "error");
        assert_eq!(refusals[0].path.as_deref(), Some("package.json"));
        assert!(
            refusals[0].message.contains("exceeds"),
            "{}",
            refusals[0].message
        );
    }

    /// Break (hypothesis under test): `pnpm-workspace.yaml` is still probed
    /// with the lexical `sandbox_join(..).is_file()`, which follows a symlink
    /// out of the repository, so an outside file decides workspace coverage.
    #[cfg(unix)]
    #[test]
    fn adv_pnpm_workspace_symlink_outside_the_repo_does_not_change_the_scan() {
        let (_guard, repo) = canonical_repo();
        let (_outside_guard, outside) = canonical_repo();
        fs::write(outside.join("ws.yaml"), "packages: ['packages/*']\n").expect("outside");
        fs::write(repo.join("package.json"), br#"{"name":"root"}"#).expect("root");
        fs::write(repo.join("pnpm-lock.yaml"), "lockfileVersion: 9\n").expect("lock");
        fs::create_dir_all(repo.join("packages/a")).expect("mkdir");
        fs::write(repo.join("packages/a/package.json"), br#"{"name":"a"}"#).expect("member");
        assert_eq!(
            resolve_lockfile(&repo, "packages/a"),
            None,
            "control: no workspace"
        );
        std::os::unix::fs::symlink(outside.join("ws.yaml"), repo.join("pnpm-workspace.yaml"))
            .expect("symlink");
        assert_eq!(
            resolve_lockfile(&repo, "packages/a"),
            None,
            "an out-of-repo pnpm-workspace.yaml must not be consulted"
        );
    }

    /// `detect_lockfile` shares the existence check: a lockfile that is a link
    /// out of the repository (or a FIFO) is not a lockfile, so it cannot cover
    /// a package, while an in-repo link still counts.
    #[cfg(unix)]
    #[test]
    fn a_lockfile_symlinked_outside_the_repo_is_not_detected() {
        let (_guard, repo) = canonical_repo();
        let (_outside_guard, outside) = canonical_repo();
        fs::write(outside.join("yarn.lock"), "# outside\n").expect("outside");
        std::os::unix::fs::symlink(outside.join("yarn.lock"), repo.join("yarn.lock"))
            .expect("symlink");
        assert_eq!(detect_lockfile(&repo, ""), None);
        let status = Command::new("mkfifo")
            .arg(repo.join("package-lock.json"))
            .status_locked()
            .expect("mkfifo");
        assert!(status.success());
        assert_eq!(detect_lockfile(&repo, ""), None, "a FIFO is not a lockfile");
        // Control: an in-repo link to a regular file still counts.
        fs::write(repo.join("real.lock"), "# real\n").expect("real");
        std::os::unix::fs::symlink("real.lock", repo.join("pnpm-lock.yaml")).expect("link");
        assert_eq!(
            detect_lockfile(&repo, "").as_deref(),
            Some("pnpm-lock.yaml")
        );
    }

    /// The `.cargo/config.toml` read shared the same size-gate bypass.
    #[cfg(unix)]
    #[test]
    fn a_cargo_config_symlinked_to_a_large_file_is_not_read() {
        let (_guard, repo) = canonical_repo();
        fs::create_dir(repo.join(".cargo")).expect("mkdir");
        let mut big = b"# auditable\n".to_vec();
        big.resize(5 * 1024 * 1024, b' ');
        fs::write(repo.join("big.toml"), big).expect("write big");
        std::os::unix::fs::symlink("../big.toml", repo.join(".cargo/config.toml"))
            .expect("symlink");
        fs::write(repo.join("Cargo.lock"), "").expect("lock");
        assert_eq!(auditable_config_path(&repo, &["Cargo.lock".into()]), None);
        // Control: a small config is read.
        fs::remove_file(repo.join(".cargo/config.toml")).expect("unlink");
        fs::write(repo.join(".cargo/config.toml"), "# auditable\n").expect("small");
        assert_eq!(
            auditable_config_path(&repo, &["Cargo.lock".into()]).as_deref(),
            Some(".cargo/config.toml")
        );
    }

    #[test]
    fn parse_cargo_audit_maps_advisory() {
        let json = r#"{
          "vulnerabilities": {
            "list": [
              {
                "advisory": {
                  "id": "RUSTSEC-2020-0071",
                  "package": "time",
                  "title": "Potential segfault in localtime_r invocations",
                  "url": "https://rustsec.org/advisories/RUSTSEC-2020-0071"
                },
                "versions": { "patched": [">=0.2.23"] }
              }
            ]
          }
        }"#;
        let (vulns, warnings) = parse_cargo_audit_json(json).unwrap();
        assert_eq!(vulns.len(), 1);
        assert_eq!(vulns[0].name, "time");
        assert_eq!(vulns[0].ecosystem, "cargo");
        assert_eq!(vulns[0].fix_available, ">=0.2.23");
        assert!(vulns[0].url.contains("RUSTSEC-2020-0071"));
        assert!(warnings.is_empty());
    }

    #[test]
    fn parse_cargo_audit_surfaces_informational_warnings() {
        let json = r#"{
          "vulnerabilities": {"list": []},
          "warnings": {
            "unsound": [{
              "package": {"name": "glib", "version": "0.18.5"},
              "advisory": {
                "id": "RUSTSEC-2024-0429",
                "title": "Unsound iterator implementation",
                "url": "https://rustsec.org/advisories/RUSTSEC-2024-0429"
              }
            }]
          }
        }"#;
        let (vulns, warnings) = parse_cargo_audit_json(json).unwrap();
        assert!(vulns.is_empty());
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].code, "cargo_audit_unsound");
        assert!(warnings[0].message.contains("glib 0.18.5"));
        assert!(warnings[0].message.contains("RUSTSEC-2024-0429"));
    }

    #[test]
    fn engines_mismatch_flags_too_old_and_too_new() {
        assert!(engines_mismatch(">=20", "v18.20.0").is_some());
        assert!(engines_mismatch(">=18", "v20.11.0").is_none());
        assert!(engines_mismatch(">=18.0.0-0", "v18.20.0").is_none());
        assert!(engines_mismatch("18.0.0-1", "v18.20.0").is_none());
        assert!(engines_mismatch(">=18 <20", "v22.0.0").is_some());
        assert!(engines_mismatch(">=18 <20", "v18.5.0").is_none());
        assert!(engines_mismatch("^18.0.0", "v16.0.0").is_some());
        assert!(engines_mismatch("*", "v12.0.0").is_none());
        assert!(engines_mismatch("not-a-spec", "v20.0.0").is_none());
        assert_eq!(parse_node_version("v20.11.1"), Some([20, 11, 1]));
    }

    /// Regression (audit M8): a hyphen range used to overwrite its own
    /// minimum with the upper bound, flagging Node 17 against `16 - 18`.
    #[test]
    fn engines_hyphen_range_is_treated_as_inclusive_interval() {
        assert!(engines_mismatch("16.4.0 - 18.2.0", "v17.9.0").is_none());
        // Reason this changed: it asserted 18.2.99 satisfies `... - 18.2.0`,
        // an artifact of comparing majors only. A full-version upper bound is
        // inclusive of that version and nothing above it.
        assert!(engines_mismatch("16.4.0 - 18.2.0", "v18.2.0").is_none());
        assert!(engines_mismatch("16.4.0 - 18.2.0", "v18.2.99").is_some());
        assert!(engines_mismatch("16.4.0 - 18.2.0", "v15.0.0").is_some());
        assert!(engines_mismatch("16.4.0 - 18.2.0", "v19.0.0").is_some());
        // Glued form without spaces.
        assert!(engines_mismatch("16.4.0-18.2.0", "v17.9.0").is_none());
        assert!(engines_mismatch("16.4.0-18.2.0", "v19.0.0").is_some());
    }

    /// OR clauses are alternatives: satisfying ANY one of them is enough.
    #[test]
    fn engines_or_clauses_satisfy_either_side() {
        assert!(engines_mismatch(">=18 || <=14", "v19.3.0").is_none());
        assert!(engines_mismatch(">=18 || <=14", "v13.0.0").is_none());
        assert!(engines_mismatch(">=18 || <=14", "v16.0.0").is_some());
        // Major-granularity conservativeness: carets set floors only.
        assert!(engines_mismatch("^18 || ^16", "v15.0.0").is_some());
        assert!(engines_mismatch("^18 || ^16", "v16.14.0").is_none());
    }

    /// One unparsable token must not discard the bounds parsed around it.
    #[test]
    fn engines_garbage_token_does_not_discard_valid_bounds() {
        assert!(engines_mismatch(">=18 banana <20", "v19.0.0").is_none());
        assert!(engines_mismatch(">=18 banana <20", "v17.0.0").is_some());
        assert!(engines_mismatch(">=18 banana <20", "v21.0.0").is_some());
    }

    #[test]
    fn local_scan_flags_missing_lockfile_lifecycle_and_skips_node_modules() {
        let repo = git_repo();
        write(
            repo.path(),
            "package.json",
            r#"{
              "name": "demo",
              "version": "1.0.0",
              "engines": { "node": ">=99" },
              "scripts": { "postinstall": "echo pwned", "test": "vitest" },
              "dependencies": { "leftpad": "1.0.0" }
            }"#,
        );
        write(
            repo.path(),
            "node_modules/evil/package.json",
            r#"{"name":"evil","scripts":{"postinstall":"x"}}"#,
        );
        write(repo.path(), "src/main.rs", "fn main() {}\n");
        write(
            repo.path(),
            "Cargo.toml",
            "[package]\nname = \"demo\"\nversion = \"1.2.3\"\nedition = \"2021\"\n",
        );
        git_add(repo.path(), "package.json");
        git_add(repo.path(), "src/main.rs");
        git_add(repo.path(), "Cargo.toml");

        let report = DepsScanner::scan_with(
            repo.path().to_str().unwrap(),
            ScanOptions {
                run_cli: false,
                path_var: None,
                home: None,
            },
        )
        .expect("scan");
        assert!(
            report.manifests.iter().any(|m| m.path == "package.json"),
            "root package.json: {:?}",
            report.manifests
        );
        assert!(
            !report
                .manifests
                .iter()
                .any(|m| m.path.contains("node_modules")),
            "node_modules must not be inventoried: {:?}",
            report.manifests
        );
        let root = report
            .manifests
            .iter()
            .find(|m| m.path == "package.json")
            .unwrap();
        assert_eq!(root.lifecycle_scripts, vec!["postinstall"]);
        assert_eq!(root.dep_count, 1);
        assert!(root.lockfile.is_none());
        assert!(report.issues.iter().any(|i| i.code == "missing_lockfile"));
        assert!(report.issues.iter().any(|i| i.code == "lifecycle_scripts"));
        if report.node_version.is_some() {
            assert!(
                report.issues.iter().any(|i| i.code == "engines_node"),
                "engines >=99 should mismatch {:?}: {:?}",
                report.node_version,
                report.issues
            );
        }
        assert!(
            report.ecosystems.iter().any(|e| e.family == "cargo"),
            "rust file should hint cargo: {:?}",
            report.ecosystems
        );
    }

    #[test]
    fn lockfile_and_package_manager_detection() {
        let repo = git_repo();
        write(
            repo.path(),
            "package.json",
            r#"{"name":"w","packageManager":"pnpm@9.1.0","workspaces":["packages/*"]}"#,
        );
        write(repo.path(), "pnpm-lock.yaml", "lockfileVersion: '9.0'\n");
        git_add(repo.path(), "package.json");
        git_add(repo.path(), "pnpm-lock.yaml");
        let report = DepsScanner::scan_with(
            repo.path().to_str().unwrap(),
            ScanOptions {
                run_cli: false,
                path_var: None,
                home: None,
            },
        )
        .unwrap();
        let root = &report.manifests[0];
        assert_eq!(root.lockfile.as_deref(), Some("pnpm-lock.yaml"));
        assert_eq!(root.package_manager, "pnpm");
        assert!(root.has_workspaces);
        assert!(!report.issues.iter().any(|i| i.code == "missing_lockfile"));
    }

    #[test]
    fn nested_package_is_scanned_when_no_root_manifest() {
        let repo = git_repo();
        write(
            repo.path(),
            "frontend/package.json",
            r#"{"name":"ui","version":"0.0.1"}"#,
        );
        write(
            repo.path(),
            "frontend/package-lock.json",
            r#"{"lockfileVersion":3}"#,
        );
        git_add(repo.path(), "frontend/package.json");
        git_add(repo.path(), "frontend/package-lock.json");
        let report = DepsScanner::scan_with(
            repo.path().to_str().unwrap(),
            ScanOptions {
                run_cli: false,
                path_var: None,
                home: None,
            },
        )
        .unwrap();
        assert_eq!(report.manifests.len(), 1);
        assert_eq!(report.manifests[0].path, "frontend/package.json");
        assert_eq!(
            report.manifests[0].lockfile.as_deref(),
            Some("package-lock.json")
        );
        assert_eq!(
            npm_scan_roots(&report.manifests),
            vec!["frontend".to_string()]
        );
    }

    #[test]
    fn empty_markdown_repo_has_no_manifests() {
        let repo = git_repo();
        write(repo.path(), "README.md", "# docs\n");
        git_add(repo.path(), "README.md");
        let report = DepsScanner::scan_with(
            repo.path().to_str().unwrap(),
            ScanOptions {
                run_cli: false,
                path_var: None,
                home: None,
            },
        )
        .unwrap();
        assert!(report.manifests.is_empty());
        assert!(report.vulnerabilities.is_empty());
        assert!(report.outdated.is_empty());
    }

    #[test]
    fn severity_rank_orders_critical_first() {
        assert!(severity_rank("critical") < severity_rank("high"));
        assert!(severity_rank("high") < severity_rank("moderate"));
        assert_eq!(severity_rank("medium"), severity_rank("moderate"));
    }

    // -- pip-audit ------------------------------------------------------------

    #[test]
    fn parse_pip_audit_docs_fixture() {
        let json = r#"[
          {
            "name": "flask",
            "version": "0.5",
            "vulns": [
              {
                "id": "PYSEC-2019-179",
                "fix_versions": ["1.0"],
                "aliases": ["CVE-2019-1010083", "GHSA-5wv5-4vpf-pj6m"],
                "description": "The Pallets Project Flask before 1.0 is affected."
              },
              {
                "id": "PYSEC-2018-66",
                "fix_versions": [],
                "aliases": [],
                "description": ""
              }
            ]
          }
        ]"#;
        let vulns = parse_pip_audit_json(json).expect("parse");
        assert_eq!(vulns.len(), 2);
        let first = &vulns[0];
        assert_eq!(first.name, "flask");
        assert_eq!(first.ecosystem, "pypi");
        assert_eq!(first.severity, SEVERITY_UNKNOWN);
        assert!(first.is_direct);
        assert_eq!(first.range, "==0.5");
        assert_eq!(first.title, "CVE-2019-1010083");
        assert_eq!(first.fix_available, "1.0");
        assert_eq!(
            first.via,
            vec!["CVE-2019-1010083", "GHSA-5wv5-4vpf-pj6m", "PYSEC-2019-179"]
        );
        let second = &vulns[1];
        assert_eq!(second.fix_available, "no");
        assert_eq!(second.title, "PYSEC-2018-66");
    }

    #[test]
    fn parse_pip_audit_rejects_unrecognised_shapes() {
        // An object without `dependencies` is not a report we understand.
        assert!(parse_pip_audit_json("{\"total\": 0}").is_err());
        assert!(parse_pip_audit_json("not json").is_err());
        assert!(parse_pip_audit_json("").is_err());
        assert!(
            parse_pip_audit_json("{\"dependencies\": \"nope\"}").is_err(),
            "non-array dependencies must fail loud"
        );
        // Empty array and empty dependency list are clean bills of health.
        assert!(parse_pip_audit_json("[]").unwrap().is_empty());
        assert!(parse_pip_audit_json("{\"dependencies\": []}")
            .unwrap()
            .is_empty());
    }

    /// Captured verbatim from pip-audit 2.10.1:
    /// `pip-audit --no-deps --disable-pip --format json -r requirements.txt`
    /// against `flask==0.5`. Current releases wrap rows in `dependencies`,
    /// unlike the bare-array shape shown in the published examples.
    #[test]
    fn parse_pip_audit_live_2_10_shape() {
        let json = r#"{"dependencies": [{"name": "flask", "version": "0.5", "vulns": [{"id": "PYSEC-2019-179", "fix_versions": ["1.0"], "aliases": ["GHSA-5wv5-4vpf-pj6m", "CVE-2019-1010083"], "description": "The Pallets Project Flask before 1.0 is affected by: unexpected memory usage."}, {"id": "PYSEC-2018-66", "fix_versions": ["0.12.3"], "aliases": ["CVE-2018-1000656", "GHSA-562c-5r94-xh97"], "description": "CWE-20."}]}]}"#;
        let vulns = parse_pip_audit_json(json).expect("live-shape parse");
        assert_eq!(vulns.len(), 2);
        let first = &vulns[0];
        assert_eq!(first.name, "flask");
        assert_eq!(first.range, "==0.5");
        assert_eq!(first.fix_available, "1.0");
        assert_eq!(
            first.via,
            vec!["GHSA-5wv5-4vpf-pj6m", "CVE-2019-1010083", "PYSEC-2019-179"]
        );
        assert_eq!(vulns[1].fix_available, "0.12.3");
    }

    #[test]
    fn parse_pip_audit_tolerates_sparse_rows() {
        let json = r#"[
          {"name": "", "vulns": [{"id": "X"}]},
          {"name": "no-vulns"},
          {"name": "weird", "vulns": "not-an-array"},
          {"name": "ok", "version": null, "vulns": [{"fix_versions": ["2.0"]}]}
        ]"#;
        let vulns = parse_pip_audit_json(json).unwrap();
        assert_eq!(vulns.len(), 1);
        assert_eq!(vulns[0].name, "ok");
        assert_eq!(vulns[0].range, "==");
        assert_eq!(vulns[0].fix_available, "2.0");
        assert!(vulns[0].via.is_empty());
    }

    #[test]
    fn parse_pip_audit_preserves_total_for_report_level_capping() {
        let mut rows = Vec::new();
        for i in 0..(MAX_VULNS * 4) {
            rows.push(format!(
                r#"{{"name":"pkg{i}","version":"1.0","vulns":[{{"id":"VULN-{i}"}}]}}"#
            ));
        }
        let text = format!("[{}]", rows.join(","));
        let vulns = parse_pip_audit_json(&text).unwrap();
        assert_eq!(vulns.len(), MAX_VULNS * 4);
    }

    // -- govulncheck ----------------------------------------------------------

    const GOVULN_STREAM: &str = r#"{"config":{"protocol_version":"v1.0.0","scanner_name":"govulncheck","go_version":"go1.24.0"}}
{"progress":{"message":"Scanning your code and P packages across M dependent modules..."}}
{"osv":{"id":"GO-2021-0111","summary":"Due to improper index validation in parseOID","details":"long details","references":[{"type":"FIX","url":"https://go.dev/cl/xxx"}]}}
{"osv":{"id":"GO-2022-0969","summary":"","references":[{"url":"https://example.com/a"}]}}
{"finding":{"osv":"GO-2021-0111","fixed_version":"v0.3.7","trace":[{"module":"golang.org/x/text","version":"v0.3.5"}]}}
{"finding":{"osv":"GO-2022-0969","fixed_version":"","trace":[{"module":"stdlib","version":"go1.24.0"}]}}
{"finding":{"osv":"GO-2021-0111","fixed_version":"v0.3.7","trace":[{"module":"golang.org/x/text","version":"v0.3.5","package":"golang.org/x/text/language"}]}}
{"finding":{"osv":"GO-2021-0111","fixed_version":"v0.3.7","trace":[{"module":"golang.org/x/text","version":"v0.3.5","package":"golang.org/x/text/language","function":"Parse"},{"module":"tmp","function":"main"}]}}
"#;

    #[test]
    fn parse_govulncheck_keeps_module_level_and_dedupes() {
        // A trailing garbage line used to ride along in this fixture and be
        // skipped; corrupt segments are now an error (see
        // govulncheck_killed_midstream_is_not_clean).
        assert!(parse_govulncheck_stream(&format!("{GOVULN_STREAM}not-json-at-all\n")).is_err());
        let vulns = parse_govulncheck_stream(GOVULN_STREAM).expect("parse");
        assert_eq!(vulns.len(), 2, "one row per (osv, module): {vulns:?}");
        let xtext = vulns
            .iter()
            .find(|v| v.name == "golang.org/x/text")
            .unwrap();
        assert_eq!(xtext.ecosystem, "go");
        assert_eq!(xtext.severity, SEVERITY_UNKNOWN);
        assert_eq!(xtext.range, "v0.3.5");
        assert_eq!(xtext.fix_available, "v0.3.7");
        assert_eq!(xtext.title, "Due to improper index validation in parseOID");
        assert_eq!(xtext.url, "https://go.dev/cl/xxx");
        assert_eq!(xtext.via, vec!["GO-2021-0111"]);
        let stdlib = vulns.iter().find(|v| v.name == "stdlib").unwrap();
        assert_eq!(
            stdlib.fix_available, "no",
            "empty fixed_version means no fix"
        );
        assert_eq!(
            stdlib.title, "GO-2022-0969",
            "falls back to id without summary"
        );
    }

    #[test]
    fn parse_govulncheck_empty_and_all_garbage() {
        assert!(parse_govulncheck_stream("").is_err());
        assert!(parse_govulncheck_stream("   \n \n").is_err());
        assert!(parse_govulncheck_stream("garbage\nmore garbage").is_err());
    }

    #[test]
    fn parse_govulncheck_survives_truncated_stream_mid_line() {
        let truncated =
            "{\"config\":{\"protocol_version\":\"v1.0.0\"}}\n{\"finding\":{\"osv\":\"GO-202";
        // Reason this changed: it used to assert the partial line is skipped
        // and the stream reads as zero findings, which reports a killed run
        // as clean. A truncated stream is now an error.
        let err = parse_govulncheck_stream(truncated).unwrap_err();
        assert!(err.contains("truncated or corrupt"), "{err}");
    }

    /// Captured verbatim (abbreviated) from govulncheck v1.7.0 against a
    /// module pinning golang.org/x/text v0.3.5. Current releases emit
    /// pretty-printed, multi-line, simply-concatenated objects — not NDJSON —
    /// and 172 `osv` advisory records accompany only 17 finding messages.
    #[test]
    fn parse_govulncheck_live_1_7_pretty_stream() {
        let live = r#"{
  "config": { "protocol_version": "v1.0.0", "scanner_name": "govulncheck" }
}
{
  "progress": { "message": "Fetching vulnerabilities from the database..." }
}
{
  "osv": {
    "id": "GO-2021-0113",
    "summary": "Out-of-bounds read in golang.org/x/text/language",
    "references": [{ "type": "FIX", "url": "https://go.dev/cl/340830" }]
  }
}
{
  "osv": {
    "id": "GO-2022-1059",
    "summary": "Denial of service via crafted Accept-Language header"
  }
}
{
  "finding": {
    "osv": "GO-2021-0113",
    "fixed_version": "v0.3.7",
    "trace": [{ "module": "golang.org/x/text", "version": "v0.3.5" }]
  }
}
{
  "finding": {
    "osv": "GO-2022-1059",
    "fixed_version": "v0.3.8",
    "trace": [{ "module": "golang.org/x/text", "version": "v0.3.5" }]
  }
}
{
  "finding": {
    "osv": "GO-2021-0113",
    "fixed_version": "v0.3.7",
    "trace": [
      {
        "module": "golang.org/x/text",
        "version": "v0.3.5",
        "package": "golang.org/x/text/language",
        "function": "Parse"
      }
    ]
  }
}"#;
        let vulns = parse_govulncheck_stream(live).expect("live pretty stream parses");
        assert_eq!(
            vulns.len(),
            2,
            "package/symbol tiers collapse into module row"
        );
        for v in &vulns {
            assert_eq!(v.name, "golang.org/x/text");
            assert_eq!(v.range, "v0.3.5");
            assert_eq!(v.severity, SEVERITY_UNKNOWN);
        }
        let go113 = vulns.iter().find(|v| v.via == ["GO-2021-0113"]).unwrap();
        assert_eq!(go113.fix_available, "v0.3.7");
        assert_eq!(
            go113.title,
            "Out-of-bounds read in golang.org/x/text/language"
        );
        assert_eq!(go113.url, "https://go.dev/cl/340830");
        let go1059 = vulns
            .iter()
            .find(|v| v.title.contains("Accept-Language"))
            .unwrap();
        assert_eq!(
            go1059.fix_available, "v0.3.8",
            "title from summary, no url field"
        );
        assert_eq!(go1059.url, "https://pkg.go.dev/vuln/GO-2022-1059");
    }

    #[test]
    fn parse_govulncheck_rejects_scalar_only_output() {
        // Parses as JSON values but contains no message objects.
        assert!(parse_govulncheck_stream("42\n").is_err());
        assert!(parse_govulncheck_stream("\"hello\"\n").is_err());
    }

    // -- composer audit -------------------------------------------------------

    #[test]
    fn parse_composer_advisories_with_defaults() {
        let json = r#"{
          "advisories": {
            "monolog/monolog": [
              {
                "advisoryId": "PKSA-3j7p-xwgr-q22g",
                "packageName": "monolog/monolog",
                "affectedVersions": ">=1.10.0,<1.12.0",
                "title": "Header injection",
                "cve": null,
                "link": "https://github.com/Seldaek/monolog/pull/143",
                "reportedAt": "2016-07-13T14:00:00+00:00"
              }
            ],
            "guzzlehttp/guzzle": [
              {
                "advisoryId": "PKSA-x",
                "CVE": "CVE-2022-31042",
                "severity": "HIGH",
                "affectedVersions": "<7.4.5"
              },
              {
                "advisoryId": "PKSA-y",
                "severity": "low",
                "description": "Certify error"
              }
            ]
          }
        }"#;
        let vulns = parse_composer_audit_json(json).expect("parse");
        assert_eq!(vulns.len(), 3);
        let monolog = vulns.iter().find(|v| v.name == "monolog/monolog").unwrap();
        assert_eq!(
            monolog.severity, "moderate",
            "missing severity defaults to moderate"
        );
        assert_eq!(monolog.title, "Header injection");
        assert_eq!(monolog.url, "https://github.com/Seldaek/monolog/pull/143");
        assert_eq!(monolog.range, ">=1.10.0,<1.12.0");
        assert_eq!(monolog.via, vec!["PKSA-3j7p-xwgr-q22g"]);
        assert_eq!(monolog.ecosystem, "composer");
        assert!(!monolog.is_direct);
        let guzzle_high = vulns.iter().find(|v| v.title != "Certify error").unwrap();
        assert_eq!(
            guzzle_high.severity, "high",
            "severity normalises to lowercase"
        );
        assert_eq!(
            guzzle_high.url, "https://www.cve.org/CVERecord?id=CVE-2022-31042",
            "CVE fallback URL when link absent"
        );
    }

    #[test]
    fn parse_composer_rejects_bad_shapes_but_accepts_clean() {
        assert!(parse_composer_audit_json("{\"advisories\": {}}")
            .unwrap()
            .is_empty());
        // Captured clean-report shape from Composer 2.10: empty array.
        assert!(
            parse_composer_audit_json("{\"advisories\": [], \"abandoned\": [], \"filter\": []}")
                .unwrap()
                .is_empty(),
            "real clean output must not read as a failure"
        );
        assert!(parse_composer_audit_json("[]").is_err());
        assert!(parse_composer_audit_json("{}").is_err());
        assert!(parse_composer_audit_json("{").is_err());
        let error = parse_composer_audit_json(
            r#"{"error":{"code":"ELOCK","summary":"No lockfile found"}}"#,
        )
        .unwrap_err();
        assert!(error.contains("ELOCK") && error.contains("lockfile"));
        // Non-object advisory entries are skipped, not fatal.
        let mixed = r#"{"advisories":{"a":[{"advisoryId":"A1"}],"b":"corrupt"}}"#;
        let vulns = parse_composer_audit_json(mixed).unwrap();
        assert_eq!(vulns.len(), 1);
        assert_eq!(vulns[0].name, "a");
    }

    /// Fields captured verbatim from Composer 2.10.x real output (Packagist
    /// advisory API): `sources` arrays, nullable `cve`, `severity` strings,
    /// and multi-range `affectedVersions`.
    #[test]
    fn parse_composer_live_2_10_fields() {
        let json = r#"{
          "advisories": {
            "guzzlehttp/guzzle": [
              {
                "advisoryId": "PKSA-fy2t-3c5f-827y",
                "packageName": "guzzlehttp/guzzle",
                "affectedVersions": "<7.15.1",
                "title": "Guzzle: URI fragments disclosed in redirect Referer headers",
                "cve": null,
                "link": "https://github.com/advisories/GHSA-h95v-h523-3mw8",
                "reportedAt": "2026-07-20T23:28:36+00:00",
                "sources": [{ "name": "GitHub", "remoteId": "GHSA-h95v-h523-3mw8" }],
                "severity": "medium"
              }
            ],
            "league/flysystem": [
              {
                "advisoryId": "PKSA-pwh8-d4fr-nywn",
                "packageName": "league/flysystem",
                "affectedVersions": "<1.1.4|>=2.0.0,<2.1.1",
                "title": "TOCTOU Race Condition enabling remote code execution",
                "cve": "CVE-2021-32708",
                "link": "https://github.com/thephpleague/flysystem/security/advisories/GHSA-9f46-5r25-5wfm",
                "reportedAt": "2021-06-23T23:56:59+00:00"
              }
            ]
          }
        }"#;
        let vulns = parse_composer_audit_json(json).expect("live-shape parse");
        assert_eq!(vulns.len(), 2);
        let guzzle = vulns
            .iter()
            .find(|v| v.name == "guzzlehttp/guzzle")
            .unwrap();
        assert_eq!(guzzle.severity, "medium");
        assert_eq!(
            guzzle.title,
            "Guzzle: URI fragments disclosed in redirect Referer headers"
        );
        assert_eq!(guzzle.range, "<7.15.1");
        // cve null + link present → link wins, no synthetic CVE url.
        assert_eq!(
            guzzle.url,
            "https://github.com/advisories/GHSA-h95v-h523-3mw8"
        );
        let fly = vulns.iter().find(|v| v.name == "league/flysystem").unwrap();
        assert_eq!(
            fly.severity, "moderate",
            "severity absent → moderate default"
        );
        assert_eq!(
            fly.url,
            "https://github.com/thephpleague/flysystem/security/advisories/GHSA-9f46-5r25-5wfm"
        );
    }

    #[test]
    fn parse_composer_preserves_total_and_falls_back_to_package_key() {
        let mut entries = Vec::new();
        for i in 0..(MAX_VULNS + 20) {
            entries.push(format!(r#"{{"advisoryId":"ID-{i}"}}"#));
        }
        let json = format!(r#"{{"advisories":{{"pkg":[{}]}}}}"#, entries.join(","));
        let vulns = parse_composer_audit_json(&json).unwrap();
        assert_eq!(vulns.len(), MAX_VULNS + 20);
        // packageName absent → key used.
        assert_eq!(vulns[0].name, "pkg");
    }

    // -- summary accounting ---------------------------------------------------

    #[test]
    fn audit_summary_counts_unknown_bucket() {
        let vulns = vec![
            Vulnerability {
                name: "a".into(),
                severity: "critical".into(),
                is_direct: true,
                title: String::new(),
                url: String::new(),
                range: String::new(),
                fix_available: String::new(),
                via: vec![],
                ecosystem: "npm".into(),
            },
            Vulnerability {
                name: "b".into(),
                severity: SEVERITY_UNKNOWN.into(),
                is_direct: true,
                title: String::new(),
                url: String::new(),
                range: String::new(),
                fix_available: String::new(),
                via: vec![],
                ecosystem: "pypi".into(),
            },
            Vulnerability {
                name: "c".into(),
                severity: "bogus".into(),
                is_direct: false,
                title: String::new(),
                url: String::new(),
                range: String::new(),
                fix_available: String::new(),
                via: vec![],
                ecosystem: "npm".into(),
            },
        ];
        let summary = AuditSummary::from_vulns(&vulns);
        assert_eq!(summary.critical, 1);
        assert_eq!(summary.unknown, 1);
        assert_eq!(summary.info, 1, "truly alien strings stay informational");
        assert_eq!(summary.total, 3);
    }

    #[test]
    fn report_level_caps_keep_exact_observed_totals() {
        let repo = git_repo();
        let env = ScanEnv::from_options(&ScanOptions {
            run_cli: false,
            path_var: None,
            home: None,
        });
        let (mut report, _) = local_scan(repo.path(), &env).unwrap();
        for index in 0..(MAX_ISSUES + 7) {
            report.issues.push(HealthIssue {
                severity: "warning".into(),
                code: format!("issue_{index}"),
                message: format!("issue {index}"),
                path: None,
            });
        }
        cap_report(&mut report);
        assert_eq!(report.issues.len(), MAX_ISSUES);
        assert!(report.truncated);
        assert!(report.limit_notices.iter().any(|notice| {
            notice.resource == "health issues"
                && notice.kept == MAX_ISSUES
                && notice.total == MAX_ISSUES + 7
        }));
    }

    #[test]
    fn inventory_display_cap_does_not_skip_audit_targets() {
        let repo = git_repo();
        for index in 0..MAX_ECOSYSTEM_MANIFESTS + 1 {
            let dir = repo.path().join(format!("crate-{index}"));
            std::fs::create_dir(&dir).unwrap();
            std::fs::write(dir.join("Cargo.toml"), "[package]\nname = 'fixture'\n").unwrap();
        }
        std::fs::write(repo.path().join("Cargo.lock"), "version = 4\n").unwrap();
        let env = ScanEnv::from_options(&ScanOptions {
            run_cli: false,
            path_var: None,
            home: None,
        });
        let (mut report, targets) = local_scan(repo.path(), &env).unwrap();
        assert_eq!(targets.cargo_locks, vec!["Cargo.lock"]);
        assert!(report.truncated);
        assert_eq!(
            report.ecosystems[0].manifests.len(),
            MAX_ECOSYSTEM_MANIFESTS
        );
        report.cargo_audit_present = true;
        report.scanners_ran = vec!["cargo".into()];
        assert!(
            audit_is_complete(&report, &targets, true),
            "inventory display did not omit an audit target"
        );
        assert!(!audit_is_complete(&report, &targets, false));
        report.scanners_ran.clear();
        assert!(!audit_is_complete(&report, &targets, true));
        report.scanners_ran.push("cargo".into());
        let complete = report.clone();
        for code in ["cargo_audit_failed", "audit_cwd"] {
            let mut failed = complete.clone();
            push_issue(
                &mut failed.issues,
                "error",
                code,
                "fixture failure".into(),
                None,
            );
            assert!(!audit_is_complete(&failed, &targets, true));
        }
        let mut legacy = complete.clone();
        legacy.limit_notices = vec![serde_json::from_str(
            r#"{"resource":"cargo ecosystem artifacts","kept":24,"total":783}"#,
        )
        .unwrap()];
        assert!(!audit_is_complete(&legacy, &targets, true));
        legacy.limit_notices.clear();
        assert!(!audit_is_complete(&legacy, &targets, true));
        record_limit(&mut report, "cargo lockfiles", 6, 7);
        assert!(!audit_is_complete(&report, &targets, true));
    }

    // -- bundler-audit ----------------------------------------------------------

    /// Captured verbatim (abbreviated description) from bundler-audit 0.9.3:
    /// `bundler-audit check --format json` against a Gemfile.lock pinning
    /// nokogiri 1.10.3 / rack 2.0.7.
    #[test]
    fn parse_bundler_audit_live_0_9_shape() {
        let json = r#"{
          "version": "0.9.3",
          "created_at": "2026-08-25 09:39:24 -0500",
          "results": [
            {
              "type": "unpatched_gem",
              "gem": { "name": "nokogiri", "version": "1.10.3" },
              "advisory": {
                "path": "/Users/x/.local/share/ruby-advisory-db/gems/nokogiri/CVE-2019-13118.yml",
                "id": "CVE-2019-13118",
                "url": "https://bugs.chromium.org/p/oss-fuzz/issues/detail?id=15069",
                "title": "libxslt Type Confusion vulnerability that affects Nokogiri",
                "cvss_v2": null,
                "cvss_v3": 7.5,
                "cve": "2019-13118",
                "osvdb": null,
                "ghsa": "cf46-6xxh-pc75",
                "unaffected_versions": [],
                "patched_versions": [">= 1.10.5"]
              },
              "insecure_version": "1.10.3"
            },
            {
              "type": "unpatched_gem",
              "gem": { "name": "rack", "version": "2.0.7" },
              "advisory": {
                "id": "CVE-2020-8185",
                "url": "https://github.com/rack/rack/security/advisories/GHSA-j4xc-xh5h-q7pr",
                "title": "Possible DoS vector in Rack::File",
                "patched_versions": ["~> 2.1.4", ">= 2.2.3"],
                "ghsa": "j4xc-xh5h-q7pr"
              }
            }
          ]
        }"#;
        let vulns = parse_bundler_audit_json(json).expect("parse");
        assert_eq!(vulns.len(), 2);
        let nokogiri = &vulns[0];
        assert_eq!(nokogiri.name, "nokogiri");
        assert_eq!(nokogiri.ecosystem, "ruby");
        assert_eq!(nokogiri.severity, "high", "cvss_v3 7.5 maps to high");
        assert_eq!(
            nokogiri.title,
            "libxslt Type Confusion vulnerability that affects Nokogiri"
        );
        assert_eq!(nokogiri.range, "==1.10.3");
        assert_eq!(nokogiri.fix_available, ">= 1.10.5");
        assert_eq!(
            nokogiri.via,
            vec!["CVE-2019-13118", "GHSA-cf46-6xxh-pc75"],
            "advisory id plus GHSA alias"
        );
        let rack = &vulns[1];
        assert_eq!(
            rack.severity, SEVERITY_UNKNOWN,
            "no cvss fields published → unranked, not fabricated"
        );
        assert_eq!(
            rack.fix_available, "~> 2.1.4, >= 2.2.3",
            "multiple patched versions join"
        );
    }

    #[test]
    fn parse_bundler_audit_clean_and_degenerate_shapes() {
        // Clean lockfiles omit or empty `results`.
        assert!(parse_bundler_audit_json("{\"version\":\"0.9.3\"}")
            .unwrap()
            .is_empty());
        assert!(parse_bundler_audit_json("{\"results\": []}")
            .unwrap()
            .is_empty());
        // Results without an advisory object still surface, unranked.
        let bare = r#"{"results":[{"type":"insecure_source","gem":{"name":"x","version":"1.0"}}]}"#;
        let vulns = parse_bundler_audit_json(bare).unwrap();
        assert_eq!(vulns.len(), 1);
        assert_eq!(vulns[0].severity, SEVERITY_UNKNOWN);
        assert_eq!(vulns[0].title, "x is vulnerable");
        assert_eq!(vulns[0].fix_available, "no");
        assert!(parse_bundler_audit_json("{\"results\": \"corrupt\"}").is_err());
        assert!(parse_bundler_audit_json("not json").is_err());
    }

    #[test]
    fn parse_bundler_audit_cvss_v2_fallback_preserves_total() {
        let mut results = Vec::new();
        for i in 0..(MAX_VULNS + 5) {
            results.push(format!(
                r#"{{"type":"unpatched_gem","gem":{{"name":"g{i}","version":"1"}},"advisory":{{"id":"A-{i}","cvss_v2":{}}}}}"#,
                if i == 0 { 4.3 } else { 0.0 }
            ));
        }
        let text = format!(r#"{{"results":[{}]}}"#, results.join(","));
        let vulns = parse_bundler_audit_json(&text).unwrap();
        assert_eq!(vulns.len(), MAX_VULNS + 5);
        assert!(vulns.iter().any(|v| v.via.contains(&"A-0".to_string())));
    }

    // -- scanner failure surfacing ---------------------------------------------

    use crate::engine::git_cli::{found_but_failed_detail, CapturedOutput};

    fn captured(status: i32, success: bool, stdout: &str, stderr: &str) -> CapturedOutput {
        CapturedOutput {
            stdout: stdout.as_bytes().to_vec(),
            stderr: stderr.as_bytes().to_vec(),
            success,
            status_code: status,
        }
    }

    #[test]
    fn scanner_stdout_fails_loud_when_tool_exits_without_output() {
        let mut issues = Vec::new();
        let out = captured(2, false, "", "network unreachable");
        let text = scanner_stdout(
            Ok(out),
            &mut issues,
            "pip_audit_failed",
            "pip-audit",
            Some("requirements.txt".into()),
        );
        assert!(text.is_none(), "failed run must not parse as clean");
        assert_eq!(issues.len(), 1, "silent skip is forbidden: {issues:?}");
        assert_eq!(issues[0].code, "pip_audit_failed");
        assert!(issues[0].message.contains("network unreachable"));
        assert_eq!(issues[0].path.as_deref(), Some("requirements.txt"));
    }

    #[test]
    fn scanner_stdout_names_exit_status_when_stderr_is_mute() {
        let mut issues = Vec::new();
        let out = captured(137, false, "", "");
        let text = scanner_stdout(
            Ok(out),
            &mut issues,
            "govulncheck_failed",
            "govulncheck",
            None,
        );
        assert!(text.is_none());
        assert!(issues[0].message.contains("137"), "{:?}", issues[0].message);
    }

    #[test]
    fn scanner_stdout_treats_success_with_findings_payload_normally() {
        // pip-audit exits 1 when findings exist; the JSON still parses.
        let mut issues = Vec::new();
        let out = captured(1, false, r#"[{"name":"flask","vulns":[]}]"#, "noise");
        let text = scanner_stdout(Ok(out), &mut issues, "pip_audit_failed", "pip-audit", None);
        assert_eq!(text.as_deref(), Some(r#"[{"name":"flask","vulns":[]}]"#));
        assert!(issues.is_empty());
    }

    // -- target collection ------------------------------------------------------

    #[test]
    fn collect_side_targets_report_exact_caps_for_each_family() {
        let mut targets = ScanTargets::default();
        for i in 0..MAX_PY_REQUIREMENTS + 3 {
            collect_side_target(
                "requirements-test.txt",
                &format!("requirements{i}.txt"),
                &mut targets,
            );
        }
        for i in 0..MAX_GO_MODS + 2 {
            collect_side_target("go.mod", &format!("svc{i}/go.mod"), &mut targets);
        }
        let mut notices = Vec::new();
        cap_scan_targets(&mut targets, &mut notices);
        assert_eq!(targets.py_requirements.len(), MAX_PY_REQUIREMENTS);
        assert_eq!(targets.go_mods.len(), MAX_GO_MODS);
        assert_eq!(targets.composer_locks.len(), 0);
        assert!(notices.iter().any(|notice| {
            notice.resource == "Python audit targets"
                && notice.kept == MAX_PY_REQUIREMENTS
                && notice.total == MAX_PY_REQUIREMENTS + 3
        }));
        assert!(notices.iter().any(|notice| {
            notice.resource == "Go audit targets"
                && notice.kept == MAX_GO_MODS
                && notice.total == MAX_GO_MODS + 2
        }));
    }

    #[test]
    fn collect_side_targets_matches_requirements_variants() {
        let cases = [
            ("requirements.txt", true),
            ("requirements-dev.txt", true),
            ("requirements_prod.txt", true),
            ("constraints.txt", true),
            ("requirements.in", false),
            ("requirements.txt.bak", false),
            ("myrequirements.txt", false),
        ];
        for (name, expected) in cases {
            let mut targets = ScanTargets::default();
            collect_side_target(name, name, &mut targets);
            assert_eq!(
                targets.py_requirements.is_empty(),
                !expected,
                "{name} should be {}",
                if expected { "collected" } else { "skipped" }
            );
        }
    }

    #[test]
    fn local_scan_reports_scanner_presence_flags() {
        let repo = git_repo();
        write(repo.path(), "requirements.txt", "flask==0.5\n");
        write(repo.path(), "go.mod", "module tmp\n\ngo 1.21\n");
        git_add(repo.path(), "requirements.txt");
        git_add(repo.path(), "go.mod");
        let (report, targets) =
            local_scan(repo.path(), &ScanEnv::from_options(&ScanOptions::default())).unwrap();
        // Probes are gated on artifacts; when a CLI cannot be run at all,
        // presence must be false — never silently true. The gate goes through
        // production resolution (capture_command), which also searches the
        // GUI-launch fallback dirs such as ~/go/bin.
        if unrunnable("pip-audit") {
            assert!(!report.pip_audit_present);
        }
        if unrunnable("govulncheck") {
            assert!(!report.govulncheck_present);
        }
        assert_eq!(
            targets.py_requirements,
            vec!["requirements.txt".to_string()]
        );
        assert_eq!(targets.go_mods, vec!["go.mod".to_string()]);
        assert!(
            report
                .ecosystems
                .iter()
                .any(|e| e.family == "python" || e.family == "go"),
            "ecosystem hints present: {:?}",
            report.ecosystems
        );
    }

    /// True when `program` cannot be spawned and exited-for at all — decided
    /// through the production spawn path so fallback-dir resolution (GUI
    /// launches, ~/go/bin, …) is honored exactly as in the app.
    fn unrunnable(program: &str) -> bool {
        crate::engine::git_cli::capture_command(
            program,
            &["--version"],
            None,
            std::time::Duration::from_secs(5),
            &[],
        )
        .map(|out| !out.success)
        .unwrap_or(true)
    }

    #[test]
    fn npm_scan_roots_prefers_repository_root() {
        let manifests = vec![
            NpmManifest {
                path: "package.json".into(),
                name: "root".into(),
                version: "1".into(),
                private: true,
                license: None,
                engines_node: None,
                package_manager: "npm".into(),
                lockfile: Some("package-lock.json".into()),
                lockfile_dir: Some(String::new()),
                has_workspaces: true,
                dep_count: 0,
                dev_dep_count: 0,
                optional_dep_count: 0,
                peer_dep_count: 0,
                lifecycle_scripts: vec![],
            },
            NpmManifest {
                path: "packages/a/package.json".into(),
                name: "a".into(),
                version: "1".into(),
                private: true,
                license: None,
                engines_node: None,
                package_manager: "npm".into(),
                lockfile: None,
                lockfile_dir: None,
                has_workspaces: false,
                dep_count: 0,
                dev_dep_count: 0,
                optional_dep_count: 0,
                peer_dep_count: 0,
                lifecycle_scripts: vec![],
            },
        ];
        assert_eq!(npm_scan_roots(&manifests), vec![String::new()]);
    }

    /// THE CLASS: a Health scan reads dependency metadata; it must not become
    /// a way for a checkout to run its own code. `npm_config_ignore_scripts`
    /// already holds that line for npm. Composer loads plugin packages out of
    /// the project's own `vendor/` on every startup — `audit` included — and
    /// the project's `composer.json` is what allow-lists them, so the same
    /// rule has to be spelled on this invocation too.
    #[cfg(unix)]
    #[test]
    fn a_composer_audit_never_loads_the_projects_own_plugins_or_scripts() {
        let repo = git_repo();
        write(repo.path(), "composer.lock", "{\"packages\":[]}\n");
        write(repo.path(), "composer.json", "{\"name\":\"demo/demo\"}\n");
        git_add(repo.path(), "composer.lock");
        git_add(repo.path(), "composer.json");

        let stubs = TempDir::new().unwrap();
        let argv_log = stubs.path().join("composer-argv");
        write_exec(
            stubs.path(),
            "composer",
            &format!(
                "#!/bin/sh\nif [ \"$1\" = --version ]; then echo 'Composer 2.10.0'; exit 0; fi\n\
                 echo \"$*\" >> {}\necho '{{\"advisories\":{{}}}}'\nexit 0\n",
                argv_log.display()
            ),
        );
        let path_var =
            std::env::join_paths([stubs.path(), Path::new("/usr/bin"), Path::new("/bin")]).unwrap();

        let report = DepsScanner::scan_with(
            repo.path().to_str().unwrap(),
            ScanOptions {
                run_cli: true,
                path_var: Some(path_var),
                home: None,
            },
        )
        .expect("scan");

        assert!(
            report.scanners_ran.contains(&"composer".to_string()),
            "composer did not run: {:?}",
            report.scanners_ran
        );
        let argv = fs::read_to_string(&argv_log).expect("composer recorded no invocation");
        assert!(
            argv.contains("--no-plugins"),
            "composer audit would activate the repository's own plugins: {argv}"
        );
        assert!(
            argv.contains("--no-scripts"),
            "composer audit would run the repository's own scripts: {argv}"
        );
    }

    /// Third instance of the same class, and the one that reaches furthest:
    /// loading a module's packages reads its own `go` and `toolchain` lines,
    /// and under the default `GOTOOLCHAIN=auto` the go command downloads and
    /// executes the toolchain those lines name. A `toolchain` line in a
    /// checked-out `go.mod` therefore chooses which binary a Health scan runs.
    ///
    /// Measured against the real tools before this was written: with
    /// GitPulse's own env pairs, `govulncheck -json ./...` on a module
    /// declaring `toolchain go1.99.7` reports
    /// `go: downloading go1.99.7 (darwin/arm64)` and exits 1; with
    /// `GOTOOLCHAIN=local` the same scan exits 0.
    #[cfg(unix)]
    #[test]
    fn a_go_scan_never_lets_the_module_choose_the_toolchain_that_runs() {
        let repo = git_repo();
        write(
            repo.path(),
            "go.mod",
            "module demo\n\ngo 1.26.4\n\ntoolchain go1.99.7\n",
        );
        git_add(repo.path(), "go.mod");

        let stubs = TempDir::new().unwrap();
        let env_log = stubs.path().join("govulncheck-env");
        write_exec(
            stubs.path(),
            "govulncheck",
            &format!(
                "#!/bin/sh\nif [ \"$1\" = -version ]; then echo 'govulncheck v1.8.0'; exit 0; fi\n\
                 echo \"GOTOOLCHAIN=${{GOTOOLCHAIN-<unset>}}\" >> {}\n\
                 echo '{{\"config\":{{\"scanner_name\":\"govulncheck\"}}}}'\nexit 0\n",
                env_log.display()
            ),
        );
        let path_var =
            std::env::join_paths([stubs.path(), Path::new("/usr/bin"), Path::new("/bin")]).unwrap();

        let report = DepsScanner::scan_with(
            repo.path().to_str().unwrap(),
            ScanOptions {
                run_cli: true,
                path_var: Some(path_var),
                home: None,
            },
        )
        .expect("scan");

        assert!(
            report.scanners_ran.contains(&"govulncheck".to_string()),
            "govulncheck did not run: {:?}",
            report.scanners_ran
        );
        let seen = fs::read_to_string(&env_log).expect("govulncheck recorded no invocation");
        assert!(
            seen.contains("GOTOOLCHAIN=local"),
            "the module's own toolchain line still decides what runs: {seen}"
        );
    }

    /// `audit` is an external cargo subcommand, not a built-in, so `[alias]`
    /// in the checkout's `.cargo/config.toml` shadows it — `alias.audit =
    /// ["run", …]` turns the scan into a build and run of repository code.
    /// Reaching the subcommand binary directly leaves no alias table in front
    /// of it.
    #[cfg(unix)]
    #[test]
    fn a_cargo_audit_does_not_go_through_the_repositorys_alias_table() {
        let repo = git_repo();
        write(repo.path(), "Cargo.lock", "# generated\nversion = 3\n");
        write(
            repo.path(),
            "Cargo.toml",
            "[package]\nname = \"demo\"\nversion = \"1.0.0\"\n",
        );
        write(
            repo.path(),
            ".cargo/config.toml",
            "[alias]\naudit = [\"run\", \"--bin\", \"payload\"]\n",
        );
        git_add(repo.path(), "Cargo.lock");
        git_add(repo.path(), "Cargo.toml");
        git_add(repo.path(), ".cargo/config.toml");

        let stubs = TempDir::new().unwrap();
        let aliased = stubs.path().join("went-through-cargo");
        write_exec(
            stubs.path(),
            "cargo-audit",
            "#!/bin/sh\nif [ \"$2\" = --version ]; then echo 'cargo-audit 0.22.0'; exit 0; fi\n\
             echo '{\"vulnerabilities\":{\"list\":[]},\"warnings\":{}}'\nexit 0\n",
        );
        write_exec(
            stubs.path(),
            "cargo",
            &format!(
                "#!/bin/sh\nif [ \"$1\" = audit ]; then echo \"$*\" >> {}; fi\n\
                 if [ \"$1\" = audit ] && [ \"$2\" = --version ]; then echo 'cargo-audit 0.22.0'; exit 0; fi\nexit 64\n",
                aliased.display()
            ),
        );
        let path_var =
            std::env::join_paths([stubs.path(), Path::new("/usr/bin"), Path::new("/bin")]).unwrap();

        let report = DepsScanner::scan_with(
            repo.path().to_str().unwrap(),
            ScanOptions {
                run_cli: true,
                path_var: Some(path_var),
                home: None,
            },
        )
        .expect("scan");

        assert_eq!(report.scanners_ran, vec!["cargo".to_string()]);
        assert!(
            !aliased.exists(),
            "the audit was dispatched through cargo, where the repository's alias decides what runs: {}",
            fs::read_to_string(&aliased).unwrap_or_default()
        );
    }

    #[cfg(unix)]
    #[test]
    fn scan_audits_nested_cargo_lockfiles_with_their_explicit_path() {
        let repo = git_repo();
        write(
            repo.path(),
            "src-tauri/Cargo.lock",
            "# generated\nversion = 3\n",
        );
        write(
            repo.path(),
            "src-tauri/Cargo.toml",
            "[package]\nname = \"demo\"\nversion = \"1.2.3\"\n",
        );
        git_add(repo.path(), "src-tauri/Cargo.lock");
        git_add(repo.path(), "src-tauri/Cargo.toml");

        let stubs = TempDir::new().unwrap();
        write_exec(
            stubs.path(),
            "cargo",
            "#!/bin/sh\nif [ \"$1\" = audit ] && [ \"$2\" = --version ]; then echo 'cargo-audit 0.22.0'; exit 0; fi\nif [ \"$1\" = audit ] && [ \"$2\" = --json ] && [ \"$3\" = --file ] && [ \"$4\" = src-tauri/Cargo.lock ]; then echo '{\"vulnerabilities\":{\"list\":[]},\"warnings\":{}}'; exit 0; fi\necho \"unexpected args: $*\" >&2\nexit 64\n",
        );
        let path_var =
            std::env::join_paths([stubs.path(), Path::new("/usr/bin"), Path::new("/bin")]).unwrap();

        let report = DepsScanner::scan_with(
            repo.path().to_str().unwrap(),
            ScanOptions {
                run_cli: true,
                path_var: Some(path_var),
                home: None,
            },
        )
        .expect("scan");

        assert_eq!(report.scanners_ran, vec!["cargo".to_string()]);
        assert!(
            report.audit_complete,
            "clean nested audit should be complete"
        );
        assert!(
            !report
                .issues
                .iter()
                .any(|issue| issue.code == "cargo_audit_failed"),
            "nested lockfile audit failed: {:?}",
            report.issues
        );
    }

    /// cargo-deny, cargo-crev, and cargo-auditable (via `cargo audit bin`)
    /// join the same health scan as cargo-audit. Each tool is the direct
    /// binary, so a repository cargo alias cannot run in its place.
    #[cfg(unix)]
    #[test]
    fn rust_supply_chain_tools_feed_the_health_report() {
        let repo = git_repo();
        write(repo.path(), "Cargo.lock", "# generated\nversion = 3\n");
        write(
            repo.path(),
            "Cargo.toml",
            "[package]\nname = \"demo\"\nversion = \"1.0.0\"\n",
        );
        write(
            repo.path(),
            "deny.toml",
            "[advisories]\nvulnerability = \"deny\"\n",
        );
        git_add(repo.path(), "Cargo.lock");
        git_add(repo.path(), "Cargo.toml");
        git_add(repo.path(), "deny.toml");
        let release = repo.path().join("target").join("release");
        fs::create_dir_all(&release).unwrap();
        // The section name is what selects a binary for `cargo audit bin`.
        write_exec(&release, "demo", "#!/bin/sh\n# .dep-v0\nexit 0\n");
        let home = TempDir::new().unwrap();
        let crev_cfg = home.path().join(".config").join("crev");
        fs::create_dir_all(&crev_cfg).unwrap();
        fs::write(crev_cfg.join("config.yaml"), "version: 0\n").unwrap();

        let stubs = TempDir::new().unwrap();
        let cargo_log = stubs.path().join("cargo-invoked");
        let deny_log = stubs.path().join("deny-args");
        let crev_log = stubs.path().join("crev-env");
        let sarif = stubs.path().join("deny.sarif");
        fs::write(
            &sarif,
            r#"{"runs":[{"results":[
                {"level":"error","message":{"text":"Uncontrolled recursion"},"partialFingerprints":{"cargo-deny/advisory-id":"RUSTSEC-2019-0001","cargo-deny/krate":"ammonia@0.7.0"},"ruleId":"a:vulnerability"},
                {"level":"error","message":{"text":"license is not explicitly allowed"},"partialFingerprints":{"cargo-deny/krate":"leftpad@1.0.0"},"ruleId":"l:rejected"}
            ]}]}"#,
        )
        .unwrap();
        write_exec(
            stubs.path(),
            "cargo-audit",
            "#!/bin/sh\nif [ \"$2\" = --version ]; then echo 'cargo-audit 0.22.0'; exit 0; fi\nif [ \"$4\" = bin ]; then echo \"demo was not built with 'cargo auditable', the report will be incomplete\" >&2; echo '{\"vulnerabilities\":{\"list\":[{\"advisory\":{\"id\":\"RUSTSEC-2024-9999\",\"package\":\"frombin\",\"title\":\"partial\"}}]}}'; exit 0; fi\necho '{\"vulnerabilities\":{\"list\":[]},\"warnings\":{}}'\nexit 0\n",
        );
        write_exec(
            stubs.path(),
            "cargo-deny",
            &format!(
                "#!/bin/sh\nif [ \"$1\" = --version ]; then echo 'cargo-deny 0.20.2'; exit 0; fi\necho \"$*\" >> {}\ncat {}\nexit 1\n",
                deny_log.display(),
                sarif.display()
            ),
        );
        write_exec(
            stubs.path(),
            "cargo-crev",
            &format!(
                "#!/bin/sh\nif [ \"$1\" = crev ] && [ \"$2\" = --help ]; then exit 0; fi\necho \"OFFLINE=${{CARGO_NET_OFFLINE-<unset>}}\" >> {}\necho '{{\"name\":\"libc\",\"version\":\"0.2.1\",\"source\":\"registry\",\"status\":\"warn\"}}'\nexit 1\n",
                crev_log.display()
            ),
        );
        write_exec(
            stubs.path(),
            "cargo",
            &format!(
                "#!/bin/sh\necho \"$*\" >> {}\nexit 64\n",
                cargo_log.display()
            ),
        );
        let path_var =
            std::env::join_paths([stubs.path(), Path::new("/usr/bin"), Path::new("/bin")]).unwrap();
        let report = DepsScanner::scan_with(
            repo.path().to_str().unwrap(),
            ScanOptions {
                run_cli: true,
                path_var: Some(path_var),
                home: Some(home.path().as_os_str().to_owned()),
            },
        )
        .expect("scan");

        assert!(
            !cargo_log.exists(),
            "a cargo alias path was used: {}",
            fs::read_to_string(&cargo_log).unwrap_or_default()
        );
        let deny_args = fs::read_to_string(&deny_log).unwrap_or_default();
        assert!(
            deny_args.contains("--offline")
                && deny_args.contains("sarif")
                && deny_args.contains("check"),
            "cargo-deny was not the offline SARIF check: {deny_args}"
        );
        let crev_env = fs::read_to_string(&crev_log).unwrap_or_default();
        assert!(
            crev_env.contains("OFFLINE=true"),
            "cargo-crev was allowed to use the network: {crev_env}"
        );
        assert_eq!(
            report.scanners_ran,
            vec![
                "cargo".to_string(),
                "cargo-crev".to_string(),
                "cargo-deny".to_string()
            ]
        );
        assert!(report.vulnerabilities.iter().any(|vuln| {
            vuln.name == "ammonia" && vuln.range == "RUSTSEC-2019-0001" && vuln.ecosystem == "cargo"
        }));
        assert!(
            !report
                .vulnerabilities
                .iter()
                .any(|vuln| vuln.name == "frombin"),
            "panic-message recovery was merged: {:?}",
            report.vulnerabilities
        );
        assert!(report
            .issues
            .iter()
            .any(|issue| issue.code == "cargo_deny_license"));
        assert!(report
            .issues
            .iter()
            .any(|issue| issue.code == "cargo_crev_trust"));
        assert!(report
            .issues
            .iter()
            .any(|issue| issue.code == "cargo_auditable_missing"));
        assert!(
            report.audit_complete,
            "findings are not scanner failures: {:?}",
            report.issues
        );
    }

    #[cfg(unix)]
    #[test]
    fn deny_toml_without_cargo_deny_is_not_a_complete_audit() {
        let repo = git_repo();
        write(repo.path(), "Cargo.lock", "# generated\nversion = 3\n");
        write(
            repo.path(),
            "Cargo.toml",
            "[package]\nname = \"demo\"\nversion = \"1.0.0\"\n",
        );
        write(repo.path(), "deny.toml", "[advisories]\n");
        git_add(repo.path(), "Cargo.lock");
        git_add(repo.path(), "Cargo.toml");
        git_add(repo.path(), "deny.toml");
        let stubs = TempDir::new().unwrap();
        write_exec(
            stubs.path(),
            "cargo-audit",
            "#!/bin/sh\nif [ \"$2\" = --version ]; then echo 'cargo-audit 0.22.0'; exit 0; fi\necho '{\"vulnerabilities\":{\"list\":[]},\"warnings\":{}}'\nexit 0\n",
        );
        let path_var =
            std::env::join_paths([stubs.path(), Path::new("/usr/bin"), Path::new("/bin")]).unwrap();
        let report = DepsScanner::scan_with(
            repo.path().to_str().unwrap(),
            ScanOptions {
                run_cli: true,
                path_var: Some(path_var),
                home: None,
            },
        )
        .expect("scan");
        assert!(report.cargo_audit_present);
        assert!(!report.cargo_deny_present);
        assert!(!report
            .scanners_ran
            .iter()
            .any(|scanner| scanner == "cargo-deny"));
        assert!(!report.audit_complete);
    }

    /// An explicit `path_var` is the whole search path. The GUI-launch
    /// fallback dirs exist for an *inherited* minimal PATH; searched under a
    /// caller's PATH they made the seam leak, so a test that hid cargo-deny
    /// still saw `/opt/homebrew/bin/cargo-deny` on any host that had one.
    /// The tool sits in `~/.cargo/bin` of a scratch HOME — a fallback dir the
    /// seam itself controls — so this holds on every host.
    #[cfg(unix)]
    #[test]
    fn an_explicit_path_var_is_not_widened_by_gui_fallback_dirs() {
        let repo = git_repo();
        write(repo.path(), "Cargo.lock", "# generated\nversion = 3\n");
        write(
            repo.path(),
            "Cargo.toml",
            "[package]\nname = \"demo\"\nversion = \"1.0.0\"\n",
        );
        write(repo.path(), "deny.toml", "[advisories]\n");
        git_add(repo.path(), "Cargo.lock");
        git_add(repo.path(), "Cargo.toml");
        git_add(repo.path(), "deny.toml");
        let home = TempDir::new().unwrap();
        let cargo_bin = home.path().join(".cargo/bin");
        fs::create_dir_all(&cargo_bin).unwrap();
        let deny_log = home.path().join("cargo-deny-argv");
        write_exec(
            &cargo_bin,
            "cargo-deny",
            &format!(
                "#!/bin/sh\necho \"$*\" >> {}\necho 'cargo-deny 0.20.2'\nexit 0\n",
                deny_log.display()
            ),
        );
        let stubs = TempDir::new().unwrap();
        write_exec(
            stubs.path(),
            "cargo-audit",
            "#!/bin/sh\nif [ \"$2\" = --version ]; then echo 'cargo-audit 0.22.0'; exit 0; fi\necho '{\"vulnerabilities\":{\"list\":[]},\"warnings\":{}}'\nexit 0\n",
        );
        let path_var =
            std::env::join_paths([stubs.path(), Path::new("/usr/bin"), Path::new("/bin")]).unwrap();
        let report = DepsScanner::scan_with(
            repo.path().to_str().unwrap(),
            ScanOptions {
                run_cli: true,
                path_var: Some(path_var),
                home: Some(home.path().as_os_str().to_owned()),
            },
        )
        .expect("scan");
        assert!(report.cargo_audit_present);
        assert!(
            !deny_log.exists(),
            "cargo-deny outside the explicit PATH was spawned: {}",
            fs::read_to_string(&deny_log).unwrap_or_default()
        );
        assert!(!report.cargo_deny_present);
        assert!(!report.audit_complete);
    }

    /// `cargo crev verify` creates an identity when none exists. A Health scan
    /// must not be that write.
    #[cfg(unix)]
    #[test]
    fn crev_without_an_identity_is_not_invoked() {
        let repo = git_repo();
        write(repo.path(), "Cargo.lock", "# generated\nversion = 3\n");
        write(
            repo.path(),
            "Cargo.toml",
            "[package]\nname = \"demo\"\nversion = \"1.0.0\"\n",
        );
        git_add(repo.path(), "Cargo.lock");
        git_add(repo.path(), "Cargo.toml");
        let home = TempDir::new().unwrap();
        let stubs = TempDir::new().unwrap();
        let invoked = stubs.path().join("crev-invoked");
        write_exec(
            stubs.path(),
            "cargo-audit",
            "#!/bin/sh\nif [ \"$2\" = --version ]; then echo 'cargo-audit 0.22.0'; exit 0; fi\necho '{\"vulnerabilities\":{\"list\":[]},\"warnings\":{}}'\nexit 0\n",
        );
        write_exec(
            stubs.path(),
            "cargo-crev",
            &format!(
                "#!/bin/sh\necho \"$*\" >> {}\nif [ \"$1\" = crev ] && [ \"$2\" = --help ]; then exit 0; fi\nexit 0\n",
                invoked.display()
            ),
        );
        let path_var =
            std::env::join_paths([stubs.path(), Path::new("/usr/bin"), Path::new("/bin")]).unwrap();
        let report = DepsScanner::scan_with(
            repo.path().to_str().unwrap(),
            ScanOptions {
                run_cli: true,
                path_var: Some(path_var),
                home: Some(home.path().as_os_str().to_owned()),
            },
        )
        .expect("scan");
        let crev_calls = fs::read_to_string(&invoked).unwrap_or_default();
        assert!(
            !crev_calls.contains("verify"),
            "verify ran without an identity: {crev_calls}"
        );
        assert!(!report.cargo_crev_present);
        assert!(report
            .issues
            .iter()
            .any(|issue| issue.code == "cargo_crev_unconfigured"));
        assert!(
            report.audit_complete,
            "a missing crev identity is not a failed audit: {:?}",
            report.issues
        );
    }

    #[cfg(unix)]
    #[test]
    fn dispatched_scanner_failure_never_marks_audit_complete() {
        let repo = git_repo();
        write(
            repo.path(),
            "package.json",
            r#"{"name":"demo","version":"1.0.0"}"#,
        );
        write(
            repo.path(),
            "package-lock.json",
            r#"{"name":"demo","lockfileVersion":3}"#,
        );
        git_add(repo.path(), "package.json");
        git_add(repo.path(), "package-lock.json");

        let stubs = TempDir::new().unwrap();
        write_exec(
            stubs.path(),
            "node",
            "#!/bin/sh\nif [ \"$1\" = -v ]; then echo v20.0.0; fi\n",
        );
        write_exec(
            stubs.path(),
            "npm",
            "#!/bin/sh\ncase \"$1\" in\n  --version) echo 10.9.0 ;;\n  audit) echo 'not json' ;;\n  outdated) echo '{}' ;;\nesac\n",
        );
        let path_var =
            std::env::join_paths([stubs.path(), Path::new("/usr/bin"), Path::new("/bin")]).unwrap();
        let report = DepsScanner::scan_with(
            repo.path().to_str().unwrap(),
            ScanOptions {
                run_cli: true,
                path_var: Some(path_var),
                home: None,
            },
        )
        .expect("scan");

        // Reason this changed: a scanner whose output could not be parsed is
        // not "ran" (it used to be, because the command was dispatched).
        assert!(report.scanners_ran.is_empty(), "{:?}", report.scanners_ran);
        assert!(!report.audit_complete);
        assert!(report
            .issues
            .iter()
            .any(|issue| issue.code == "audit_failed"));
    }

    /// `bun.lock` is not an npm lockfile. `npm audit` exits ENOLOCK on it, and
    /// that used to be recorded as a failed audit of a project that had a
    /// lockfile all along.
    #[cfg(unix)]
    #[test]
    fn bun_lockfile_is_audited_with_bun_not_npm() {
        let repo = git_repo();
        write(
            repo.path(),
            "package.json",
            r#"{"name":"demo","version":"1.0.0","packageManager":"bun@1.4.2","dependencies":{"lodash":"4.17.15"}}"#,
        );
        write(repo.path(), "bun.lock", "{}\n");
        git_add(repo.path(), "package.json");
        git_add(repo.path(), "bun.lock");

        let stubs = TempDir::new().unwrap();
        let npm_log = stubs.path().join("npm-args");
        let bun_log = stubs.path().join("bun-args");
        write_exec(
            stubs.path(),
            "npm",
            &format!(
                "#!/bin/sh\necho \"$1\" >> {}\ncase \"$1\" in\n  --version) echo 11.0.0 ;;\n  audit) echo '{{\"error\":{{\"code\":\"ENOLOCK\",\"summary\":\"This command requires an existing lockfile.\",\"detail\":\"npm i\"}}}}' ;;\n  outdated) echo '{{}}' ;;\nesac\n",
                npm_log.display()
            ),
        );
        write_exec(
            stubs.path(),
            "bun",
            &format!(
                "#!/bin/sh\necho \"$1\" >> {}\ncase \"$1\" in\n  --version) echo 1.4.2 ;;\n  audit) echo '{{\"lodash\":[{{\"id\":1106913,\"url\":\"https://github.com/advisories/GHSA-35jh-r3h4-6jhm\",\"title\":\"Command Injection in lodash\",\"severity\":\"high\",\"vulnerable_versions\":\"<4.17.21\"}}]}}'; exit 1 ;;\nesac\n",
                bun_log.display()
            ),
        );
        let path_var =
            std::env::join_paths([stubs.path(), Path::new("/usr/bin"), Path::new("/bin")]).unwrap();
        let report = DepsScanner::scan_with(
            repo.path().to_str().unwrap(),
            ScanOptions {
                run_cli: true,
                path_var: Some(path_var),
                home: None,
            },
        )
        .expect("scan");

        let npm_args = fs::read_to_string(&npm_log).unwrap_or_default();
        assert!(
            !npm_args.lines().any(|line| line == "audit"),
            "npm audit must not run against bun.lock: {npm_args}"
        );
        assert!(
            npm_args.lines().any(|line| line == "outdated"),
            "npm outdated still runs: {npm_args}"
        );
        let bun_args = fs::read_to_string(&bun_log).unwrap_or_default();
        assert!(
            bun_args.lines().any(|line| line == "audit"),
            "bun audit must run: {bun_args}"
        );
        assert!(report.bun_cli_present, "{:?}", report.issues);
        assert!(report.scanners_ran.iter().any(|scanner| scanner == "bun"));
        assert!(
            !report.scanners_ran.iter().any(|scanner| scanner == "npm"),
            "outdated is not an audit: {:?}",
            report.scanners_ran
        );
        assert!(
            report.audit_complete,
            "a readable bun audit is complete even when it exits 1: {:?}",
            report.issues
        );
        assert!(report
            .issues
            .iter()
            .all(|issue| issue.code != "audit_failed" && issue.code != "bun_audit_failed"));
        let lodash = report
            .vulnerabilities
            .iter()
            .find(|vuln| vuln.name == "lodash")
            .expect("lodash advisory");
        assert_eq!(lodash.severity, "high");
        assert!(lodash.is_direct);
        assert_eq!(lodash.title, "Command Injection in lodash");
    }

    #[cfg(unix)]
    #[test]
    fn bun_audit_failure_does_not_mark_the_audit_complete() {
        let repo = git_repo();
        write(
            repo.path(),
            "package.json",
            r#"{"name":"demo","version":"1.0.0","packageManager":"bun@1.4.2"}"#,
        );
        write(repo.path(), "bun.lock", "{}\n");
        git_add(repo.path(), "package.json");
        git_add(repo.path(), "bun.lock");

        let stubs = TempDir::new().unwrap();
        write_exec(
            stubs.path(),
            "npm",
            "#!/bin/sh\ncase \"$1\" in\n  --version) echo 11.0.0 ;;\n  *) echo '{}' ;;\nesac\n",
        );
        write_exec(
            stubs.path(),
            "bun",
            "#!/bin/sh\ncase \"$1\" in\n  --version) echo 1.4.2 ;;\n  audit) echo 'audit request failed' >&2; exit 1 ;;\nesac\n",
        );
        let path_var =
            std::env::join_paths([stubs.path(), Path::new("/usr/bin"), Path::new("/bin")]).unwrap();
        let report = DepsScanner::scan_with(
            repo.path().to_str().unwrap(),
            ScanOptions {
                run_cli: true,
                path_var: Some(path_var),
                home: None,
            },
        )
        .expect("scan");

        assert!(!report.audit_complete);
        assert!(report.issues.iter().any(|issue| {
            issue.code == "bun_audit_failed" && issue.message.contains("audit request failed")
        }));
        assert!(!report
            .issues
            .iter()
            .any(|issue| issue.code == "audit_failed"));
    }

    #[cfg(unix)]
    #[test]
    fn yarn_lockfile_is_not_sent_to_npm_audit() {
        let repo = git_repo();
        write(
            repo.path(),
            "package.json",
            r#"{"name":"demo","version":"1.0.0","packageManager":"yarn@4.0.0"}"#,
        );
        write(repo.path(), "yarn.lock", "# yarn lockfile v1\n");
        git_add(repo.path(), "package.json");
        git_add(repo.path(), "yarn.lock");

        let stubs = TempDir::new().unwrap();
        let npm_log = stubs.path().join("npm-args");
        write_exec(
            stubs.path(),
            "npm",
            &format!(
                "#!/bin/sh\necho \"$1\" >> {}\ncase \"$1\" in\n  --version) echo 11.0.0 ;;\n  audit) echo '{{\"error\":{{\"code\":\"ENOLOCK\",\"summary\":\"lock\",\"detail\":\"no\"}}}}' ;;\n  *) echo '{{}}' ;;\nesac\n",
                npm_log.display()
            ),
        );
        let path_var =
            std::env::join_paths([stubs.path(), Path::new("/usr/bin"), Path::new("/bin")]).unwrap();
        let report = DepsScanner::scan_with(
            repo.path().to_str().unwrap(),
            ScanOptions {
                run_cli: true,
                path_var: Some(path_var),
                home: None,
            },
        )
        .expect("scan");

        let npm_args = fs::read_to_string(&npm_log).unwrap_or_default();
        assert!(
            !npm_args.lines().any(|line| line == "audit"),
            "npm audit must not run against yarn.lock: {npm_args}"
        );
        assert!(!report.audit_complete);
        assert!(report
            .issues
            .iter()
            .any(|issue| issue.code == "js_lockfile_unaudited"
                && issue.message.contains("yarn.lock")));
        assert!(!report
            .issues
            .iter()
            .any(|issue| issue.code == "audit_failed"));
    }

    // -- probe diagnosis & GUI-minimal PATH regressions ------------------------

    /// Writes an executable stub script (chmod 755, unix shebang) for the
    /// resolution tests below.
    #[cfg(unix)]
    fn write_exec(dir: &Path, name: &str, content: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        fs::write(&path, content).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    /// Regression (GUI launch, end-to-end through `scan_with(run_cli: true)`):
    /// with a Finder/Dock-style minimal PATH, npm must still be found and run.
    /// The stub directory holds `npm` — a script whose `#!/usr/bin/env node`
    /// interpreter is a tiny `node` shim placed in the SAME dir — so both the
    /// top-level resolution and the shebang interpreter lookup go through the
    /// injected PATH, exactly how the real `/opt/homebrew/bin/npm` +
    /// `/opt/homebrew/bin/node` pair behaves under a GUI-minimal PATH.
    ///
    /// Exact version strings are asserted so a real system npm/node cannot
    /// serve the probes unnoticed: either the stubs answered or the test fails.
    #[cfg(unix)]
    #[test]
    fn scan_resolves_npm_under_gui_minimal_path_end_to_end() {
        let stubs = TempDir::new().unwrap();
        write_exec(
            stubs.path(),
            "node",
            "#!/bin/sh\n# GitPulse test stub: minimal node stand-in.\ncase \"$1\" in\n  -v) echo \"v20.11.0\"; exit 0 ;;\nesac\ncase \"$2\" in\n  --version) echo \"10.9.0\"; exit 0 ;;\nesac\n# audit --json / outdated --json land here.\necho \"{}\"\n",
        );
        write_exec(
            stubs.path(),
            "npm",
            "#!/usr/bin/env node\n// GitPulse test stub: valid JS so either node answers identically.\nconst arg = process.argv[2] || \"\";\nif (arg === \"--version\") { console.log(\"10.9.0\"); } else { console.log(\"{}\"); }\n",
        );

        let repo = git_repo();
        write(
            repo.path(),
            "package.json",
            r#"{"name":"demo","version":"1.0.0"}"#,
        );
        write(
            repo.path(),
            "package-lock.json",
            r#"{"name":"demo","lockfileVersion":3}"#,
        );
        git_add(repo.path(), "package.json");
        git_add(repo.path(), "package-lock.json");

        // GUI-minimal PATH plus the stub dir PREPENDED, so the stubs win over
        // any real install sitting in the resolver's fallback dirs.
        let path_var = std::env::join_paths([
            stubs.path(),
            Path::new("/usr/bin"),
            Path::new("/bin"),
            Path::new("/usr/sbin"),
            Path::new("/sbin"),
        ])
        .unwrap();

        let report = DepsScanner::scan_with(
            repo.path().to_str().unwrap(),
            ScanOptions {
                run_cli: true,
                path_var: Some(path_var),
                home: None,
            },
        )
        .expect("scan");

        assert!(report.npm_cli_present, "issues: {:?}", report.issues);
        assert_eq!(report.npm_version.as_deref(), Some("10.9.0"));
        assert_eq!(report.node_version.as_deref(), Some("v20.11.0"));
        assert_eq!(report.scanners_ran, vec!["npm".to_string()]);
        assert!(
            !report.issues.iter().any(|i| i.code == "npm_missing"),
            "issues: {:?}",
            report.issues
        );
        assert!(
            !report
                .issues
                .iter()
                .any(|i| i.code == "audit_failed" || i.code == "outdated_failed"),
            "the stubs must answer cleanly: {:?}",
            report.issues
        );
    }

    /// An npm that RESOLVES but cannot run (interpreter gone — the exact shape
    /// of `/opt/homebrew/bin/npm` whose `env node` fails with exit 127) must be
    /// reported as found-but-broken, naming the resolved path and quoting the
    /// underlying error, never mislabeled as "not installed".
    #[cfg(unix)]
    #[test]
    fn local_scan_says_npm_was_found_when_it_exists_but_fails_to_run() {
        let repo = git_repo();
        write(
            repo.path(),
            "package.json",
            r#"{"name":"demo","version":"1.0.0"}"#,
        );
        git_add(repo.path(), "package.json");

        let stubs = TempDir::new().unwrap();
        let npm_stub = write_exec(
            stubs.path(),
            "npm",
            "#!/bin/sh\necho 'env: node: No such file or directory' >&2\nexit 127\n",
        );

        let report = DepsScanner::scan_with(
            repo.path().to_str().unwrap(),
            ScanOptions {
                run_cli: false,
                path_var: Some(stubs.path().as_os_str().to_os_string()),
                home: None,
            },
        )
        .expect("scan");

        assert!(!report.npm_cli_present);
        assert!(report.scanners_ran.is_empty(), "CLI enrichment was skipped");
        let issue = report
            .issues
            .iter()
            .find(|i| i.code == "npm_missing")
            .expect("npm_missing must still be raised");
        assert_eq!(issue.severity, "warning");
        let msg = &issue.message;
        assert!(msg.contains("was found"), "{msg}");
        assert!(
            msg.contains(npm_stub.to_string_lossy().as_ref()),
            "resolved path missing: {msg}"
        );
        assert!(
            msg.contains("env: node: No such file or directory"),
            "underlying error not quoted: {msg}"
        );
        assert!(
            !msg.contains("not installed"),
            "must not call an existing npm missing: {msg}"
        );
    }

    /// Wording contract of [`npm_missing_message`]: found-but-broken says so
    /// (and mentions the independently-missing node interpreter), while truly
    /// unresolved keeps the long-standing "not installed" text.
    #[test]
    fn npm_missing_message_matches_resolution_truth() {
        let env = ScanEnv {
            path_var: None,
            path_source: PathSource::Inherited,
            home: None,
            scan: None,
        };
        let broken = ToolProbe::FoundButFailed("env: node: No such file or directory".to_string());
        let node_gone = ToolProbe::NotFound(
            "Failed to spawn node: No such file or directory (os error 2)".to_string(),
        );
        let msg = npm_missing_message(&broken, &node_gone, &env);
        assert!(msg.contains("was found"), "{msg}");
        assert!(
            msg.contains("env: node: No such file or directory"),
            "{msg}"
        );
        assert!(
            msg.contains("Node.js interpreter") && msg.contains("was not found"),
            "node-missing-while-npm-present must be mentioned: {msg}"
        );
        assert!(!msg.contains("not installed"), "{msg}");

        let unresolved = ToolProbe::NotFound(
            "Failed to spawn npm: No such file or directory (os error 2)".to_string(),
        );
        let msg = npm_missing_message(&unresolved, &node_gone, &env);
        assert!(
            msg.contains("not installed or not on PATH"),
            "classic wording preserved: {msg}"
        );
        assert!(!msg.contains("was found"), "{msg}");
    }

    /// A timeout is found-but-failed and names the limit, never not-found.
    /// Empty output likewise classifies as failed-with-diagnosis.
    #[cfg(unix)]
    #[test]
    fn probe_classifies_timeout_and_empty_output_as_found_but_failed() {
        let dir = TempDir::new().unwrap();
        write_exec(dir.path(), "gitpulse-silent-probe", "#!/bin/sh\nexit 0\n");
        let silent_env = ScanEnv {
            path_var: Some(dir.path().as_os_str().to_os_string()),
            path_source: PathSource::Explicit,
            home: None,
            scan: None,
        };
        match probe_tool_version(
            "gitpulse-silent-probe",
            &["--version"],
            &silent_env,
            VERSION_TIMEOUT,
        ) {
            ToolProbe::FoundButFailed(detail) => {
                assert!(detail.contains("no version output"), "{detail}")
            }
            other => panic!("expected found-but-failed, got {other:?}"),
        }

        // Absolute /bin/sleep: the seam PATH deliberately excludes /bin, so a
        // bare `sleep` would die with "command not found" instead of timing out.
        write_exec(
            dir.path(),
            "gitpulse-slow-probe",
            "#!/bin/sh\n/bin/sleep 30\n",
        );
        let slow_env = ScanEnv {
            path_var: Some(dir.path().as_os_str().to_os_string()),
            path_source: PathSource::Explicit,
            home: None,
            scan: None,
        };
        match probe_tool_version(
            "gitpulse-slow-probe",
            &["--version"],
            &slow_env,
            Duration::from_secs(1),
        ) {
            ToolProbe::FoundButFailed(detail) => {
                assert!(detail.contains("timed out after 1s"), "{detail}");
                assert!(detail.contains("gitpulse-slow-probe"), "{detail}");
            }
            other => panic!("expected timeout classification, got {other:?}"),
        }
    }

    /// A binary resolving nowhere carries the raw spawn error text — the input
    /// for the honest "not installed" wording upstream.
    #[test]
    fn probe_reports_not_found_with_spawn_error_text() {
        let env = ScanEnv {
            path_var: Some(std::ffi::OsString::new()),
            path_source: PathSource::Explicit,
            home: None,
            scan: None,
        };
        match probe_tool_version(
            "gitpulse-nowhere-probe-xyz",
            &["--version"],
            &env,
            Duration::from_secs(2),
        ) {
            ToolProbe::NotFound(err) => {
                assert!(err.starts_with("Failed to spawn "), "{err}");
                assert!(err.contains("gitpulse-nowhere-probe-xyz"), "{err}");
            }
            other => panic!("expected not-found, got {other:?}"),
        }
    }

    /// Detail text shapes: non-zero exits quote stderr, fall back to the exit
    /// status when stderr is mute, and treat successful-but-empty runs as
    /// their own failure rather than a version.
    #[test]
    fn probe_detail_covers_exit_status_stderr_and_mute_success() {
        let detail = found_but_failed_detail(
            "npm",
            &captured(127, false, "", "env: node: No such file or directory"),
        );
        assert_eq!(detail, "env: node: No such file or directory");

        let detail = found_but_failed_detail("npm", &captured(137, false, "", ""));
        assert!(detail.contains("137"), "{detail}");
        assert!(detail.starts_with("npm exited"), "{detail}");

        let detail = found_but_failed_detail("npm", &captured(0, true, "", ""));
        assert!(detail.contains("no version output"), "{detail}");
    }

    /// `scanners_ran` is `serde(default)`-compatible: reports serialized by
    /// older builds (without the field) deserialize cleanly, and new payloads
    /// carry the field through a round-trip.
    #[test]
    fn deps_report_deserializes_without_scanners_ran_field() {
        let json = r#"{
            "node_version": null,
            "npm_version": null,
            "npm_cli_present": false,
            "cargo_audit_present": false,
            "pip_audit_present": false,
            "govulncheck_present": false,
            "composer_present": false,
            "bundler_audit_present": false,
            "manifests": [],
            "ecosystems": [],
            "issues": [],
            "vulnerabilities": [],
            "audit": {"info": 0, "low": 0, "moderate": 0, "high": 0, "critical": 0, "unknown": 0, "total": 0},
            "outdated": [],
            "truncated": false
        }"#;
        let report: DepsHealthReport = serde_json::from_str(json).expect("older payload loads");
        assert!(report.scanners_ran.is_empty());

        let mut fresh = report.clone();
        fresh.scanners_ran.push("npm".to_string());
        let text = serde_json::to_string(&fresh).unwrap();
        assert!(text.contains(r#""scanners_ran":["npm"]"#));
    }

    /// Stress: concurrent scans sharing one repo must not interfere, because
    /// the seam environment ([`ScanEnv`]) is per-scan and nothing in the
    /// scanner touches process-global state. If someone later introduces a
    /// shared cache or env mutation, the distinct stub versions below make the
    /// cross-talk visible instead of silently corrupting a report.
    #[cfg(unix)]
    #[test]
    fn concurrent_scans_with_distinct_envs_stay_isolated() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        let repo = git_repo();
        write(
            repo.path(),
            "package.json",
            r#"{"name":"demo","version":"1.0.0"}"#,
        );
        write(
            repo.path(),
            "package-lock.json",
            r#"{"name":"demo","lockfileVersion":3}"#,
        );
        git_add(repo.path(), "package.json");
        git_add(repo.path(), "package-lock.json");

        let rounds = 4;
        let ok = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::new();
        for round in 0..rounds {
            let repo_path = repo.path().to_path_buf();
            let ok = Arc::clone(&ok);
            handles.push(std::thread::spawn(move || {
                let stubs = TempDir::new().unwrap();
                write_exec(
                    stubs.path(),
                    "node",
                    "#!/bin/sh\ncase \"$1\" in -v) exit 0 ;; esac\nexit 0\n",
                );
                // Each thread's npm prints its own round number; a scan that
                // sees another thread's stub has leaked state across scans.
                let version = format!("9.9.{round}");
                write_exec(
                    stubs.path(),
                    "npm",
                    &format!("#!/bin/sh\nif [ \"$1\" = --version ]; then echo {version}; else echo '{{}}'; fi\n"),
                );
                let path_var =
                    std::env::join_paths([stubs.path(), Path::new("/usr/bin"), Path::new("/bin")])
                        .unwrap();
                let report = DepsScanner::scan_with(
                    repo_path.to_str().unwrap(),
                    ScanOptions {
                        run_cli: true,
                        path_var: Some(path_var),
                        home: None,
                    },
                )
                .expect("concurrent scan");
                assert_eq!(report.npm_version.as_deref(), Some(version.as_str()));
                assert_eq!(report.scanners_ran, vec!["npm".to_string()]);
                ok.fetch_add(1, Ordering::SeqCst);
            }));
        }
        for handle in handles {
            handle.join().expect("scan thread panicked");
        }
        assert_eq!(ok.load(Ordering::SeqCst), rounds);
    }
}

#[cfg(test)]
mod fail_closed_tests {
    use super::*;
    use crate::engine::git_cli::CapturedOutput;

    fn captured(status: i32, success: bool, stdout: &str, stderr: &str) -> CapturedOutput {
        CapturedOutput {
            stdout: stdout.as_bytes().to_vec(),
            stderr: stderr.as_bytes().to_vec(),
            success,
            status_code: status,
        }
    }

    /// The scanner_stdout -> parse_govulncheck_stream path enrich_go runs.
    /// Returns whether the run was reported (an issue or a parse error).
    fn go_reported(out: CapturedOutput) -> bool {
        let mut issues = Vec::new();
        match scanner_stdout(
            Ok(out),
            &mut issues,
            "govulncheck_failed",
            "govulncheck",
            None,
        ) {
            None => !issues.is_empty(),
            Some(text) => parse_govulncheck_stream(&text).is_err() || !issues.is_empty(),
        }
    }

    #[test]
    fn govulncheck_killed_midstream_is_not_clean() {
        let out = captured(
            137,
            false,
            "{\"config\":{\"protocol_version\":\"v1.0.0\"}}\n{\"finding\":{\"osv\":\"GO-202",
            "signal: killed",
        );
        assert!(go_reported(out));
    }

    #[test]
    fn govulncheck_exit_zero_empty_stdout_is_not_clean() {
        assert!(go_reported(captured(0, true, "", "")));
    }

    #[test]
    fn govulncheck_stream_without_config_is_not_a_complete_run() {
        let err = parse_govulncheck_stream(r#"{"osv":{"id":"GO-1"}}"#).unwrap_err();
        assert!(err.contains("config"), "{err}");
        assert!(
            parse_govulncheck_stream(r#"{"config":{"protocol_version":"v1.0.0"}}"#)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn scanner_stdout_flags_silent_success_for_every_scanner() {
        for program in ["pip-audit", "composer", "bundler-audit"] {
            let mut issues = Vec::new();
            let text = scanner_stdout(
                Ok(captured(0, true, "  \n", "")),
                &mut issues,
                "x_failed",
                program,
                None,
            );
            assert!(text.is_none());
            assert_eq!(issues.len(), 1, "{program}: {issues:?}");
        }
    }

    #[test]
    fn cargo_audit_without_vulnerabilities_section_is_not_clean() {
        let err =
            parse_cargo_audit_json(r#"{"error":"couldn't fetch advisory database"}"#).unwrap_err();
        assert!(err.contains("couldn't fetch"), "{err}");
        assert!(parse_cargo_audit_json(r#"{"vulnerabilities":{"found":false}}"#).is_err());
    }

    #[test]
    fn cargo_audit_count_must_match_the_list() {
        let doc = r#"{"vulnerabilities":{"found":true,"count":2,"list":[
            {"advisory":{"id":"RUSTSEC-1","package":"a"},"versions":{"patched":[]}}]}}"#;
        let err = parse_cargo_audit_json(doc).unwrap_err();
        assert!(err.contains("2") && err.contains("1"), "{err}");
        let ok = r#"{"vulnerabilities":{"found":false,"count":0,"list":[]}}"#;
        assert!(parse_cargo_audit_json(ok).unwrap().0.is_empty());
    }

    #[test]
    fn cvss_vectors_map_to_their_real_severity() {
        assert_eq!(
            cvss_to_severity("CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:H/A:H"),
            "critical"
        );
        assert_eq!(
            cvss_to_severity("CVSS:3.1/AV:P/AC:H/PR:H/UI:R/S:U/C:L/I:N/A:N"),
            "low"
        );
        // 6.1: the classic reflected-XSS vector.
        assert_eq!(
            cvss_to_severity("CVSS:3.1/AV:N/AC:L/PR:N/UI:R/S:C/C:L/I:L/A:N"),
            "moderate"
        );
        assert_eq!(
            cvss_to_severity("CVSS:3.0/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:N/A:N"),
            "high"
        );
        assert_eq!(cvss_to_severity("7.5"), "high");
    }

    #[test]
    fn unscorable_cvss_is_unknown_not_high() {
        for bad in [
            "",
            "garbage",
            "CVSS:4.0/AV:N/AC:L/AT:N/PR:N/UI:N/VC:H/VI:H/VA:H/SC:N/SI:N/SA:N",
            "CVSS:3.1/AV:N",
            "0.0",
        ] {
            assert_eq!(cvss_to_severity(bad), SEVERITY_UNKNOWN, "{bad}");
        }
    }

    #[test]
    fn cargo_audit_missing_cvss_is_unknown() {
        let doc = r#"{"vulnerabilities":{"count":1,"list":[
            {"advisory":{"id":"RUSTSEC-1","package":"a"},"versions":{"patched":[]}}]}}"#;
        assert_eq!(
            parse_cargo_audit_json(doc).unwrap().0[0].severity,
            SEVERITY_UNKNOWN
        );
    }

    // -- engines.node ranges ---------------------------------------------------

    #[test]
    fn engines_ranges_compare_full_versions() {
        // Spaced operator: used to read `20` as a minimum and flag v18.
        assert!(engines_mismatch(">=18 < 20", "v18.20.0").is_none());
        assert!(engines_mismatch(">=18 < 20", "v20.0.0").is_some());
        // `< 20` alone used to read `20` as a minimum and miss v22.
        assert!(engines_mismatch("< 20", "v22.0.0").is_some());
        assert!(engines_mismatch("< 20", "v18.0.0").is_none());
        // Minor-level strict bound: 18.20.0 is above 18.5.0.
        assert!(engines_mismatch(">18.5.0", "v18.20.0").is_none());
        assert!(engines_mismatch(">18.5.0", "v18.5.0").is_some());
        assert!(engines_mismatch(">=18.5.0 <18.6", "v18.5.9").is_none());
        assert!(engines_mismatch(">=18.5.0 <18.6", "v18.6.0").is_some());
        assert!(engines_mismatch("<=20.1", "v20.1.9").is_none());
        assert!(engines_mismatch("~18.5", "v18.6.0").is_some());
        assert!(engines_mismatch("^0.2.3", "v0.3.0").is_some());
        assert!(engines_mismatch("18.x", "v18.19.0").is_none());
        assert!(engines_mismatch("18.x", "v20.0.0").is_some());
        assert!(engines_mismatch("16 - 18", "v18.9.0").is_none());
        assert!(engines_mismatch("16 - 18", "v19.0.0").is_some());
    }

    // -- coverage: found versus audited -----------------------------------------

    #[cfg(unix)]
    fn scan_with_stubs(repo: &Path, scripts: &[(&str, &str)]) -> DepsHealthReport {
        use std::os::unix::fs::PermissionsExt;
        let stubs = tempfile::TempDir::new().unwrap();
        for (name, body) in scripts {
            let path = stubs.path().join(name);
            std::fs::write(&path, body).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let path_var =
            std::env::join_paths([stubs.path(), Path::new("/usr/bin"), Path::new("/bin")]).unwrap();
        DepsScanner::scan_with(
            repo.to_str().unwrap(),
            ScanOptions {
                run_cli: true,
                path_var: Some(path_var),
                home: None,
            },
        )
        .expect("scan")
    }

    #[cfg(unix)]
    fn git_add_all(repo: &Path) {
        use crate::procguard::LockedSpawn;
        let status = std::process::Command::new("git")
            .args(["add", "-A"])
            .current_dir(repo)
            .status_locked()
            .unwrap();
        assert!(status.success());
    }

    #[cfg(unix)]
    const NODE_STUB: (&str, &str) = (
        "node",
        "#!/bin/sh\nif [ \"$1\" = -v ]; then echo v20.0.0; fi\n",
    );
    #[cfg(unix)]
    const CLEAN_NPM_STUB: (&str, &str) = (
        "npm",
        "#!/bin/sh\ncase \"$1\" in\n  --version) echo 10.9.0 ;;\n  audit) echo '{\"auditReportVersion\":2,\"vulnerabilities\":{},\"metadata\":{\"vulnerabilities\":{\"total\":0}}}' ;;\n  outdated) echo '{}' ;;\nesac\n",
    );

    #[cfg(unix)]
    fn npm_repo(files: &[(&str, &str)]) -> tempfile::TempDir {
        let repo = crate::test_support::git_repo();
        for (rel, content) in files {
            crate::test_support::write(repo.path(), rel, content);
        }
        git_add_all(repo.path());
        repo
    }

    #[cfg(unix)]
    const PKG: &str = r#"{"name":"demo","version":"1.0.0"}"#;
    #[cfg(unix)]
    const LOCK: &str = r#"{"name":"demo","lockfileVersion":3}"#;

    #[cfg(unix)]
    #[test]
    fn a_clean_root_audit_alone_is_complete_with_matching_counts() {
        let repo = npm_repo(&[("package.json", PKG), ("package-lock.json", LOCK)]);
        let report = scan_with_stubs(repo.path(), &[NODE_STUB, CLEAN_NPM_STUB]);
        assert!(report.audit_complete, "{:?}", report.issues);
        assert_eq!(report.audit_coverage.found, 1);
        assert_eq!(report.audit_coverage.audited, 1);
    }

    #[cfg(unix)]
    #[test]
    fn a_nested_npm_lockfile_is_audited_or_blocks_completeness() {
        let repo = npm_repo(&[
            ("package.json", PKG),
            ("package-lock.json", LOCK),
            ("docs/package.json", PKG),
            ("docs/package-lock.json", LOCK),
        ]);
        let report = scan_with_stubs(repo.path(), &[NODE_STUB, CLEAN_NPM_STUB]);
        assert_eq!(
            report.audit_coverage.found, 2,
            "{:?}",
            report.audit_coverage
        );
        assert_eq!(report.audit_coverage.audited, 2);
        assert!(report.audit_complete, "{:?}", report.issues);

        // Without npm the nested lockfile cannot be audited and the audit
        // must not claim it was.
        let none = scan_with_stubs(repo.path(), &[NODE_STUB]);
        assert!(!none.audit_complete);
        assert_eq!(none.audit_coverage.found, 2);
        assert_eq!(none.audit_coverage.audited, 0);
    }

    #[cfg(unix)]
    #[test]
    fn a_workspace_member_uses_the_root_lockfile() {
        let repo = npm_repo(&[
            (
                "package.json",
                r#"{"name":"w","version":"1.0.0","workspaces":["packages/*"]}"#,
            ),
            ("package-lock.json", LOCK),
            ("packages/a/package.json", PKG),
        ]);
        let report = scan_with_stubs(repo.path(), &[NODE_STUB, CLEAN_NPM_STUB]);
        assert!(
            !report.issues.iter().any(|i| i.code == "missing_lockfile"),
            "{:?}",
            report.issues
        );
        let member = report
            .manifests
            .iter()
            .find(|m| m.path == "packages/a/package.json")
            .unwrap();
        assert_eq!(member.lockfile.as_deref(), Some("package-lock.json"));
        assert_eq!(member.lockfile_dir.as_deref(), Some(""));
        assert!(report.audit_complete, "{:?}", report.issues);
        assert_eq!(report.audit_coverage.found, 1);
    }

    #[cfg(unix)]
    #[test]
    fn a_lockfile_in_a_non_workspace_ancestor_does_not_cover_a_package() {
        let repo = npm_repo(&[
            ("package.json", PKG),
            ("package-lock.json", LOCK),
            ("tools/package.json", PKG),
        ]);
        let report = scan_with_stubs(repo.path(), &[NODE_STUB, CLEAN_NPM_STUB]);
        assert!(report.issues.iter().any(|i| i.code == "missing_lockfile"));
        assert!(!report.audit_complete);
    }

    #[cfg(unix)]
    #[test]
    fn python_lockfiles_and_pyproject_only_projects_are_not_audited() {
        for files in [
            vec![
                ("uv.lock", "version = 1\n"),
                ("pyproject.toml", "[project]\nname='x'\n"),
            ],
            vec![("poetry.lock", "# lock\n")],
            vec![("pyproject.toml", "[project]\nname='x'\n")],
        ] {
            let mut all = vec![("package.json", PKG), ("package-lock.json", LOCK)];
            all.extend(files);
            let repo = npm_repo(&all);
            let report = scan_with_stubs(repo.path(), &[NODE_STUB, CLEAN_NPM_STUB]);
            assert!(!report.audit_complete, "{:?}", report.audit_coverage);
            assert!(
                report
                    .audit_coverage
                    .unaudited
                    .iter()
                    .any(|u| u.starts_with("python:")),
                "{:?}",
                report.audit_coverage
            );
            assert!(report.audit_coverage.found > report.audit_coverage.audited);
        }
    }

    // -- one scan deadline ------------------------------------------------------

    /// Stubs for `scan_with_stubs` that also append their name to `log`, so a
    /// test can say exactly which auditors a scan started.
    #[cfg(unix)]
    fn logging_stubs(log: &Path, hang: &[&str]) -> Vec<(String, String)> {
        let log = log.display();
        vec![
            (
                "node".to_string(),
                "#!/bin/sh\nif [ \"$1\" = -v ]; then echo v20.0.0; fi\n".to_string(),
            ),
            (
                "npm".to_string(),
                format!(
                    "#!/bin/sh\ncase \"$1\" in\n  --version) echo 10.9.0 ;;\n  audit) echo npm-audit >> '{log}'\n{hang_npm}\n    echo '{{\"auditReportVersion\":2,\"vulnerabilities\":{{}},\"metadata\":{{\"vulnerabilities\":{{\"total\":0}}}}}}' ;;\n  outdated) echo npm-outdated >> '{log}'; echo '{{}}' ;;\nesac\n",
                    hang_npm = if hang.contains(&"npm") { "    /bin/sleep 30" } else { "" }
                ),
            ),
            (
                "cargo-audit".to_string(),
                format!(
                    "#!/bin/sh\ncase \"$1\" in\n  audit)\n    if [ \"$2\" = --version ]; then echo 0.21.0; exit 0; fi\n    echo cargo-audit >> '{log}'\n    echo '{{\"vulnerabilities\":{{\"found\":false,\"count\":0,\"list\":[]}}}}' ;;\n  *) echo 0.21.0 ;;\nesac\n"
                ),
            ),
        ]
    }

    #[cfg(unix)]
    fn scan_with_deadline(
        repo: &Path,
        stubs: &[(String, String)],
        deadline: crate::deadline::ScanDeadline,
    ) -> DepsHealthReport {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::TempDir::new().unwrap();
        for (name, body) in stubs {
            let path = dir.path().join(name);
            std::fs::write(&path, body).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let path_var =
            std::env::join_paths([dir.path(), Path::new("/usr/bin"), Path::new("/bin")]).unwrap();
        DepsScanner::scan_with_deadline(
            repo.to_str().unwrap(),
            ScanOptions {
                run_cli: true,
                path_var: Some(path_var),
                home: None,
            },
            deadline,
        )
        .expect("scan")
    }

    #[cfg(unix)]
    fn npm_and_cargo_repo() -> tempfile::TempDir {
        npm_repo(&[
            ("package.json", PKG),
            ("package-lock.json", LOCK),
            (
                "Cargo.toml",
                "[package]\nname = \"x\"\nversion = \"0.1.0\"\n",
            ),
            ("Cargo.lock", "version = 3\n"),
        ])
    }

    #[cfg(unix)]
    fn logged(log: &Path) -> Vec<String> {
        std::fs::read_to_string(log)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// R3-03: a scan whose deadline has passed starts no auditor, and says so
    /// instead of reading as a scan that found nothing.
    #[cfg(unix)]
    #[test]
    fn a_scan_past_its_deadline_starts_no_auditor_and_is_not_complete() {
        let repo = npm_and_cargo_repo();
        let logs = tempfile::TempDir::new().unwrap();
        let log = logs.path().join("ran");
        let report = scan_with_deadline(
            repo.path(),
            &logging_stubs(&log, &[]),
            crate::deadline::ScanDeadline::after(Duration::ZERO),
        );
        assert!(logged(&log).is_empty(), "started: {:?}", logged(&log));
        assert!(!report.audit_complete);
        assert!(report.truncated);
        assert!(report.scanners_ran.is_empty(), "{:?}", report.scanners_ran);
        assert!(
            report
                .limit_notices
                .iter()
                .any(|n| n.resource.contains("scan deadline")
                    && n.total > n.kept
                    && !n.inventory_only),
            "{:?}",
            report.limit_notices
        );
        assert!(
            report.issues.iter().any(|i| i.code == "scan_deadline"),
            "{:?}",
            report.issues
        );
    }

    /// R3-03: cut the scan after each possible number of stage checks. Every
    /// result is either the complete scan or one that says what it skipped, and
    /// the auditors that did start are always a prefix of the full run.
    #[cfg(unix)]
    #[test]
    fn a_deadline_between_any_two_stages_leaves_an_honest_report() {
        let repo = npm_and_cargo_repo();
        let logs = tempfile::TempDir::new().unwrap();
        let full_log = logs.path().join("full");
        let full = scan_with_deadline(
            repo.path(),
            &logging_stubs(&full_log, &[]),
            crate::deadline::ScanDeadline::audit(),
        );
        assert!(
            full.audit_complete,
            "a fast repo is complete: {:?}",
            full.issues
        );
        assert!(full.limit_notices.is_empty(), "{:?}", full.limit_notices);
        let full_order = logged(&full_log);
        assert!(full_order.len() >= 3, "{full_order:?}");

        let (mut saw_cut, mut saw_complete) = (false, false);
        for after in 0..8 {
            let log = logs.path().join(format!("cut{after}"));
            let _forced = crate::deadline::tests::ExpireAfterChecks::new(after);
            let report = scan_with_deadline(
                repo.path(),
                &logging_stubs(&log, &[]),
                crate::deadline::ScanDeadline::audit(),
            );
            drop(_forced);
            let started = logged(&log);
            assert!(
                full_order.starts_with(&started),
                "after {after}: {started:?} is not a prefix of {full_order:?}"
            );
            if report.audit_complete {
                saw_complete = true;
                assert_eq!(started, full_order, "after {after}");
            } else {
                saw_cut = true;
                assert!(report.truncated, "after {after}");
                assert!(
                    report.limit_notices.iter().any(|n| n.total > n.kept),
                    "after {after}: {:?}",
                    report.limit_notices
                );
            }
        }
        assert!(saw_cut && saw_complete, "the sweep must cross the boundary");
    }

    /// R3-03: an auditor still running at the deadline is cut at it — the
    /// command gets what is left of the deadline as its timeout — and nothing
    /// after it starts.
    #[cfg(unix)]
    #[test]
    fn a_hung_auditor_is_cut_at_the_deadline_and_the_next_one_never_starts() {
        let repo = npm_and_cargo_repo();
        let logs = tempfile::TempDir::new().unwrap();
        let log = logs.path().join("ran");
        let started = std::time::Instant::now();
        let report = scan_with_deadline(
            repo.path(),
            &logging_stubs(&log, &["npm"]),
            crate::deadline::ScanDeadline::after(Duration::from_secs(2)),
        );
        let elapsed = started.elapsed();
        assert!(
            // Relaxed at most 4x (28 s), still short of the 30 s auditor.
            elapsed < crate::test_support::coverage_relaxed(Duration::from_secs(7)),
            "a 30 s auditor under a 2 s deadline took {elapsed:?}"
        );
        let ran = logged(&log);
        assert!(ran.contains(&"npm-audit".to_string()), "{ran:?}");
        assert!(!ran.contains(&"cargo-audit".to_string()), "{ran:?}");
        assert!(!report.audit_complete);
        assert!(report.truncated);
        assert!(
            report.issues.iter().any(|i| i.code == "scan_deadline"),
            "{:?}",
            report.issues
        );
    }

    /// Zero artifacts: a repository with nothing to audit returns under an
    /// expired deadline without starting anything, and is not complete.
    #[cfg(unix)]
    #[test]
    fn an_expired_deadline_on_a_repo_with_nothing_to_audit_starts_nothing() {
        let repo = npm_repo(&[("README.md", "hello\n")]);
        let logs = tempfile::TempDir::new().unwrap();
        let log = logs.path().join("ran");
        let report = scan_with_deadline(
            repo.path(),
            &logging_stubs(&log, &[]),
            crate::deadline::ScanDeadline::after(Duration::ZERO),
        );
        assert!(logged(&log).is_empty());
        assert!(!report.audit_complete);
        assert!(report.manifests.is_empty() && report.vulnerabilities.is_empty());
    }

    /// A probe the deadline refused never ran, so it says nothing about
    /// whether the tool exists. The report must not tell the reader to install
    /// a tool that is installed, nor claim npm "was found but failed".
    #[cfg(unix)]
    #[test]
    fn a_probe_refused_at_the_deadline_is_not_run_rather_than_not_installed() {
        let repo = npm_and_cargo_repo();
        let logs = tempfile::TempDir::new().unwrap();
        let log = logs.path().join("ran");
        let report = scan_with_deadline(
            repo.path(),
            &logging_stubs(&log, &[]),
            crate::deadline::ScanDeadline::after(Duration::ZERO),
        );
        assert!(report.truncated);
        assert!(!report.ecosystems.is_empty());
        for hint in &report.ecosystems {
            assert!(
                !hint.note.contains("Install "),
                "{}: a scanner that was never probed is reported as missing: {}",
                hint.family,
                hint.note
            );
        }
        for issue in report.issues.iter().filter(|i| i.code == "npm_missing") {
            assert!(
                !issue.message.contains("not installed") && !issue.message.contains("was found"),
                "npm was never probed: {}",
                issue.message
            );
        }
        let env = ScanEnv {
            scan: Some(Arc::new(ScanClock::new(
                crate::deadline::ScanDeadline::after(Duration::ZERO),
            ))),
            ..ScanEnv::from_options(&ScanOptions::default())
        };
        assert!(matches!(
            probe_tool_version("npm", &["--version"], &env, VERSION_TIMEOUT),
            ToolProbe::NotRun(_)
        ));
    }

    /// A command that failed for its own reason is not "cut off at the
    /// deadline" just because the clock has passed by the time we look.
    #[test]
    fn a_command_failing_for_its_own_reason_is_not_labelled_a_deadline_cut() {
        let clock = ScanClock::new(crate::deadline::ScanDeadline::after(Duration::ZERO));
        clock.note_finished::<()>(
            &Err("npm audit produced no output".into()),
            "npm audit",
            false,
        );
        assert!(
            clock.state().not_run.is_empty(),
            "{:?}",
            clock.state().not_run
        );
        clock.note_finished::<()>(
            &Err("npm audit timed out after 1s".into()),
            "npm audit",
            true,
        );
        assert_eq!(clock.state().not_run.len(), 1);
    }

    /// A load deferral did not run and is not a timeout, so it is never a
    /// deadline cut; a slot-wait timeout under a clamped deadline is one.
    #[test]
    fn a_load_deferral_is_not_a_deadline_cut_but_a_slot_wait_timeout_is() {
        let deferred = "npm deferred under load after 0.000s: background work is shed";
        assert!(crate::engine::git_cli::is_deferred_under_load(deferred));
        let clock = ScanClock::new(crate::deadline::ScanDeadline::after(Duration::from_secs(
            60,
        )));
        clock.note_finished::<()>(&Err(deferred.into()), "npm audit", true);
        assert!(
            clock.state().not_run.is_empty(),
            "{:?}",
            clock.state().not_run
        );
        let slot = "npm timed out after 1.000s waiting for a process slot";
        assert!(crate::engine::git_cli::is_slot_wait_timeout(slot));
        clock.note_finished::<()>(&Err(slot.into()), "npm audit", true);
        assert_eq!(clock.state().not_run.len(), 1);
    }

    /// A step is complete only if nothing it ran was cut, even when the cut
    /// command carries a name an earlier step already recorded.
    #[test]
    fn a_step_that_was_cut_is_not_counted_complete_when_its_name_repeats() {
        let clock = ScanClock::new(crate::deadline::ScanDeadline::after(Duration::from_secs(
            60,
        )));
        clock.note_not_run("npm audit");
        clock.step("cargo audit", true, || clock.note_not_run("npm audit"));
        assert_eq!(clock.state().completed, 0);
        clock.step("pip-audit", true, || ());
        assert_eq!(clock.state().completed, 1);
    }

    #[test]
    fn a_family_with_no_scanner_is_found_but_never_audited() {
        let mut targets = ScanTargets::default();
        let mut other = BTreeMap::new();
        other.insert("jvm".to_string(), vec!["pom.xml".to_string()]);
        other.insert("native".to_string(), vec!["Makefile".to_string()]);
        collect_unaudited_targets(&[], &other, &mut targets);
        assert_eq!(
            targets.unaudited,
            vec![("jvm".to_string(), "pom.xml".to_string())]
        );
    }

    #[test]
    fn empty_cargo_crev_output_is_not_a_run() {
        assert!(cargo_supply::parse_cargo_crev_jsonl("").is_err());
        assert!(cargo_supply::parse_cargo_crev_jsonl("  \n\n").is_err());
        let ok = cargo_supply::parse_cargo_crev_jsonl(
            r#"{"name":"a","version":"1.0.0","status":"pass"}"#,
        );
        assert_eq!(ok.unwrap().verified, 1);
    }

    // -- outbound data: scrubbed environment, pinned config, refusal ---------------

    /// A stub `npm` that records its argv and its whole environment on `audit`.
    #[cfg(unix)]
    fn recording_npm(dump: &Path) -> String {
        format!(
            "#!/bin/sh\ncase \"$1\" in\n  --version) echo 10.9.0 ;;\n  audit) echo \"$@\" >> '{d}.argv'; env >> '{d}.env'; echo '{{\"auditReportVersion\":2,\"vulnerabilities\":{{}},\"metadata\":{{\"vulnerabilities\":{{\"total\":0}}}}}}' ;;\n  outdated) echo '{{}}' ;;\nesac\n",
            d = dump.display()
        )
    }

    #[cfg(unix)]
    fn node_stub() -> (&'static str, String) {
        (
            "node",
            "#!/bin/sh\nif [ \"$1\" = -v ]; then echo v20.0.0; fi\n".to_string(),
        )
    }

    #[cfg(unix)]
    fn scan_with_home(repo: &Path, scripts: &[(&str, String)], home: &Path) -> DepsHealthReport {
        use std::os::unix::fs::PermissionsExt;
        let stubs = tempfile::TempDir::new().unwrap();
        for (name, body) in scripts {
            let path = stubs.path().join(name);
            std::fs::write(&path, body).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let path_var =
            std::env::join_paths([stubs.path(), Path::new("/usr/bin"), Path::new("/bin")]).unwrap();
        DepsScanner::scan_with(
            repo.to_str().unwrap(),
            ScanOptions {
                run_cli: true,
                path_var: Some(path_var),
                home: Some(home.as_os_str().to_owned()),
            },
        )
        .expect("scan")
    }

    #[cfg(unix)]
    #[test]
    fn npm_scan_child_sees_no_tokens_and_a_pinned_config() {
        let serial = crate::test_support::env::env_serial();
        let _env = crate::test_support::env::bind_env(&serial)
            .set("NPM_TOKEN", "npm-secret-1")
            .set("NODE_AUTH_TOKEN", "node-secret-2")
            .set("GITHUB_TOKEN", "gh-secret-3")
            .set("AWS_SECRET_ACCESS_KEY", "aws-secret-4")
            .set("npm_config_registry", "https://env-chosen.invalid/")
            .set("HTTPS_PROXY", "http://user-proxy.invalid:3128")
            .remove("npm_config_userconfig")
            .remove("npm_config_globalconfig");
        let repo = npm_repo(&[("package.json", PKG), ("package-lock.json", LOCK)]);
        let home = tempfile::TempDir::new().unwrap();
        std::fs::write(
            home.path().join(".npmrc"),
            "registry=https://user-level.example/\n",
        )
        .unwrap();
        let dump = tempfile::TempDir::new().unwrap();
        let dump = dump.path().join("npm");
        let report = scan_with_home(
            repo.path(),
            &[node_stub(), ("npm", recording_npm(&dump))],
            home.path(),
        );
        assert!(report.audit_complete, "{:?}", report.issues);
        let env = std::fs::read_to_string(format!("{}.env", dump.display())).unwrap();
        for leaked in [
            "npm-secret-1",
            "node-secret-2",
            "gh-secret-3",
            "aws-secret-4",
        ] {
            assert!(
                !env.contains(leaked),
                "{leaked} reached the scanner:\n{env}"
            );
        }
        for name in ["NPM_TOKEN=", "NODE_AUTH_TOKEN=", "GITHUB_TOKEN=", "AWS_"] {
            assert!(!env.contains(name), "{name} reached the scanner:\n{env}");
        }
        // The user's own proxy survives; the environment's registry does not
        // outrank the pin.
        assert!(
            env.contains("HTTPS_PROXY=http://user-proxy.invalid:3128"),
            "{env}"
        );
        assert!(
            env.lines()
                .any(|l| l == "npm_config_registry=https://user-level.example/"),
            "{env}"
        );
        let userconfig = format!("npm_config_userconfig={}/.npmrc", home.path().display());
        assert!(env.lines().any(|l| l == userconfig), "{env}");
        assert!(
            env.lines()
                .any(|l| l.starts_with("npm_config_globalconfig=")),
            "{env}"
        );
    }

    #[cfg(unix)]
    fn registry_refusal_issues(report: &DepsHealthReport) -> Vec<&HealthIssue> {
        report
            .issues
            .iter()
            .filter(|i| i.code == "registry_overridden")
            .collect()
    }

    #[cfg(unix)]
    #[test]
    fn a_repo_npmrc_naming_a_registry_is_refused_and_no_request_is_made() {
        use std::io::ErrorKind;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let npmrc = format!(
            "registry=http://127.0.0.1:{port}/\n//127.0.0.1:{port}/:_authToken=${{NPM_TOKEN}}\n"
        );
        let repo = npm_repo(&[
            ("package.json", PKG),
            ("package-lock.json", LOCK),
            (".npmrc", &npmrc),
        ]);
        let home = tempfile::TempDir::new().unwrap();
        let dump = tempfile::TempDir::new().unwrap();
        let dump = dump.path().join("npm");
        let report = scan_with_home(
            repo.path(),
            &[node_stub(), ("npm", recording_npm(&dump))],
            home.path(),
        );
        let refusals = registry_refusal_issues(&report);
        assert_eq!(refusals.len(), 1, "{:?}", report.issues);
        assert_eq!(
            refusals[0].message,
            "registry overridden by repository config"
        );
        assert_eq!(refusals[0].severity, "error");
        assert!(
            !report.audit_complete,
            "a refused scan must not read as clean"
        );
        assert!(
            !Path::new(&format!("{}.argv", dump.display())).exists(),
            "npm audit ran despite the refusal"
        );
        match listener.accept() {
            Err(e) if e.kind() == ErrorKind::WouldBlock => {}
            other => panic!("the repo registry received a connection: {other:?}"),
        }
    }

    #[test]
    fn every_registry_credential_and_proxy_spelling_is_refused() {
        for body in [
            "registry=https://evil.example/\n",
            "@acme:registry=https://evil.example/\n",
            "//evil.example/:_authToken=${NPM_TOKEN}\n",
            "_auth=abc\n",
            "//evil.example/:_password=eHg=\n",
            "proxy=http://evil.example:8080\n",
            "https-proxy=http://evil.example:8080\n",
            "  Registry = https://evil.example/\n",
        ] {
            assert!(npmrc_overrides_network(body), "accepted: {body}");
        }
        for body in [
            "save-exact=true\n",
            "# registry=x\n",
            "; proxy=x\n",
            "engine-strict=true\n",
        ] {
            assert!(!npmrc_overrides_network(body), "refused: {body}");
        }
        assert!(bunfig_overrides_network(
            "[install]\nregistry = \"https://evil.example\"\n"
        ));
        assert!(bunfig_overrides_network(
            "[install.scopes]\nacme = { url = \"https://x\" }\n"
        ));
        assert!(bunfig_overrides_network(
            "[install]\nregistry = { url = \"u\", token = \"t\" }\n"
        ));
        assert!(!bunfig_overrides_network("[install]\nexact = true\n"));
    }

    #[cfg(unix)]
    #[test]
    fn a_workspace_root_npmrc_refuses_the_nested_package_and_a_plain_npmrc_does_not() {
        let nested = npm_repo(&[
            ("package.json", PKG),
            (".npmrc", "registry=https://evil.example/\n"),
            ("pkg/package.json", PKG),
            ("pkg/package-lock.json", LOCK),
        ]);
        let home = tempfile::TempDir::new().unwrap();
        let dump = tempfile::TempDir::new().unwrap();
        let dump = dump.path().join("npm");
        let report = scan_with_home(
            nested.path(),
            &[node_stub(), ("npm", recording_npm(&dump))],
            home.path(),
        );
        assert!(
            !registry_refusal_issues(&report).is_empty(),
            "{:?}",
            report.issues
        );
        assert!(!Path::new(&format!("{}.argv", dump.display())).exists());

        let plain = npm_repo(&[
            ("package.json", PKG),
            ("package-lock.json", LOCK),
            (".npmrc", "save-exact=true\n"),
        ]);
        let report = scan_with_home(
            plain.path(),
            &[node_stub(), ("npm", recording_npm(&dump))],
            home.path(),
        );
        assert!(
            registry_refusal_issues(&report).is_empty(),
            "{:?}",
            report.issues
        );
        assert!(report.audit_complete, "{:?}", report.issues);
    }

    #[cfg(unix)]
    #[test]
    fn a_repo_bunfig_naming_a_registry_is_refused_and_bun_does_not_run() {
        let repo = crate::test_support::git_repo();
        crate::test_support::write(
            repo.path(),
            "package.json",
            r#"{"name":"demo","version":"1.0.0","packageManager":"bun@1.4.2"}"#,
        );
        crate::test_support::write(repo.path(), "bun.lock", "{}\n");
        crate::test_support::write(
            repo.path(),
            "bunfig.toml",
            "[install]\nregistry = \"https://evil.example\"\n",
        );
        git_add_all(repo.path());
        let home = tempfile::TempDir::new().unwrap();
        let ran = home.path().join("bun-ran");
        let bun = format!(
            "#!/bin/sh\ncase \"$1\" in\n  --version) echo 1.4.2 ;;\n  audit) echo ran >> '{}'; echo '{{}}' ;;\nesac\n",
            ran.display()
        );
        let report = scan_with_home(
            repo.path(),
            &[
                node_stub(),
                ("npm", "#!/bin/sh\necho 10.9.0\n".to_string()),
                ("bun", bun),
            ],
            home.path(),
        );
        let refusals = registry_refusal_issues(&report);
        assert_eq!(refusals.len(), 1, "{:?}", report.issues);
        assert_eq!(
            refusals[0].message,
            "registry overridden by repository config"
        );
        assert!(!ran.exists(), "bun audit ran despite the refusal");
        assert!(!report.audit_complete);
    }

    #[cfg(unix)]
    #[test]
    fn only_variables_the_user_level_npmrc_references_reach_the_child() {
        let serial = crate::test_support::env::env_serial();
        let _env = crate::test_support::env::bind_env(&serial)
            .set("NPM_TOKEN", "user-token-1")
            .set("GITHUB_TOKEN", "gh-secret-3")
            .set("AWS_SECRET_ACCESS_KEY", "aws-secret-4")
            .remove("npm_config_userconfig")
            .remove("npm_config_globalconfig");
        let repo = npm_repo(&[("package.json", PKG), ("package-lock.json", LOCK)]);
        let home = tempfile::TempDir::new().unwrap();
        std::fs::write(
            home.path().join(".npmrc"),
            "; ${COMMENTED_OUT}\n//my.registry/:_authToken=${NPM_TOKEN}\n",
        )
        .unwrap();
        let dump = tempfile::TempDir::new().unwrap();
        let dump = dump.path().join("npm");
        let report = scan_with_home(
            repo.path(),
            &[node_stub(), ("npm", recording_npm(&dump))],
            home.path(),
        );
        assert!(report.audit_complete, "{:?}", report.issues);
        let env = std::fs::read_to_string(format!("{}.env", dump.display())).unwrap();
        assert!(env.lines().any(|l| l == "NPM_TOKEN=user-token-1"), "{env}");
        assert!(!env.contains("gh-secret-3"), "{env}");
        assert!(!env.contains("aws-secret-4"), "{env}");
    }

    #[cfg(unix)]
    #[test]
    fn a_repo_npmrc_cannot_widen_the_allowlist() {
        let serial = crate::test_support::env::env_serial();
        let _env = crate::test_support::env::bind_env(&serial)
            .set("GITHUB_TOKEN", "gh-secret-3")
            .remove("npm_config_userconfig")
            .remove("npm_config_globalconfig");
        let repo = npm_repo(&[
            ("package.json", PKG),
            ("package-lock.json", LOCK),
            (".npmrc", "//evil.example/:_authToken=${GITHUB_TOKEN}\n"),
        ]);
        let home = tempfile::TempDir::new().unwrap();
        let dump = tempfile::TempDir::new().unwrap();
        let dump = dump.path().join("npm");
        let report = scan_with_home(
            repo.path(),
            &[node_stub(), ("npm", recording_npm(&dump))],
            home.path(),
        );
        assert_eq!(
            registry_refusal_issues(&report).len(),
            1,
            "{:?}",
            report.issues
        );
        // The refused scan started no audit, so the token reached no audit child.
        assert!(!Path::new(&format!("{}.env", dump.display())).exists());
        // The referenced names come from the user's files only.
        let env_view = ScanEnv {
            path_var: None,
            path_source: PathSource::Inherited,
            home: Some(home.path().as_os_str().to_owned()),
            scan: None,
        };
        assert!(!user_config_referenced_vars(&env_view).contains(&"GITHUB_TOKEN".to_string()));
    }

    #[test]
    fn parse_npmrc_reads_keys_the_way_npm_does() {
        let keys =
            |text: &str| -> Vec<String> { parse_npmrc(text).into_iter().map(|(k, _)| k).collect() };
        assert_eq!(
            keys("\u{feff}Proxy=x\rFund=false\r\nca[]=y\n"),
            ["proxy", "fund", "ca"]
        );
        assert_eq!(
            keys("\"registry\"=x\n'Https-Proxy' = y\n"),
            ["registry", "https-proxy"]
        );
        assert_eq!(keys("; c=1\n# d=2\n[sec]\nbare\n"), ["bare"]);
    }

    #[test]
    fn bunfig_is_refused_on_any_mention_and_a_plain_one_is_not() {
        for text in [
            "[ install . scopes ]\n",
            "install.registry = \"x\"\n",
            "# registry was here\n",
            "[install]\nca = \"pem\"\n",
            "[install]\ncafile = \"p\"\n",
        ] {
            assert!(bunfig_overrides_network(text), "{text:?}");
        }
        assert!(!bunfig_overrides_network(
            "[install]\nexact = true\n[test]\nroot = \"src\"\n"
        ));
    }

    #[test]
    fn transport_trust_and_credential_keys_are_refused() {
        for text in [
            "strict-ssl=false\n",
            "//r.example/:certfile=/c\n",
            "//r.example/:keyfile=/k\n",
            "//r.example/:tokenHelper=/h\n",
            "//r.example/:email=a@b\n",
            "noproxy=x\n",
        ] {
            assert!(npmrc_overrides_network(text), "{text:?}");
        }
    }
}

#[cfg(test)]
#[path = "registry_refusal_adversarial_tests.rs"]
mod registry_refusal_adversarial_tests;
