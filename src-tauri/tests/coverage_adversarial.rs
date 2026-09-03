//! Adversarial filesystem scenarios for the coverage scanner: special files,
//! permission walls, symlink escapes, boundary sizes, workspace floods, and
//! concurrent hammering. Complements the parser-level corpus in
//! coverage_stress.rs by attacking what sits AROUND the parsers.

use gitpulse_lib::analyzer::coverage::{CoverageScanner, ScanLimits};
use std::fs;
#[cfg(unix)]
use std::fs::Permissions;
#[cfg(unix)]
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
///
/// Unix only: Windows has no FIFO for the scanner to meet. Git for Windows
/// does ship an `mkfifo.exe`, so the command succeeds there and the test ran
/// on to assert about an artifact row that was never created -- a check that
/// could not run reporting as one that did.
#[cfg(unix)]
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
#[cfg(unix)]
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

// REGRESSION GUARD: suffix resolution used Path::is_file, which follows a
// repository symlink outside the checkout. An artifact could therefore make an
// external source path appear in the report even though source loading later
// refused it. Discovery and detail lookup must share canonical containment.
#[cfg(unix)]
#[test]
fn source_symlink_escape_never_enters_report_or_detail() {
    let outside = TempDir::new().unwrap();
    let secret = outside.path().join("secret.rs");
    fs::write(&secret, "pub const SECRET: &str = \"outside\";\n").unwrap();

    let repo = git_repo();
    fs::create_dir_all(repo.path().join("src")).unwrap();
    std::os::unix::fs::symlink(&secret, repo.path().join("src/leak.rs")).unwrap();
    write(
        repo.path(),
        "lcov.info",
        "SF:/build/agent/work/src/leak.rs\nDA:1,1\nend_of_record\n",
    );

    let report = CoverageScanner::scan(repo.path().to_str().unwrap()).expect("scan");
    assert!(report.files.iter().all(|file| file.path != "src/leak.rs"));
    assert!(CoverageScanner::file_coverage(repo.path().to_str().unwrap(), "src/leak.rs").is_err());
}

