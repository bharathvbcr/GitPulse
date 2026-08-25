//! Live verification of the multi-ecosystem scanners against the real CLIs.
//!
//! Each check is gated on the tool being reachable (via PATH or an explicit
//! `GITPULSE_TEST_*` override pointing at the binary). When a tool is absent
//! the check prints why and returns, so machines without it still pass; when
//! it is present, the assertions must hold against its genuine output.

use gitpulse_lib::analyzer::deps::{DepsScanner, ScanOptions};
use std::fs;
use std::path::{Path, PathBuf};
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

/// Prepends `bin_dir` to the process PATH so `capture_command` finds the
/// instrumented CLI first while keeping every normal lookup (git, node…)
/// working for tests running concurrently in this binary.
fn prepend_path(bin_dir: &Path) {
    let current = std::env::var("PATH").unwrap_or_default();
    let next = std::env::join_paths(
        std::iter::once(bin_dir.to_path_buf()).chain(std::env::split_paths(&current)),
    )
    .expect("join paths");
    std::env::set_var("PATH", next);
}

fn resolve_tool(env_key: &str, name: &str) -> Option<PathBuf> {
    if let Ok(path) = std::env::var(env_key) {
        let p = PathBuf::from(path);
        if p.is_file() {
            return Some(p);
        }
    }
    for dir in std::env::split_paths(&std::env::var("PATH").unwrap_or_default()) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Real `pip-audit` against a requirements file pinning flask==0.5, which
/// carries PYSEC-2019-179 / PYSEC-2018-66 per the PyPA advisory database.
#[test]
fn live_pip_audit_reports_flask_advisories() {
    let Some(bin) = resolve_tool("GITPULSE_TEST_PIP_AUDIT", "pip-audit") else {
        eprintln!("skip: pip-audit not on PATH and GITPULSE_TEST_PIP_AUDIT unset");
        return;
    };
    prepend_path(bin.parent().expect("bin dir"));

    let repo = git_repo();
    write(repo.path(), "requirements.txt", "flask==0.5\n");
    write(repo.path(), "app.py", "import flask\n");
    git_add(repo.path(), "requirements.txt");
    git_add(repo.path(), "app.py");

    let report =
        DepsScanner::scan_with(repo.path().to_str().unwrap(), ScanOptions { run_cli: true })
            .expect("scan");

    assert!(
        report.pip_audit_present,
        "scanner probe must find the real pip-audit"
    );
    assert!(
        !report.issues.iter().any(|i| i.code == "pip_audit_failed"),
        "live audit must not fail: {:?}",
        report.issues
    );
    let flask: Vec<_> = report
        .vulnerabilities
        .iter()
        .filter(|v| v.name == "flask" && v.ecosystem == "pypi")
        .collect();
    assert!(
        flask
            .iter()
            .any(|v| v.via.iter().any(|id| id == "PYSEC-2019-179")),
        "expected PYSEC-2019-179 among: {:?}",
        report.vulnerabilities
    );
    assert!(flask.iter().all(|v| v.is_direct));
    assert_eq!(
        report.audit.unknown as usize,
        flask.len(),
        "pypi findings are unranked and counted as such"
    );
}

/// Real `govulncheck` against a module pinning golang.org/x/text v0.3.5,
/// which carries GO-2021-0113 / GO-2022-1059 at module level.
#[test]
fn live_govulncheck_reports_xtext_module_findings() {
    let Some(bin) = resolve_tool("GITPULSE_TEST_GOVULNCHECK", "govulncheck") else {
        eprintln!("skip: govulncheck not on PATH and GITPULSE_TEST_GOVULNCHECK unset");
        return;
    };
    prepend_path(bin.parent().expect("bin dir"));

    let repo = git_repo();
    write(
        repo.path(),
        "go.mod",
        "module livetest\n\ngo 1.21\n\nrequire golang.org/x/text v0.3.5\n",
    );
    write(
        repo.path(),
        "main.go",
        "package main\n\nimport (\n\t\"fmt\"\n\t\"golang.org/x/text/language\"\n)\n\nfunc main() {\n\tt, _ := language.Parse(\"en\")\n\tfmt.Println(t)\n}\n",
    );
    // Resolve go.sum so the module graph builds without -mod overrides.
    let _ = Command::new("go")
        .args(["mod", "tidy"])
        .env("GOFLAGS", "-mod=mod")
        .current_dir(repo.path())
        .status();
    git_add(repo.path(), "go.mod");
    if repo.path().join("go.sum").exists() {
        git_add(repo.path(), "go.sum");
    }
    git_add(repo.path(), "main.go");

    let report =
        DepsScanner::scan_with(repo.path().to_str().unwrap(), ScanOptions { run_cli: true })
            .expect("scan");

    assert!(
        report.govulncheck_present,
        "scanner probe must find the real govulncheck"
    );
    assert!(
        !report.issues.iter().any(|i| i.code == "govulncheck_failed"),
        "live scan must not fail: {:?}",
        report.issues
    );
    let xtext: Vec<_> = report
        .vulnerabilities
        .iter()
        .filter(|v| v.name == "golang.org/x/text" && v.ecosystem == "go")
        .collect();
    assert!(
        xtext.iter().any(|v| v
            .via
            .iter()
            .any(|id| id == "GO-2021-0113" || id == "GO-2022-1059")),
        "expected GO-2021-0113/GO-2022-1059 among: {:?}",
        xtext
    );
}
