//! The coverage and dependency scans: the copy a cache hit pays, the
//! end-of-file read a cold scan pays, and the time a dependency scan takes
//! when one auditor hangs.
//!
//! Run with `cargo bench --manifest-path src-tauri/Cargo.toml --bench analyzer_scans`.
//!
//! * `coverage scan cache hit` and `file_coverage cache hit`: a repository
//!   whose merged hit maps hold 200k line entries (under the 250k cache cap),
//!   scanned again with unchanged artifacts. What the row times is the hit:
//!   fingerprinting the artifact plus whatever the hit copies out of the cache.
//! * `coverage cold scan, early hits in large sources`: 300 source files of
//!   256 KiB each, every one with hits only in its first 50 lines. Each sample
//!   rewrites the report (a new fingerprint, so a real re-parse) and times the
//!   scan, whose end-of-file check is the read of those sources.
//! * `deps scan, hung auditor`: `npm audit` sleeps 5 s. With no scan deadline
//!   the scan takes the 5 s; under a 1 s deadline it returns in about that
//!   second with the cut named. This row is the one that needs the deadline,
//!   so it exists only on builds that have one.

mod common;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use common::fixture::{git, trust_repo};
use common::{interleave, Case, Report, Sample};
use gitpulse_lib::analyzer::coverage::{CoverageScanner, ScanLimits};
use gitpulse_lib::analyzer::deps::{DepsScanner, ScanOptions};

const HIT_FILES: usize = 2_000;
const HIT_LINES: usize = 100;
const COLD_FILES: usize = 300;
const COLD_SOURCE_BYTES: usize = 256 * 1024;
const COLD_HIT_LINES: usize = 50;

fn rust_repo(root: &Path) -> PathBuf {
    let main = root.join("main");
    std::fs::create_dir_all(main.join("src")).expect("create fixture root");
    git(&main, &["init", "-q", "-b", "main"]);
    git(&main, &["config", "user.name", "Bench"]);
    git(&main, &["config", "user.email", "bench@example.com"]);
    git(&main, &["config", "commit.gpgsign", "false"]);
    std::fs::write(
        main.join(".git/info/exclude"),
        ".devcouncil/\n.devmap/\n.gitpulse/\n",
    )
    .expect("write info/exclude");
    std::fs::write(
        main.join("Cargo.toml"),
        "[package]\nname = \"bench\"\nversion = \"0.1.0\"\n",
    )
    .expect("write manifest");
    main
}

fn lcov(files: usize, hit_lines: usize, lead: &str) -> String {
    let mut text = String::from(lead);
    for file in 0..files {
        text.push_str(&format!("SF:src/f{file}.rs\n"));
        for line in 1..=hit_lines {
            text.push_str(&format!("DA:{line},{}\n", line % 3));
        }
        text.push_str("end_of_record\n");
    }
    text
}

/// 2k files x 100 hit lines, sources written before the report.
fn hit_repo(root: &Path) -> PathBuf {
    let main = rust_repo(root);
    for file in 0..HIT_FILES {
        std::fs::write(
            main.join(format!("src/f{file}.rs")),
            "x\n".repeat(HIT_LINES),
        )
        .expect("write source");
    }
    std::fs::write(main.join("lcov.info"), lcov(HIT_FILES, HIT_LINES, "")).expect("write report");
    git(&main, &["add", "-A"]);
    main
}

/// 300 sources of 256 KiB; hits only in the first 50 lines.
fn cold_repo(root: &Path) -> PathBuf {
    let main = rust_repo(root);
    for file in 0..COLD_FILES {
        std::fs::write(
            main.join(format!("src/f{file}.rs")),
            "x\n".repeat(COLD_SOURCE_BYTES / 2),
        )
        .expect("write source");
    }
    std::fs::write(
        main.join("lcov.info"),
        lcov(COLD_FILES, COLD_HIT_LINES, "TN:a\n"),
    )
    .expect("write report");
    git(&main, &["add", "-A"]);
    main
}

