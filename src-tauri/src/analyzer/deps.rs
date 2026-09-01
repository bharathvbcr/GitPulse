//! Dependency health for the opened repository.
//!
//! GitPulse does not install packages or apply fixes. It inventories manifests,
//! flags lockfile / engine / install-script issues locally, and — when the
//! matching CLI is on PATH — runs read-only audits: `npm audit` / `npm outdated`,
//! `cargo audit`, `pip-audit --no-deps` (pinned requirements files only),
//! `govulncheck -json`, `composer audit --locked`, and `bundler-audit check`.
//! A missing CLI is reported as such — and a CLI that exists but fails to run
//! (wrong interpreter, non-zero exit, timeout) is reported with its real cause,
//! never mislabeled as missing. It is never treated as a clean bill of
//! health. Scanner advisory databases are never refreshed implicitly — a stale
//! DB is the user's call, not a background write outside the repo.

use crate::analyzer::language::LanguageDetector;
use crate::engine::git_cli::{
    capture_command_with_env, git_text, resolve_spawn_program_with, sandbox_join,
    sandbox_join_canonical, validate_repo, CapturedOutput,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

const MAX_SOURCE_FILES: usize = 10_000;
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
const MAX_MANIFESTS: usize = 12;
const MAX_VULNS: usize = 200;
const MAX_OUTDATED: usize = 200;
const MAX_ISSUES: usize = 48;
const MAX_NPM_ROOTS: usize = 6;
const MAX_CARGO_LOCKS: usize = 6;
const MAX_PY_REQUIREMENTS: usize = 6;
const MAX_GO_MODS: usize = 4;
const MAX_COMPOSER_LOCKS: usize = 6;
const MAX_ECOSYSTEM_MANIFESTS: usize = 24;
/// pip-audit / govulncheck / composer findings carry no CVSS-style severity.
/// They are counted in their own bucket instead of masquerading as "info".
pub(crate) const SEVERITY_UNKNOWN: &str = "unknown";
const VERSION_TIMEOUT: Duration = Duration::from_secs(5);
const AUDIT_TIMEOUT: Duration = Duration::from_secs(90);

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
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DepsHealthReport {
    pub node_version: Option<String>,
    pub npm_version: Option<String>,
    pub npm_cli_present: bool,
    pub cargo_audit_present: bool,
    pub pip_audit_present: bool,
    pub govulncheck_present: bool,
    pub composer_present: bool,
    pub bundler_audit_present: bool,
    /// Scanners that issued at least one real CLI command during this scan
    /// (`npm`, `cargo`, `pip-audit`, `govulncheck`, `composer`,
    /// `bundler-audit`). The `*_present` flags say what COULD run; this says
    /// what DID. `serde(default)` keeps older serialized reports loadable.
    #[serde(default)]
    pub scanners_ran: Vec<String>,
    /// True only when every discovered, supported audit target was scanned
    /// successfully and no coverage-affecting safety cap was hit.
    #[serde(default)]
    pub audit_complete: bool,
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

/// Ecosystem artifacts collected while listing tracked files; the CLI enrichers
/// consume these so probes only run when something is actually there to scan.
#[derive(Debug, Default)]
struct ScanTargets {
    cargo_locks: Vec<String>,
    /// requirements*.txt files (pinned lists pip-audit can audit without pip).
    py_requirements: Vec<String>,
    go_mods: Vec<String>,
    composer_locks: Vec<String>,
    gemfile_locks: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ScanOptions {
    /// When false, skip npm audit / outdated and cargo audit. Local inventory
    /// still runs. Tests use this so a missing lockfile does not hit the registry.
    pub run_cli: bool,
    /// Test seam: when set, replaces the process `PATH` for every probe and
    /// spawn this scan performs, so a GUI-minimal launch environment can be
    /// simulated without mutating process state. `None` inherits the real
    /// environment (app behavior).
    pub path_var: Option<std::ffi::OsString>,
    /// Test seam: same contract as [`ScanOptions::path_var`] for `HOME`.
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
    home: Option<std::ffi::OsString>,
}

impl ScanEnv {
    /// Environment view implied by the options: an override wins wholesale,
    /// otherwise the process value is used as-is (`None` stays `None`).
    fn from_options(options: &ScanOptions) -> Self {
        Self {
            path_var: options
                .path_var
                .clone()
                .or_else(|| std::env::var_os("PATH")),
            home: options
                .home
                .clone()
                .or_else(|| std::env::var_os("HOME"))
                .or_else(|| std::env::var_os("USERPROFILE")),
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

    pub fn scan_with(repo_path: &str, options: ScanOptions) -> Result<DepsHealthReport, String> {
        let repo = validate_repo(repo_path)?;
        let env = ScanEnv::from_options(&options);
        let (mut report, targets) = local_scan(&repo, &env)?;
        if options.run_cli {
            // A scanner is recorded only once it actually dispatched a
            // command — presence flags alone never imply execution.
            let mut ran: Vec<String> = Vec::new();
            if enrich_npm(&repo, &mut report, &env) {
                ran.push("npm".into());
            }
            if enrich_cargo(&repo, &targets, &mut report, &env) {
                ran.push("cargo".into());
            }
            if enrich_python(&repo, &targets, &mut report, &env) {
                ran.push("pip-audit".into());
            }
            if enrich_go(&repo, &targets, &mut report, &env) {
                ran.push("govulncheck".into());
            }
            if enrich_php(&repo, &targets, &mut report, &env) {
                ran.push("composer".into());
            }
            if enrich_ruby(&repo, &targets, &mut report, &env) {
                ran.push("bundler-audit".into());
            }
            ran.sort();
            report.scanners_ran = ran;
        }
        report.audit = AuditSummary::from_vulns(&report.vulnerabilities);
        sort_report(&mut report);
        cap_report(&mut report);
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
        });
    }
    cap_scan_targets(&mut targets, &mut limit_notices);

    let node_probe = probe_tool_version(node_program(), &["-v"], env, VERSION_TIMEOUT);
    let npm_probe = probe_tool_version(npm_program(), &["--version"], env, VERSION_TIMEOUT);
    let node_version = node_probe.version().map(str::to_string);
    let npm_version = npm_probe.version().map(str::to_string);
    let npm_cli_present = npm_version.is_some();
    let cargo_audit_present = !targets.cargo_locks.is_empty() && cargo_audit_available(env);
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

    if !manifests.is_empty() && !npm_cli_present {
        push_issue(
            &mut issues,
            "warning",
            "npm_missing",
            npm_missing_message(&npm_probe, &node_probe, env),
            None,
        );
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
            });
            files.truncate(MAX_ECOSYSTEM_MANIFESTS);
        }
        let note = match family.as_str() {
            "cargo" => {
                if cargo_audit_present {
                    "Cargo.lock files will be checked with cargo audit".into()
                } else {
                    "Install cargo-audit (`cargo install cargo-audit`) to scan Rust crates".into()
                }
            }
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
            cargo_audit_present,
            pip_audit_present,
            govulncheck_present,
            composer_present,
            bundler_audit_present,
            scanners_ran: Vec::new(),
            audit_complete: false,
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

/// Records non-npm scan inputs spotted during the tracked-file walk.
fn collect_side_target(name: &str, rel: &str, targets: &mut ScanTargets) {
    if name == "Cargo.lock" {
        targets.cargo_locks.push(rel.to_string());
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

/// Runs `npm audit --json` / `npm outdated --json` at every scan root.
///
/// Returns true when at least one real npm command was dispatched — the
/// caller records that in `scanners_ran`. A root whose working directory
/// cannot be validated dispatches nothing and does not count.
fn enrich_npm(repo: &Path, report: &mut DepsHealthReport, env: &ScanEnv) -> bool {
    if !report.npm_cli_present || report.manifests.is_empty() {
        return false;
    }
    let mut roots = npm_scan_roots(&report.manifests);
    if roots.len() > MAX_NPM_ROOTS {
        record_limit(report, "npm audit roots", MAX_NPM_ROOTS, roots.len());
        roots.truncate(MAX_NPM_ROOTS);
    }
    let mut ran = false;
    for rel_dir in roots {
        let cwd = match npm_cwd(repo, &rel_dir) {
            Ok(p) => p,
            Err(msg) => {
                push_issue(&mut report.issues, "error", "audit_cwd", msg, Some(rel_dir));
                continue;
            }
        };
        match run_npm_json(&cwd, &["audit", "--json"], env) {
            Ok(text) => match parse_npm_audit_json(&text) {
                Ok((mut vulns, err)) => {
                    if let Some(err) = err {
                        push_issue(
                            &mut report.issues,
                            "error",
                            "audit_failed",
                            err,
                            Some(package_label(&rel_dir)),
                        );
                    }
                    report.vulnerabilities.append(&mut vulns);
                }
                Err(e) => push_issue(
                    &mut report.issues,
                    "error",
                    "audit_failed",
                    e,
                    Some(package_label(&rel_dir)),
                ),
            },
            Err(e) => push_issue(
                &mut report.issues,
                "error",
                "audit_failed",
                e,
                Some(package_label(&rel_dir)),
            ),
        }
        ran = true;
        match run_npm_json(&cwd, &["outdated", "--json"], env) {
            Ok(text) => match parse_npm_outdated_json(&text) {
                Ok(mut rows) => {
                    fill_dependency_types(
                        &mut rows,
                        &declared_dependency_sections(&cwd.join("package.json")),
                    );
                    report.outdated.append(&mut rows);
                }
                Err(e) => push_issue(
                    &mut report.issues,
                    "warning",
                    "outdated_failed",
                    e,
                    Some(package_label(&rel_dir)),
                ),
            },
            Err(e) => push_issue(
                &mut report.issues,
                "warning",
                "outdated_failed",
                e,
                Some(package_label(&rel_dir)),
            ),
        }
    }
    ran
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
            "cargo",
            &["audit", "--json", "--file", rel],
            Some(repo),
            AUDIT_TIMEOUT,
            &[("CARGO_TERM_COLOR", "never")],
        ) {
            Ok(out) => {
                ran = true;
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
                ran = true;
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
    ran
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
        ran = true;
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
            Ok(mut vulns) => report.vulnerabilities.append(&mut vulns),
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
            &[("GOFLAGS", "-mod=readonly"), ("GO111MODULE", "on")],
        );
        ran = true;
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
            Ok(mut vulns) => report.vulnerabilities.append(&mut vulns),
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
            &["audit", "--format=json", "--locked", "--no-interaction"],
            Some(&cwd),
            AUDIT_TIMEOUT,
            &[
                ("COMPOSER_NO_INTERACTION", "1"),
                ("COMPOSER_DISABLE_NETWORK", "0"),
            ],
        );
        ran = true;
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
            Ok(mut vulns) => report.vulnerabilities.append(&mut vulns),
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
        ran = true;
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
            Ok(mut vulns) => report.vulnerabilities.append(&mut vulns),
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
/// tool's stderr (or exit status when stderr is mute). Only a successful run
/// with empty output counts as zero findings — scanners like pip-audit always
/// emit at least `[]`, so emptiness there would itself be suspicious.
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
                if o.success {
                    return None;
                }
                let err = o.stderr_text();
                let message = if err.is_empty() {
                    format!("{program} exited {} with no output", o.status_code)
                } else {
                    err
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

fn npm_scan_roots(manifests: &[NpmManifest]) -> Vec<String> {
    let has_root = manifests.iter().any(|m| m.path == "package.json");
    if has_root {
        return vec![String::new()];
    }
    manifests
        .iter()
        .filter(|m| m.lockfile.is_some())
        .map(|m| dir_of(&m.path))
        .collect()
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
    let out = capture_scanner_command(
        env,
        npm_program(),
        args,
        Some(cwd),
        AUDIT_TIMEOUT,
        NPM_SAFE_ENV,
    )?;
    let stdout = out.stdout_text();
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        if out.success {
            return Ok("{}".into());
        }
        let err = out.stderr_text();
        if err.is_empty() {
            return Err(format!(
                "npm {} produced no output",
                args.first().unwrap_or(&"")
            ));
        }
        return Err(err);
    }
    Ok(trimmed.to_string())
}

/// What a CLI version probe concluded.
///
/// Three outcomes, deliberately distinct. Collapsing "ran but failed" into
/// "not found" is what made a GUI-launched scan report
/// "`npm is not installed`" while `/opt/homebrew/bin/npm` existed — its
/// shebang interpreter was missing, which is a completely different problem
/// with a different fix.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ToolProbe {
    /// Probe exited 0 and printed a non-empty first stdout line.
    Present(String),
    /// Nothing spawnpable resolved anywhere on PATH or in the fallback dirs;
    /// carries the raw spawn error text.
    NotFound(String),
    /// The binary was found and launched but could not report a version:
    /// non-zero exit (with stderr tail), timeout (naming the limit), or empty
    /// output. Carries the diagnosis.
    FoundButFailed(String),
}

impl ToolProbe {
    fn version(&self) -> Option<&str> {
        match self {
            ToolProbe::Present(version) => Some(version),
            _ => None,
        }
    }
}

/// Probes `program args` under `env`'s PATH/home view and classifies the run.
///
/// Every spawn goes through [`capture_scanner_command`], so probes resolve
/// exactly like production spawns: PATH first, then the GUI-launch fallback
/// dirs. The error-string classification mirrors `run_bounded`'s wording:
/// spawn failures start with `"Failed to spawn "`; timeouts read
/// `"{program} timed out after {n}s"` (the limit is already in the message).
fn probe_tool_version(program: &str, args: &[&str], env: &ScanEnv, timeout: Duration) -> ToolProbe {
    let out = capture_scanner_command(env, program, args, None, timeout, &[]);
    match out {
        Ok(out) => {
            let stdout = out.stdout_text();
            let line = stdout.lines().next().unwrap_or("").trim();
            if out.success && !line.is_empty() {
                ToolProbe::Present(line.to_string())
            } else {
                ToolProbe::FoundButFailed(found_but_failed_detail(program, &out))
            }
        }
        Err(e) => {
            if e.starts_with("Failed to spawn ") {
                ToolProbe::NotFound(e)
            } else {
                // Timeout ("… timed out after …s"), truncation cap, wait
                // failure: the program was there, the run went wrong.
                ToolProbe::FoundButFailed(e)
            }
        }
    }
}

/// Diagnosis for a finished run that yielded no usable version line.
///
/// Success is checked before stdout so a non-zero exit can never be promoted
/// to "present" by incidental output; a mute stderr falls back to naming the
/// exit status rather than leaving an unexplained failure.
fn found_but_failed_detail(program: &str, out: &CapturedOutput) -> String {
    if out.success {
        return format!("{program} produced no version output");
    }
    let err = out.stderr_text();
    if err.is_empty() {
        format!("{program} exited {} without a diagnosis", out.status_code)
    } else {
        err
    }
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
            let resolved =
                resolve_spawn_program_with(npm_program(), env.path(), env.home());
            let location = if resolved.contains('/') || resolved.contains('\\') {
                format!(" at {resolved}")
            } else {
                String::new()
            };
            format!(
                "npm was found{location} but could not be run: {detail}. Vulnerability and outdated checks did not run; local lockfile and engine checks still apply."
            )
        }
    };
    let node_note = match node_probe {
        ToolProbe::NotFound(_) => " The Node.js interpreter (`node`) was not found either — npm is a script that needs `node` to run, so install Node.js."
            .to_string(),
        _ => String::new(),
    };
    format!("{base}{node_note}")
}

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
    capture_command_with_env(
        program,
        args,
        cwd,
        timeout,
        extra_env,
        env.path(),
        env.home(),
    )
}

fn cargo_audit_available(env: &ScanEnv) -> bool {
    capture_scanner_command(
        env,
        "cargo",
        &["audit", "--version"],
        None,
        VERSION_TIMEOUT,
        &[("CARGO_TERM_COLOR", "never")],
    )
    .map(|o| o.success)
    .unwrap_or(false)
}

fn npm_program() -> &'static str {
    if cfg!(windows) {
        "npm.cmd"
    } else {
        "npm"
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
        })
    } else {
        None
    };
    Ok((out, limit))
}

