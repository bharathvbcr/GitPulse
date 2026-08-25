//! Adversarial filesystem scenarios for the coverage scanner: special files,
//! permission walls, symlink escapes, boundary sizes, workspace floods, and
//! concurrent hammering. Complements the parser-level corpus in
//! coverage_stress.rs by attacking what sits AROUND the parsers.

use gitpulse_lib::analyzer::coverage::{CoverageScanner, ScanLimits};
use std::fs;
use std::fs::Permissions;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
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

fn artifact_row<'a>(
    report: &'a gitpulse_lib::analyzer::coverage::CoverageReport,
    suffix: &str,
) -> &'a gitpulse_lib::analyzer::coverage::CoverageArtifact {
    report
        .artifacts
        .iter()
        .find(|a| a.path.ends_with(suffix))
        .unwrap_or_else(|| {
            panic!(
                "no artifact row ending with {suffix}: {:?}",
                report.artifacts
            )
        })
}

/// A named pipe posing as an artifact must be skipped with a reason, never
/// block the scan (open-with-no-writer would pin the thread forever).
#[test]
fn fifo_artifact_is_skipped_not_blocking() {
    let repo = git_repo();
    write(repo.path(), "src/lib.rs", "fn a() {}\n");
    let fifo = repo.path().join("lcov.info");
    let status = Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .expect("mkfifo available");
    assert!(status.success());

    let started = std::time::Instant::now();
    let report = CoverageScanner::scan(repo.path().to_str().unwrap()).expect("scan must finish");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(10),
        "scan blocked on FIFO for {:?}",
        started.elapsed()
    );
    let row = artifact_row(&report, "lcov.info");
    assert!(row.skipped, "fifo row: {row:?}");
    assert_eq!(row.skip_reason.as_deref(), Some("not a regular file"));
    assert!(report.files.is_empty());
}

/// A symlink planted exactly at a spec path pointing outside the repository
/// must surface as a skipped row carrying the escape reason.
#[test]
fn symlink_at_spec_path_is_reported_as_escape() {
    let repo = git_repo();
    write(repo.path(), "src/lib.rs", "fn a() {}\n");
    #[cfg(target_os = "macos")]
    let outside = "/etc/hosts";
    #[cfg(not(target_os = "macos"))]
    let outside = "/etc/hostname";
    std::os::unix::fs::symlink(outside, repo.path().join("lcov.info")).expect("symlink");

    let report = CoverageScanner::scan(repo.path().to_str().unwrap()).expect("scan");
    let row = artifact_row(&report, "lcov.info");
    assert!(row.skipped, "escape row: {row:?}");
    assert_eq!(
        row.skip_reason.as_deref(),
        Some("artifact path escaped the repository")
    );
    // /etc/hosts content must not leak into the report.
    assert!(report.files.is_empty());
    assert_eq!(report.overall.lines_found, 0);
}

/// An unreadable artifact must degrade to an explicit skip reason instead of
/// silently vanishing from the report.
#[test]
fn permission_denied_artifact_reports_reason() {
    if unsafe { libc_geteuid() } == 0 {
        eprintln!("running as root; permission test is meaningless, skipping");
        return;
    }
    let repo = git_repo();
    write(repo.path(), "src/lib.rs", "fn a() {}\n");
    write(
        repo.path(),
        "coverage/lcov.info",
        "SF:src/lib.rs\nDA:1,1\nend_of_record\n",
    );
    let artifact = repo.path().join("coverage/lcov.info");
    fs::set_permissions(&artifact, Permissions::from_mode(0o000)).expect("chmod");

    let report = CoverageScanner::scan(repo.path().to_str().unwrap()).expect("scan");
    fs::set_permissions(&artifact, Permissions::from_mode(0o644)).unwrap();

    let row = artifact_row(&report, "coverage/lcov.info");
    assert!(row.skipped, "denied row: {row:?}");
    assert_eq!(row.skip_reason.as_deref(), Some("permission denied"));
}

unsafe fn libc_geteuid() -> u32 {
    // Avoid a libc dependency: read euid from a fresh process.
    let output = Command::new("id").args(["-u"]).output().expect("id -u");
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .expect("uid numeric")
}