fn main() {
    let mut report = Report::new("analyzer_scans");
    let temp = tempfile::tempdir().expect("temp dir");

    let hit = hit_repo(&temp.path().join("hit"));
    trust_repo(&hit);
    let hit_path = hit.to_str().expect("UTF-8 path").to_string();
    // Warm the cache so every timed sample is a hit.
    let warm = CoverageScanner::scan_with_limits(&hit_path, ScanLimits::default())
        .expect("warm scan")
        .0;
    assert_eq!(warm.files.len(), HIT_FILES, "fixture must scan fully");
    assert!(!warm.truncated, "fixture must scan without a cap firing");

    let cold = cold_repo(&temp.path().join("cold"));
    trust_repo(&cold);
    let cold_path = cold.to_str().expect("UTF-8 path").to_string();

    let mut cases: Vec<Case> = Vec::new();
    {
        let path = hit_path.clone();
        cases.push(Case::new("coverage scan cache hit", move || {
            let (report, maps) = CoverageScanner::scan_with_limits(&path, ScanLimits::default())
                .expect("cache-hit scan");
            let files = report.files.len() as u64;
            std::hint::black_box(&maps);
            Sample::default().counter("files", files)
        }));
    }
    {
        let path = hit_path.clone();
        cases.push(Case::new("file_coverage cache hit", move || {
            let detail = CoverageScanner::file_coverage(&path, "src/f7.rs").expect("detail");
            let lines = detail.lines.len() as u64;
            std::hint::black_box(&detail);
            Sample::default().counter("lines", lines)
        }));
    }
    {
        let path = cold_path.clone();
        let report_path = cold.join("lcov.info");
        let mut turn = false;
        cases.push(Case::new(
            "coverage cold scan, early hits in large sources",
            move || {
                turn = !turn;
                std::fs::write(
                    &report_path,
                    lcov(
                        COLD_FILES,
                        COLD_HIT_LINES,
                        if turn { "TN:a\n" } else { "TN:b\n" },
                    ),
                )
                .expect("rewrite report");
                let started = Instant::now();
                let report = CoverageScanner::scan_with_limits(&path, ScanLimits::default())
                    .expect("cold scan")
                    .0;
                let elapsed = started.elapsed();
                let files = report.files.len() as u64;
                std::hint::black_box(&report);
                Sample::default().counter("files", files).elapsed(elapsed)
            },
        ));
    }
    report.note(format!(
        "coverage cache hit: {HIT_FILES} files x {HIT_LINES} hit lines = {} entries; cold scan: \
         {COLD_FILES} sources of {} KiB with hits in the first {COLD_HIT_LINES} lines, report \
         rewritten before each timed scan",
        HIT_FILES * HIT_LINES,
        COLD_SOURCE_BYTES / 1024
    ));

    let (deps_repo, stubs) = deps_fixture(temp.path());
    trust_repo(&deps_repo);
    let deps_path = deps_repo.to_str().expect("UTF-8 path").to_string();
    let path_var =
        std::env::join_paths([stubs.as_path(), Path::new("/usr/bin"), Path::new("/bin")])
            .expect("PATH");
    cases.push(Case::new("deps scan, hung auditor", move || {
        let options = ScanOptions {
            run_cli: true,
            path_var: Some(path_var.clone()),
            home: None,
        };
        let started = Instant::now();
        let report = DepsScanner::scan_within(&deps_path, options, Duration::from_secs(1))
            .expect("deps scan");
        let elapsed = started.elapsed();
        std::hint::black_box(&report);
        Sample::default()
            .counter("audit_complete", u64::from(report.audit_complete))
            .elapsed(elapsed)
    }));
    report.note(
        "deps scan: one npm root whose `npm audit` sleeps 5 s, scan deadline 1 s (builds without a \
         deadline have no such row; their time for the same fixture is the 5 s the auditor takes)"
            .to_string(),
    );

    let rows = interleave(cases, report.opts.samples(9), report.opts.warmup(1));
    report.extend(rows);
    report.finish();
}

fn deps_fixture(root: &Path) -> (PathBuf, PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let main = root.join("deps");
    std::fs::create_dir_all(&main).expect("create deps fixture");
    git(&main, &["init", "-q", "-b", "main"]);
    std::fs::write(
        main.join("package.json"),
        r#"{"name":"demo","version":"1.0.0"}"#,
    )
    .expect("write package.json");
    std::fs::write(
        main.join("package-lock.json"),
        r#"{"name":"demo","lockfileVersion":3}"#,
    )
    .expect("write lockfile");
    git(&main, &["add", "-A"]);
    let stubs = root.join("stubs");
    std::fs::create_dir_all(&stubs).expect("create stubs");
    for (name, body) in [
        (
            "node",
            "#!/bin/sh\nif [ \"$1\" = -v ]; then echo v20.0.0; fi\n",
        ),
        (
            "npm",
            "#!/bin/sh\ncase \"$1\" in\n  --version) echo 10.9.0 ;;\n  audit) /bin/sleep 5; echo '{\"auditReportVersion\":2,\"vulnerabilities\":{},\"metadata\":{\"vulnerabilities\":{\"total\":0}}}' ;;\n  outdated) echo '{}' ;;\nesac\n",
        ),
    ] {
        let path = stubs.join(name);
        std::fs::write(&path, body).expect("write stub");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("chmod stub");
    }
    (main, stubs)
}