fn read_npm_manifest(repo: &Path, rel: &str) -> Result<Option<NpmManifest>, String> {
    let dest = sandbox_join(repo, rel)?;
    let meta = match std::fs::symlink_metadata(&dest) {
        Ok(m) => m,
        Err(_) => return Ok(None),
    };
    if meta.is_dir() {
        return Ok(None);
    }
    let canon = dest
        .canonicalize()
        .map_err(|e| format!("Cannot read {rel}: {e}"))?;
    if !canon.starts_with(repo) {
        return Err(format!("{rel} escaped the repository"));
    }
    if meta.len() > MAX_MANIFEST_BYTES {
        return Err(format!(
            "{rel} exceeds {} byte limit ({})",
            MAX_MANIFEST_BYTES,
            meta.len()
        ));
    }
    let bytes = std::fs::read(&canon).map_err(|e| format!("Cannot read {rel}: {e}"))?;
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
    let lockfile = detect_lockfile(repo, &dir);
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
        let Ok(path) = sandbox_join(repo, &rel) else {
            continue;
        };
        if path.is_file() {
            return Some((*name).to_string());
        }
    }
    None
}

fn lockfile_to_manager(name: &str) -> &'static str {
    match name {
        "yarn.lock" => "yarn",
        "pnpm-lock.yaml" => "pnpm",
        "bun.lock" | "bun.lockb" => "bun",
        _ => "npm",
    }
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