/// An artifact EXACTLY at the byte limit is readable; one byte over is not.
/// The off-by-one direction is part of the contract.
#[test]
fn artifact_size_boundary_is_inclusive() {
    let repo = git_repo();
    write(repo.path(), "src/lib.rs", "fn a() {}\n");

    const LIMIT: u64 = 512;
    let base = "SF:src/lib.rs\nDA:1,1\nend_of_record\n";
    let mut exact = base.to_string();
    // Pad with blank lines, then trim back to exactly LIMIT bytes — the
    // trailing blanks are inert to the lcov parser.
    while exact.len() < LIMIT as usize {
        exact.push('\n');
    }
    exact.truncate(LIMIT as usize);
    assert_eq!(exact.len(), LIMIT as usize);
    write(repo.path(), "lcov.info", &exact);
    assert_eq!(
        fs::metadata(repo.path().join("lcov.info")).unwrap().len(),
        LIMIT
    );

    let limits = ScanLimits {
        max_artifact_bytes: LIMIT,
        ..ScanLimits::default()
    };
    let (report, maps) =
        CoverageScanner::scan_with_limits(repo.path().to_str().unwrap(), limits).expect("scan");
    assert!(
        !artifact_row(&report, "lcov.info").skipped,
        "exact-limit artifact must be parsed"
    );
    assert!(maps.contains_key("src/lib.rs"));

    // One byte over the limit is skipped.
    write(repo.path(), "lcov.info", &format!("{exact}\n"));
    assert_eq!(
        fs::metadata(repo.path().join("lcov.info")).unwrap().len(),
        LIMIT + 1,
        "oversize fixture must actually exceed the limit"
    );
    let (report_over, _) =
        CoverageScanner::scan_with_limits(repo.path().to_str().unwrap(), limits).expect("scan");
    let row = artifact_row(&report_over, "lcov.info");
    assert!(row.skipped);
    assert!(row
        .skip_reason
        .as_deref()
        .unwrap_or_default()
        .contains("byte limit"));
}

/// Eight cargo workspaces in one repo, each holding its own llvm-cov output.
/// Nested-workspace discovery must find ALL of them even though the static
/// spec list grows with every workspace (cap starvation regression).
#[test]
fn monorepo_eight_workspaces_all_discovered() {
    let repo = git_repo();
    let mut expected_files = Vec::new();
    for w in 0..8usize {
        let ws = format!("services/crate_{w:02}");
        write(
            repo.path(),
            &format!("{ws}/Cargo.toml"),
            "[package]\nname = \"crate\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        );
        let src_rel = format!("{ws}/src/main.rs");
        write(repo.path(), &src_rel, "fn main() {}\n");
        write(
            repo.path(),
            &format!("{ws}/target/llvm-cov/lcov.info"),
            &format!("SF:{src_rel}\nDA:1,{}\nend_of_record\n", w % 2),
        );
        expected_files.push(src_rel);
    }
    let report = CoverageScanner::scan(repo.path().to_str().unwrap()).expect("scan");
    assert!(!report.truncated, "8 workspaces must fit the artifact cap");
    for rel in &expected_files {
        assert!(
            report.files.iter().any(|f| f.path == *rel),
            "{rel} missing from {:?}",
            report
                .files
                .iter()
                .map(|f| f.path.clone())
                .collect::<Vec<_>>()
        );
    }
    let parsed = report
        .artifacts
        .iter()
        .filter(|a| a.path.contains("/target/llvm-cov/lcov.info") && !a.skipped)
        .count();
    assert_eq!(parsed, 8, "all eight nested artifacts parsed");
}

/// Detail lookups fall back to ASCII-case-insensitive matching so covered
/// files stay visible when the requested path's case differs (APFS/NTFS).
#[test]
fn detail_lookup_matches_case_insensitively() {
    let repo = git_repo();
    write(repo.path(), "src/Lib.rs", "fn a() {}\n");
    write(
        repo.path(),
        "lcov.info",
        "SF:src/Lib.rs\nDA:1,1\nend_of_record\n",
    );
    let root = repo.path().to_str().unwrap();
    let detail = CoverageScanner::file_coverage(root, "SRC/lib.rs").expect("detail");
    assert_eq!(detail.totals.lines_hit, 1);
    assert_eq!(detail.lines.len(), 1);
    assert!(!detail.lines_truncated);
}

/// Parallel scans plus interleaved detail fetches over one shared repo must
/// all succeed and agree on totals (cache coherency under contention).
#[test]
fn concurrent_scans_and_details_agree() {
    let repo = git_repo();
    write(repo.path(), "src/lib.rs", "fn a() {}\nfn b() {}\n");
    write(repo.path(), "src/app.ts", "export const x = 1;\n");
    write(
        repo.path(),
        "coverage/lcov.info",
        "TN:\nSF:src/app.ts\nDA:1,1\nend_of_record\n",
    );
    write(
        repo.path(),
        "lcov.info",
        "SF:src/lib.rs\nDA:1,1\nDA:2,0\nend_of_record\n",
    );

    let root = repo.path().to_str().unwrap().to_string();
    let mut handles = Vec::new();
    for t in 0..12usize {
        let root = root.clone();
        handles.push(std::thread::spawn(move || {
            for i in 0..15 {
                if t % 3 == 0 && i % 2 == 0 {
                    let detail = CoverageScanner::file_coverage(&root, "src/lib.rs")
                        .expect("detail under contention");
                    assert_eq!(detail.totals.lines_found, 2);
                } else {
                    let report = CoverageScanner::scan(&root).expect("scan under contention");
                    assert_eq!(report.overall.lines_found, 3);
                    assert_eq!(report.overall.lines_hit, 2);
                    assert_eq!(report.languages.len(), 2);
                }
            }
        }));
    }
    for handle in handles {
        handle.join().expect("thread must not panic");
    }
}