// REGRESSION GUARD: command planning read a root package.json through an
// outside symlink and could offer that external project's coverage script.
// Manifest discovery must use the same canonical containment as artifact and
// source discovery.
#[cfg(unix)]
#[test]
fn outside_package_manifest_cannot_shape_coverage_commands() {
    let outside = TempDir::new().unwrap();
    let package = outside.path().join("package.json");
    fs::write(&package, r#"{"scripts":{"coverage":"outside-command"}}"#).unwrap();

    let repo = git_repo();
    write(repo.path(), "src/app.ts", "export const app = true;\n");
    std::os::unix::fs::symlink(&package, repo.path().join("package.json")).unwrap();

    let report = CoverageScanner::scan(repo.path().to_str().unwrap()).expect("scan");
    let javascript = report
        .families
        .iter()
        .find(|family| family.family == "javascript")
        .expect("javascript family");
    assert!(!javascript
        .suggested_commands
        .iter()
        .any(|command| command == "npm run coverage"));
}

/// An unreadable artifact must degrade to an explicit skip reason instead of
/// silently vanishing from the report.
#[cfg(unix)]
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

#[cfg(unix)]
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

/// REGRESSION GUARD (audit M2, user-visible side): the generated-junk flood a
/// real Python repo produces (htmlcov full of .html pages) used to permanently
/// show "scan capped" because ANY over-64 probe directory flagged. Junk beyond
/// the window must not flag; real coverage must still be found.
#[test]
fn htmlcov_junk_flood_does_not_flag_scan_capped() {
    let repo = git_repo();
    write(repo.path(), "pkg/mod.py", "def f():\n    return 1\n");
    write(
        repo.path(),
        "coverage.xml",
        r#"<coverage><class filename="pkg/mod.py"><line number="1" hits="4"/><line number="2" hits="0"/></class></coverage>"#,
    );
    // 128 = 2 × MAX_DIR_ENTRIES; the const is private to the scanner.
    for i in 0..128usize {
        write(
            repo.path(),
            &format!("htmlcov/page_{i:03}.html"),
            "<html></html>\n",
        );
    }
    let report = CoverageScanner::scan(repo.path().to_str().unwrap()).expect("scan");
    assert!(
        !report.truncated,
        "junk-only overflow must not flag truncation"
    );
    let file = report
        .files
        .iter()
        .find(|f| f.path == "pkg/mod.py")
        .expect("python coverage must survive the junk flood");
    assert_eq!(file.lines_hit, 1);
}

/// Cache invalidation on DELETION: modification and addition were pinned by
/// other tests; removing an artifact must make it vanish from the next scan
/// instead of being served stale from the fingerprint cache (an empty
/// fingerprint — nothing present — must never count as a cache hit).
#[test]
fn deleted_artifact_disappears_from_rescan() {
    let repo = git_repo();
    write(repo.path(), "src/lib.rs", "fn a() {}\n");
    write(
        repo.path(),
        "lcov.info",
        "SF:src/lib.rs\nDA:1,1\nend_of_record\n",
    );
    let root = repo.path().to_str().unwrap();

    let first = CoverageScanner::scan(root).expect("first scan");
    assert!(first.files.iter().any(|f| f.path == "src/lib.rs"));

    fs::remove_file(repo.path().join("lcov.info")).expect("remove artifact");

    let second = CoverageScanner::scan(root).expect("rescan after delete");
    assert!(
        !second.artifacts.iter().any(|a| a.path == "lcov.info"),
        "deleted artifact must vanish entirely: {:?}",
        second.artifacts
    );
    assert!(second.files.is_empty());
    assert_eq!(second.overall.lines_found, 0);
}

/// Detail lookups must resolve non-normalized spellings of one and the same
/// file — Windows separators, "./"-prefixed, trailing slash — onto the
/// artifact's canonical hit map rather than reporting "no coverage".
/// (`normalize_rel_path` handles all three forms.)
#[test]
fn detail_lookup_accepts_non_normalized_path_forms() {
    let repo = git_repo();
    write(repo.path(), "src/lib.rs", "fn a() {}\nfn b() {}\n");
    write(
        repo.path(),
        "lcov.info",
        "SF:src/lib.rs\nDA:1,5\nDA:2,0\nend_of_record\n",
    );
    let root = repo.path().to_str().unwrap();
    let canonical = CoverageScanner::file_coverage(root, "src/lib.rs").expect("canonical detail");
    assert!(!canonical.lines.is_empty(), "fixture must have data");
    for form in ["src\\lib.rs", "./src/lib.rs", "src/lib.rs/"] {
        let detail = CoverageScanner::file_coverage(root, form)
            .unwrap_or_else(|e| panic!("form {form:?} rejected: {e}"));
        assert_eq!(detail.path, canonical.path, "{form}: canonical key");
        assert_eq!(detail.totals, canonical.totals, "{form}");
        assert_eq!(detail.lines, canonical.lines, "{form}");
    }
}

/// lcov SF paths recorded OUTSIDE the repository (a CI machine's build dir)
/// must map onto the existing repo file via longest-suffix fallback; when
/// several suffixes exist, the most specific (longest) EXISTING match wins,
/// deterministically. Pins both the fallback and its ambiguity tradeoff so
/// either regressing to "no match" or flipping resolution order screams.
#[test]
fn lcov_paths_outside_repo_map_via_deterministic_suffix_fallback() {
    // Foreign absolute prefix falls back to the single existing suffix match.
    let repo = git_repo();
    write(repo.path(), "src/lib.rs", "fn a() {}\n");
    write(
        repo.path(),
        "lcov.info",
        "SF:/ci/build/src/lib.rs\nDA:1,3\nend_of_record\n",
    );
    let report = CoverageScanner::scan(repo.path().to_str().unwrap()).expect("scan");
    let file = report
        .files
        .iter()
        .find(|f| f.path == "src/lib.rs")
        .expect("outside path must map onto the existing repo file");
    assert_eq!(file.lines_hit, 1);

    // Ambiguity: both `crates/src/lib.rs` and `src/lib.rs` exist; the
    // longest-suffix-first rule must pick `crates/src/lib.rs`, every run.
    let repo = git_repo();
    write(repo.path(), "src/lib.rs", "fn a() {}\n");
    write(repo.path(), "crates/src/lib.rs", "fn b() {}\n");
    write(
        repo.path(),
        "lcov.info",
        "SF:/opt/ci/crates/src/lib.rs\nDA:1,7\nend_of_record\n",
    );
    let report = CoverageScanner::scan(repo.path().to_str().unwrap()).expect("scan");
    assert!(
        report.files.iter().any(|f| f.path == "crates/src/lib.rs"),
        "longest existing suffix must win: {:?}",
        report
            .files
            .iter()
            .map(|f| f.path.clone())
            .collect::<Vec<_>>()
    );
    assert!(
        !report.files.iter().any(|f| f.path == "src/lib.rs"),
        "shorter suffix must not steal the match"
    );
}

/// Present-before-absent partitioning: static spec paths that don't exist on
/// disk must never consume the artifact cap, so a parseable artifact late in
/// the spec order survives even with max_artifacts = 1. The boundary is
/// pinned too: a PRESENT-but-skipped row does consume the cap.
#[test]
fn absent_spec_paths_do_not_starve_late_present_artifacts() {
    let repo = git_repo();
    write(repo.path(), "src/lib.rs", "fn a() {}\n");
    // Late in the rust spec table; every earlier spec stays absent.
    write(
        repo.path(),
        "target/llvm-cov/cobertura.xml",
        r#"<coverage><class filename="src/lib.rs"><line number="1" hits="2"/></class></coverage>"#,
    );
    let limits = ScanLimits {
        max_artifacts: 1,
        ..ScanLimits::default()
    };
    let root = repo.path().to_str().unwrap();

    let (report, _) = CoverageScanner::scan_with_limits(root, limits).expect("scan");
    let row = report
        .artifacts
        .iter()
        .find(|a| a.path == "target/llvm-cov/cobertura.xml")
        .expect("present artifact must be reached despite tiny cap");
    assert!(!row.skipped, "{row:?}");
    assert!(
        !report.truncated,
        "absent specs must not burn the cap: {:?}",
        report.artifacts
    );
    assert!(report.files.iter().any(|f| f.path == "src/lib.rs"));

    // Contrast: a PRESENT artifact that gets skipped (binary junk ahead of it
    // in spec order) DOES consume slot zero of a cap this small.
    write(
        repo.path(),
        "cobertura.xml",
        "<coverage>\0not really xml</coverage>",
    );
    let (report, _) = CoverageScanner::scan_with_limits(root, limits).expect("scan");
    let junk_row = report
        .artifacts
        .iter()
        .find(|a| a.path == "cobertura.xml")
        .expect("skipped row still reported");
    assert!(junk_row.skipped, "{junk_row:?}");
    assert!(
        !report
            .artifacts
            .iter()
            .any(|a| a.path == "target/llvm-cov/cobertura.xml" && !a.skipped),
        "present skipped rows consume the cap (documented boundary)"
    );
    assert!(report.truncated);
}