fn audit_is_complete(report: &DepsHealthReport, targets: &ScanTargets, run_cli: bool) -> bool {
    if !run_cli || report.truncated {
        return false;
    }
    let ran = |name: &str| report.scanners_ran.iter().any(|scanner| scanner == name);
    let requirements = [
        (
            !report.manifests.is_empty(),
            report.npm_cli_present && ran("npm"),
        ),
        (
            !targets.cargo_locks.is_empty(),
            report.cargo_audit_present && ran("cargo"),
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
    let failure_codes = [
        "audit_cwd",
        "audit_failed",
        "cargo_audit_failed",
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
/// A manifest that cannot be read or parsed yields an empty map, leaving
/// `dep_type` blank exactly as it was before — degrading to "unknown", never
/// to a guess.
fn declared_dependency_sections(manifest_path: &Path) -> BTreeMap<String, String> {
    const SECTIONS: [&str; 4] = [
        "dependencies",
        "devDependencies",
        "optionalDependencies",
        "peerDependencies",
    ];
    let mut sections = BTreeMap::new();
    let Ok(text) = std::fs::read_to_string(manifest_path) else {
        return sections;
    };
    let Ok(value) = serde_json::from_str::<Value>(&text) else {
        return sections;
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
    sections
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
    let list = value
        .pointer("/vulnerabilities/list")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
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
            .unwrap_or_else(|| "high".into());
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

fn cvss_to_severity(score: &str) -> String {
    let n: f64 = score.parse().unwrap_or(0.0);
    if n >= 9.0 {
        "critical".into()
    } else if n >= 7.0 {
        "high".into()
    } else if n >= 4.0 {
        "moderate".into()
    } else if n > 0.0 {
        "low".into()
    } else {
        "high".into()
    }
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
        return Ok(Vec::new());
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

    let deserializer = serde_json::Deserializer::from_str(trimmed);
    for item in deserializer.into_iter::<Value>() {
        // A corrupt trailing segment (truncated run, interleaved writer)
        // skips rather than poisoning everything before it.
        let Ok(value) = item else {
            continue;
        };
        if !value.is_object() {
            continue;
        }
        parsed_any = true;
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

/// Conservative engines.node check: flag only when the running Node major
/// satisfies NONE of the spec's `||` alternatives. Each alternative is an AND
/// of its comparators; unparsable fragments are skipped rather than allowed
/// to discard their neighbours, and unparseable specs produce no warning.
pub(crate) fn engines_mismatch(spec: &str, node_version: &str) -> Option<String> {
    let current = parse_node_major(node_version)?;
    let spec = spec.trim();
    if spec.is_empty() || spec == "*" {
        return None;
    }
    let mut parsed_any = false;
    let mut satisfied_any = false;
    for clause in spec.split("||") {
        let Some((min_major, max_exclusive)) = parse_engine_bounds(clause) else {
            continue;
        };
        parsed_any = true;
        let above_min = min_major.is_none_or(|min| current >= min);
        let below_max = max_exclusive.is_none_or(|max| current < max);
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

pub(crate) fn parse_node_major(version: &str) -> Option<u32> {
    let s = version.trim().trim_start_matches('v');
    let digits: String = s.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

/// Parses ONE `||`-free clause into inclusive-major bounds. Comparators AND
/// together; a hyphen range (`16.4.0 - 18.2.0`, spaced or glued) is the
/// interval [A, B] with B inclusive; unknown fragments are skipped instead
/// of discarding the bounds already parsed.
fn parse_engine_bounds(clause: &str) -> Option<(Option<u32>, Option<u32>)> {
    let clause = clause.trim();
    if clause.is_empty() || clause == "*" {
        return None;
    }
    let mut min: Option<u32> = None;
    let mut max_ex: Option<u32> = None;

    // Split glued hyphen ranges first so token iteration stays uniform.
    let mut tokens: Vec<String> = Vec::new();
    for raw in clause.split_whitespace() {
        let mut piece = raw.trim().to_string();
        if !piece.contains(' ') && !piece.contains('-') {
            tokens.push(piece);
            continue;
        }
        // Hyphen ranges carry a '-' between two version-looking operands.
        if let Some(idx) = find_hyphen_range_split(&piece) {
            let a = piece[..idx].to_string();
            let b = piece[idx + 1..].to_string();
            tokens.push(a);
            tokens.push("-".to_string());
            piece = b;
        }
        tokens.push(piece);
    }

    let mut expect_upper = false;
    for token in &tokens {
        let token = token.trim().trim_matches(',');
        if token.is_empty() || token == "||" {
            continue;
        }
        if token == "-" {
            expect_upper = true;
            continue;
        }
        if let Some(rest) = token.strip_prefix(">=") {
            min = Some(leading_major(rest).unwrap_or(min.unwrap_or(0)));
        } else if let Some(rest) = token.strip_prefix('>') {
            if let Some(n) = leading_major(rest) {
                min = Some(n.saturating_add(1));
            }
        } else if let Some(rest) = token.strip_prefix("<=") {
            if let Some(n) = leading_major(rest) {
                max_ex = Some(n.saturating_add(1));
            }
        } else if let Some(rest) = token.strip_prefix('<') {
            if let Some(n) = leading_major(rest) {
                max_ex = Some(n);
            }
        } else if let Some(rest) = token.strip_prefix('^').or_else(|| token.strip_prefix('~')) {
            let m = leading_major(rest);
            min = m;
            if let Some(n) = m {
                max_ex = Some(n.saturating_add(1));
            }
        } else if expect_upper {
            // Upper bound of a hyphen range: inclusive, hence +1 exclusive.
            if let Some(n) = leading_major(token) {
                max_ex = Some(n.saturating_add(1));
            }
            expect_upper = false;
        } else if token.chars().next().is_some_and(|c| c.is_ascii_digit()) {
            min = leading_major(token);
        }
    }
    if min.is_none() && max_ex.is_none() {
        None
    } else {
        Some((min, max_ex))
    }
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

fn leading_major(s: &str) -> Option<u32> {
    let s = s.trim().trim_start_matches('v');
    let digits: String = s.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;
    use tempfile::TempDir;

    fn git_repo() -> TempDir {
        let dir = TempDir::new().expect("tempdir");
        let status = Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(dir.path())
            .status()
            .expect("git init");
        assert!(status.success());
        dir
    }

    fn write(dir: &Path, rel: &str, content: &str) {
        let dest = dir.join(rel);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(dest, content).unwrap();
    }

    fn git_add(dir: &Path, rel: &str) {
        let status = Command::new("git")
            .args(["add", rel])
            .current_dir(dir)
            .status()
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
        let sections = declared_dependency_sections(&dir.path().join("package.json"));
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
        assert!(declared_dependency_sections(&dir.path().join("package.json")).is_empty());
        std::fs::write(dir.path().join("package.json"), "{ not json").expect("write");
        assert!(declared_dependency_sections(&dir.path().join("package.json")).is_empty());
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
        assert_eq!(parse_node_major("v20.11.1"), Some(20));
    }

    /// Regression (audit M8): a hyphen range used to overwrite its own
    /// minimum with the upper bound, flagging Node 17 against `16 - 18`.
    #[test]
    fn engines_hyphen_range_is_treated_as_inclusive_interval() {
        assert!(engines_mismatch("16.4.0 - 18.2.0", "v17.9.0").is_none());
        assert!(engines_mismatch("16.4.0 - 18.2.0", "v18.2.99").is_none());
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
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
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
not-json-at-all
"#;

    #[test]
    fn parse_govulncheck_keeps_module_level_and_dedupes() {
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
        assert!(parse_govulncheck_stream("").unwrap().is_empty());
        assert!(parse_govulncheck_stream("   \n \n").unwrap().is_empty());
        assert!(parse_govulncheck_stream("garbage\nmore garbage").is_err());
    }

    #[test]
    fn parse_govulncheck_survives_truncated_stream_mid_line() {
        let truncated =
            "{\"config\":{\"protocol_version\":\"v1.0.0\"}}\n{\"finding\":{\"osv\":\"GO-202";
        let vulns = parse_govulncheck_stream(truncated).unwrap();
        assert!(
            vulns.is_empty(),
            "partial finding line is skipped: {vulns:?}"
        );
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

    use crate::engine::git_cli::CapturedOutput;

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
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n",
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

        assert_eq!(report.scanners_ran, vec!["npm".to_string()]);
        assert!(!report.audit_complete);
        assert!(report
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
            home: None,
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
            home: None,
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
            home: None,
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
            home: None,
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
        git_add(repo.path(), "package.json");

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
