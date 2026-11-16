//! The working-tree scan's choice of engine: the in-process scanner by
//! default, Kingfisher only when asked, and a scan with holes never clean.

use super::primary::{scan_primary, scan_working_tree_with};
use super::{ScanCompleteness, SecretLocation};
use crate::leak_scan::worktree::WorktreeLimits;
use crate::leak_scan::ENGINE;
use crate::test_support::{git_in, git_repo, write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

/// Assembled at run time so this file carries no token a scanner would stop.
fn token() -> String {
    format!("ghp_{}", "aB3dE5gH7jK9mN1pQ3sT5vW7yZ9bC1dE3fG5")
}

fn path_of(dir: &TempDir) -> String {
    dir.path().to_str().expect("utf-8 temp path").to_string()
}

/// A stub `kingfisher` that records that it ran and reports a clean tree.
fn stub(dir: &Path) -> PathBuf {
    let path = dir.join("kingfisher");
    let marker = dir.join("ran");
    let script = format!(
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 'kingfisher 9.9.9'; exit 0; fi\n\
         touch '{}'\necho '{{\"findings\":[],\"audit\":{{\"summary\":{{\"scan_partial\":0,\
         \"scan_failed\":0,\"pending\":0}},\"repositories\":[{{\"scan\":{{\"status\":\
         \"completed\"}}}}]}}}}'\nexit 0\n",
        marker.display()
    );
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn repo_with_plants() -> TempDir {
    let dir = git_repo();
    write(
        dir.path(),
        "src/app.rs",
        &format!("let t = \"{}\";\n", token()),
    );
    git_in(dir.path(), &["add", "src/app.rs"]);
    write(dir.path(), "notes.txt", &format!("{}\n", token()));
    dir
}

#[test]
fn without_kingfisher_the_scan_still_completes_with_the_in_house_engine() {
    let dir = repo_with_plants();
    let report = scan_working_tree_with(&path_of(&dir), false, || None).expect("scan runs");
    assert!(report.ok, "{report:?}");
    assert_eq!(report.error, None);
    assert_eq!(report.engine, ENGINE);
    assert!(!report.kingfisher_present);
    assert_eq!(report.completeness, ScanCompleteness::Complete);
    assert_eq!(report.findings_total, 2);
    assert!(report.git_status_known);
    let location = |p: &str| {
        report
            .findings
            .iter()
            .find(|f| f.path == p)
            .unwrap()
            .location
    };
    assert_eq!(location("src/app.rs"), SecretLocation::Tracked);
    assert_eq!(location("notes.txt"), SecretLocation::Untracked);
    let wire = serde_json::to_string(&report).unwrap();
    assert!(
        !wire.contains(&token()) && !wire.contains(&token()[..12]),
        "{wire}"
    );
}

#[test]
fn the_default_scan_never_starts_kingfisher_but_reports_that_it_is_installed() {
    let dir = repo_with_plants();
    let bin = TempDir::new().unwrap();
    let kingfisher = stub(bin.path());
    let report = scan_working_tree_with(&path_of(&dir), false, || Some(kingfisher.clone()))
        .expect("scan runs");
    assert_eq!(report.engine, ENGINE);
    assert!(
        report.kingfisher_present,
        "the panel offers the deep scan on this"
    );
    assert!(!bin.path().join("ran").exists(), "the stub was started");
}

#[test]
fn a_scan_with_skips_is_partial_and_names_what_it_skipped() {
    let dir = git_repo();
    write(dir.path(), "big.txt", &"x".repeat(2_000));
    write(dir.path(), "ok.txt", "fine\n");
    let repo = std::fs::canonicalize(dir.path()).unwrap();
    let report = scan_primary(
        &repo,
        WorktreeLimits {
            file_bytes: 1_000,
            ..WorktreeLimits::DEFAULT
        },
        false,
    );
    assert!(report.ok, "{report:?}");
    assert_eq!(report.findings_total, 0);
    assert_eq!(report.completeness, ScanCompleteness::Partial);
    assert_eq!(report.skipped_total, 1);
    assert_eq!(report.skipped[0].path, "big.txt");
    assert_eq!(report.skipped[0].bytes, Some(2_000));
}

#[test]
fn a_listing_failure_is_a_failed_scan_not_a_clean_one() {
    // Not a repository: git cannot list it, so there is no verdict to give.
    let dir = TempDir::new().unwrap();
    let report = scan_primary(dir.path(), WorktreeLimits::DEFAULT, false);
    assert!(!report.ok, "{report:?}");
    assert!(report.error.is_some());
    assert_eq!(report.completeness, ScanCompleteness::Unverified);
    assert_eq!(report.engine, ENGINE);
}

#[test]
fn the_deep_scan_runs_kingfisher_when_asked() {
    let dir = repo_with_plants();
    let bin = TempDir::new().unwrap();
    let kingfisher = stub(bin.path());
    let report = scan_working_tree_with(&path_of(&dir), true, || Some(kingfisher.clone()))
        .expect("scan runs");
    assert!(
        bin.path().join("ran").exists(),
        "the stub was never started: {report:?}"
    );
    assert!(report.ok, "{report:?}");
    assert_eq!(report.engine, "kingfisher");
    assert_eq!(report.kingfisher_version.as_deref(), Some("9.9.9"));
    assert_eq!(report.skipped_total, 0);
}

#[test]
fn the_deep_scan_without_kingfisher_is_unavailable_not_a_fallback() {
    let dir = repo_with_plants();
    let report = scan_working_tree_with(&path_of(&dir), true, || None).expect("scan runs");
    assert!(!report.ok, "{report:?}");
    assert_eq!(report.engine, "kingfisher");
    assert!(!report.kingfisher_present);
}
