//! The working-tree scan's choice of engine: the in-process scanner always,
//! Kingfisher as a second opinion only on an explicit scan, and a scan with
//! holes never clean.

use super::primary::{merge_second_opinion, scan_primary, scan_working_tree_with};
use super::run::failed_report;
use super::{ScanCompleteness, SecondOpinion, SecretLocation};
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
    stub_with(dir, "", "exit 0")
}

/// A stub `kingfisher` whose scan prints `findings` (JSON array elements, with
/// `$ROOT` standing for the scanned directory) and then runs `ending`.
fn stub_with(dir: &Path, findings: &str, ending: &str) -> PathBuf {
    let path = dir.join("kingfisher");
    let marker = dir.join("ran");
    let script = format!(
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 'kingfisher 9.9.9'; exit 0; fi\n\
         ROOT=\"$3\"\ntouch '{marker}'\ncat <<EOF\n\
         {{\"findings\":[{findings}],\"audit\":{{\"summary\":{{\"scan_partial\":0,\
         \"scan_failed\":0,\"pending\":0}},\"repositories\":[{{\"scan\":{{\"status\":\
         \"completed\"}}}}]}}}}\nEOF\n{ending}\n",
        marker = marker.display(),
    );
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// One Kingfisher finding at `$ROOT/<path>:<line>`, for [`stub_with`].
fn kf_row(path: &str, line: u32) -> String {
    format!(
        "{{\"rule\":{{\"id\":\"betterleaks.github-pat\",\"name\":\"github-pat\"}},\
         \"finding\":{{\"path\":\"$ROOT/{path}\",\"line\":{line},\"confidence\":\"high\",\
         \"snippet\":\"[REDACTED:aaaaaaaa]\"}}}}"
    )
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
    assert_eq!(report.second_opinion, SecondOpinion::NotInstalled);
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
fn an_automatic_scan_never_starts_kingfisher_but_reports_that_it_is_installed() {
    let dir = repo_with_plants();
    let bin = TempDir::new().unwrap();
    let kingfisher = stub(bin.path());
    let report = scan_working_tree_with(&path_of(&dir), false, || Some(kingfisher.clone()))
        .expect("scan runs");
    assert_eq!(report.engine, ENGINE);
    assert!(report.kingfisher_present);
    assert_eq!(report.second_opinion, SecondOpinion::NotRun);
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
fn an_explicit_scan_runs_kingfisher_after_the_in_process_scan_and_keeps_both() {
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
    assert_eq!(report.engine, ENGINE);
    assert_eq!(report.findings_total, 2, "the primary's findings stand");
    assert_eq!(report.completeness, ScanCompleteness::Complete);
    assert_eq!(report.kingfisher_version.as_deref(), Some("9.9.9"));
    assert_eq!(
        report.second_opinion,
        SecondOpinion::Ran {
            completeness: ScanCompleteness::Complete,
            version: Some("9.9.9".into()),
            findings_total: 0,
            confirmed: 0,
            added: 0,
        }
    );
}

#[test]
fn kingfisher_rows_are_the_union_and_a_shared_path_and_line_is_one_row() {
    let dir = repo_with_plants();
    write(dir.path(), ".gitignore", "local.env\n");
    write(dir.path(), "local.env", "x\n");
    let bin = TempDir::new().unwrap();
    // src/app.rs:1 and notes.txt:1 are the primary's; notes.txt:2 and the
    // ignored file are not.
    let rows = [
        kf_row("src/app.rs", 1),
        kf_row("notes.txt", 1),
        kf_row("notes.txt", 2),
        kf_row("local.env", 1),
    ]
    .join(",");
    let kingfisher = stub_with(bin.path(), &rows, "exit 200");
    let report = scan_working_tree_with(&path_of(&dir), true, || Some(kingfisher.clone()))
        .expect("scan runs");
    assert!(report.ok, "{report:?}");
    assert_eq!(report.findings_total, 4, "{report:?}");
    assert_eq!(report.findings.len(), 4, "{report:?}");
    match &report.second_opinion {
        SecondOpinion::Ran {
            findings_total,
            confirmed,
            added,
            ..
        } => assert_eq!((*findings_total, *confirmed, *added), (4, 2, 2)),
        other => panic!("{other:?}"),
    }
    let at = |p: &str, l: u32| {
        report
            .findings
            .iter()
            .filter(|f| f.path == p && f.line == Some(l))
            .count()
    };
    assert_eq!(at("src/app.rs", 1), 1, "kept once");
    assert_eq!(at("notes.txt", 1), 1, "kept once");
    assert_eq!(at("notes.txt", 2), 1);
    let ignored = report
        .findings
        .iter()
        .find(|f| f.path == "local.env")
        .unwrap();
    assert_eq!(ignored.location, SecretLocation::Ignored);
}

#[test]
fn a_failed_kingfisher_never_changes_the_primary_result() {
    let dir = repo_with_plants();
    let clean = scan_working_tree_with(&path_of(&dir), false, || None).expect("scan runs");
    // Exit 1 is not a success exit; a signal death and a missing envelope are
    // the other shapes of a Kingfisher that did not finish.
    for ending in ["exit 1", "kill -9 $$", "exit 0"] {
        let bin = TempDir::new().unwrap();
        let findings = if ending == "exit 0" { "not json" } else { "" };
        let kingfisher = stub_with(bin.path(), findings, ending);
        let report = scan_working_tree_with(&path_of(&dir), true, || Some(kingfisher.clone()))
            .expect("scan runs");
        assert!(bin.path().join("ran").exists(), "{ending}: stub never ran");
        assert!(report.ok, "{ending}: {report:?}");
        assert_eq!(report.error, None, "{ending}");
        assert_eq!(report.engine, ENGINE, "{ending}");
        assert_eq!(report.findings, clean.findings, "{ending}");
        assert_eq!(report.findings_total, clean.findings_total, "{ending}");
        assert_eq!(report.completeness, clean.completeness, "{ending}");
        assert!(
            matches!(&report.second_opinion, SecondOpinion::Unavailable { reason } if !reason.is_empty()),
            "{ending}: {:?}",
            report.second_opinion
        );
    }
}

#[test]
fn every_way_kingfisher_can_fail_leaves_the_primary_findings_alone() {
    let dir = repo_with_plants();
    let base = scan_working_tree_with(&path_of(&dir), false, || None).expect("scan runs");
    for reason in [
        "kingfisher did not finish within 300s",
        "kingfisher output was truncated",
        "kingfisher output could not be read to the end",
        "superseded by a newer secrets scan",
        "kingfisher exited with status 3",
    ] {
        let mut report = base.clone();
        merge_second_opinion(&mut report, failed_report(reason, Some("9.9.9".into())));
        assert_eq!(
            report.second_opinion,
            SecondOpinion::Unavailable {
                reason: reason.into()
            }
        );
        let mut expected = base.clone();
        expected.second_opinion = report.second_opinion.clone();
        assert_eq!(report, expected, "{reason}: more than the opinion changed");
    }
}

#[test]
fn an_explicit_scan_without_kingfisher_is_the_primary_result_and_not_installed() {
    let dir = repo_with_plants();
    let report = scan_working_tree_with(&path_of(&dir), true, || None).expect("scan runs");
    assert!(report.ok, "{report:?}");
    assert_eq!(report.engine, ENGINE);
    assert_eq!(report.findings_total, 2);
    assert_eq!(report.second_opinion, SecondOpinion::NotInstalled);
    assert!(!report.kingfisher_present);
}

#[test]
fn a_scan_that_left_a_nested_repository_unread_is_partial_and_names_it() {
    let dir = git_repo();
    write(dir.path(), "top.txt", "fine\n");
    let inner = dir.path().join("vendor/inner");
    std::fs::create_dir_all(&inner).unwrap();
    git_in(&inner, &["init", "-q", "-b", "main"]);
    write(&inner, "k.txt", &format!("{}\n", token()));
    let repo = std::fs::canonicalize(dir.path()).unwrap();
    let report = scan_primary(&repo, WorktreeLimits::DEFAULT, false);
    assert!(report.ok, "{report:?}");
    assert_eq!(report.findings_total, 0, "the nested file was not read");
    assert_eq!(report.skipped_total, 0, "it is named, not a skip");
    assert_eq!(report.excluded_paths, vec!["vendor/inner/".to_string()]);
    assert_eq!(
        report.completeness,
        ScanCompleteness::Partial,
        "a scan that did not read a nested repository is not complete"
    );
}
