//! Adversarial and stress coverage for the language-aware coverage scanner.
//!
//! Regression tests below are marked REGRESSION GUARD: each encodes a bug
//! found in audit that has since been fixed and now asserts the fixed
//! behavior, failing loudly if it reappears.

use gitpulse_lib::analyzer::coverage::{CoverageScanner, ScanLimits};
use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::Instant;
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

fn scan(dir: &Path) -> gitpulse_lib::analyzer::coverage::CoverageReport {
    CoverageScanner::scan(dir.to_str().unwrap()).expect("scan should not error")
}

fn entries_for(report: &gitpulse_lib::analyzer::coverage::CoverageReport, path: &str) -> usize {
    report
        .files
        .iter()
        .find(|f| f.path == path)
        .map(|f| f.lines_found)
        .unwrap_or(0)
}

// REGRESSION GUARD: lcov `DA:<line>,<hits>[,<checksum>]` records were silently
// dropped by the pre-fix parser because the optional third field made the hits
// value unparseable; records must now parse with the checksum ignored.
#[test]
fn regression_lcov_checksum_records_are_kept() {
    let repo = git_repo();
    write(repo.path(), "src/lib.rs", "fn a() {}\nfn b() {}\n");
    write(
        repo.path(),
        "lcov.info",
        "SF:src/lib.rs\nDA:1,1,aB3xYz\nDA:2,0,deadbeef\nend_of_record\n",
    );
    let report = scan(repo.path());
    assert_eq!(entries_for(&report, "src/lib.rs"), 2);
    let detail =
        CoverageScanner::file_coverage(repo.path().to_str().unwrap(), "src/lib.rs").unwrap();
    assert_eq!(detail.lines.len(), 2);
    assert_eq!(detail.lines[0].line_no, 1);
    assert_eq!(detail.lines[0].hits, 1);
    assert_eq!(detail.lines[1].hits, 0);
}

// REGRESSION GUARD: a few KB of go-cover text once expanded into millions of
// BTreeMap entries because block ranges had no entry budget; expansion must
// stay bounded.
#[test]
fn regression_go_cover_expansion_is_bounded() {
    let repo = git_repo();
    write(repo.path(), "src/main.go", "package main\n");
    let mut text = String::from("mode: set\n");
    for i in 0..400usize {
        let start = i * 10_000 + 1;
        text.push_str(&format!("pkg/big.go:{}.1,{}.9 0 1\n", start, start + 9_999));
    }
    assert!(text.len() < 64 * 1024, "payload must stay tiny");
    write(repo.path(), "coverage.out", &text);

    let started = Instant::now();
    let report = scan(repo.path());
    let elapsed = started.elapsed();
    assert!(
        elapsed.as_secs() < 15,
        "scan took {:?}, expansion must be bounded",
        elapsed
    );
    let recorded = entries_for(&report, "pkg/big.go");
    assert!(
        recorded <= 200_000,
        "per-file entries {} exceeded sane budget",
        recorded
    );
}

// REGRESSION GUARD: the same artifact path was once read and parsed once per
// interested family (e.g. javascript + native both claim coverage/lcov.info);
// deduplication must report it exactly once.
#[test]
fn regression_shared_artifact_is_reported_once() {
    let repo = git_repo();
    write(repo.path(), "src/app.ts", "export const x = 1;\n");
    write(repo.path(), "src/util.c", "int util(void) { return 1; }\n");
    write(
        repo.path(),
        "coverage/lcov.info",
        "SF:src/app.ts\nDA:1,1\nend_of_record\nSF:src/util.c\nDA:1,0\nend_of_record\n",
    );
    let report = scan(repo.path());
    let dupes = report
        .artifacts
        .iter()
        .filter(|a| a.path == "coverage/lcov.info")
        .count();
    assert_eq!(dupes, 1, "artifact must be parsed exactly once");
    assert_eq!(entries_for(&report, "src/app.ts"), 1);
    assert_eq!(entries_for(&report, "src/util.c"), 1);
}

#[test]
fn malformed_lcov_corpus_does_not_panic() {
    let repo = git_repo();
    write(repo.path(), "src/lib.rs", "fn a() {}\n");
    let repeated = "DA:1,1\n".repeat(100_000);
    let corpus: Vec<&str> = vec![
        "",
        "mode: set\n",
        "\u{feff}SF:src/lib.rs\r\nDA:1,1\r\nend_of_record\r\n",
        "SF:src/lib.rs\nDA:999999999,1\nDA:-5,-1\nDA:x,y\nend_of_record\n",
        "SF:\nDA:1,1\nend_of_record\nSF:src/lib.rs\nDA:1,7\nend_of_record\nSF:src/lib.rs\nDA:1,0\nend_of_record\n",
        &repeated,
    ];
    for (i, body) in corpus.iter().enumerate() {
        write(repo.path(), "lcov.info", body);
        let report = scan(repo.path());
        if i != 4 {
            continue;
        }
        assert_eq!(
            entries_for(&report, "src/lib.rs"),
            1,
            "conflicting duplicate SF merges by max"
        );
    }
}

#[test]
fn malformed_xml_corpus_does_not_panic() {
    let repo = git_repo();
    write(repo.path(), "pkg/mod.py", "def f():\n    return 1\n");
    let corpus: &[&str] = &[
        "<coverage><class filename=\"pkg/mod.py\"><line number",
        "<?xml version=\"1.0\"?><coverage></coverage>",
        "<report><package name='p'><sourcefile name='mod.py'><line nr='1' ci='0' mi='4'/></sourcefile></package></report>",
        "<coverage><file name='m.php' path='m.php'><line num='1' count='2'/></file></coverage>",
    ];
    for body in corpus {
        write(repo.path(), "coverage.xml", body);
        let _ = scan(repo.path());
    }
}

#[test]
fn malformed_istanbul_corpus_does_not_panic() {
    let repo = git_repo();
    write(repo.path(), "src/app.ts", "export const x = 1;\n");
    let corpus: &[&str] = &[
        "[]",
        "\"scalar\"",
        "{ \"src/app.ts\": { \"path\": \"src/app.ts\", \"l\": null } }",
        "{ \"src/app.ts\": { \"s\": {}, \"statementMap\": {} } }",
        "{ \"src/app.ts\": { \"l\": { \"1\": 18446744073709551615 } } }",
        "{ \"src/app.ts\": { \"l\": { \"not-a-line\": 3 } } }",
    ];
    for body in corpus {
        write(repo.path(), "coverage/coverage-final.json", body);
        let _ = scan(repo.path());
    }
}

#[test]
fn malformed_go_cover_corpus_does_not_panic() {
    let repo = git_repo();
    write(repo.path(), "src/main.go", "package main\n");
    let corpus: &[&str] = &[
        "mode: count\n",
        "garbage line without colons\n",
        "src/main.go:1.1,2.1\n",
        "src/main.go:1.1,2.1 x y\n",
        "C:\\weird\\main.go:1.1,2.1 1 1\n",
        "src/main.go:0.0,0.0 0 -1\n",
        "src/main.go:99999999.1,99999999.2 1 1\n",
    ];
    for body in corpus {
        write(repo.path(), "coverage.out", body);
        let _ = scan(repo.path());
    }
}

#[test]
fn binary_and_unicode_artifacts_behave_sensibly() {
    let repo = git_repo();
    write(repo.path(), "src/lib.rs", "fn a() {}\n");
    write(repo.path(), "lcov.info", "SF:src/li\x00b.rs\nDA:1,\u{0}\n");
    let report = scan(repo.path());
    assert!(report.artifacts.iter().all(|a| a.skipped));

    write(repo.path(), "src/héllo wörld.rs", "pub fn ünicode() {}\n");
    write(
        repo.path(),
        "lcov.info",
        "SF:src/héllo wörld.rs\nDA:1,1\nend_of_record\n",
    );
    let report = scan(repo.path());
    assert!(report.files.iter().any(|f| f.path.contains("héllo wörld")));
}

#[test]
fn sandbox_escape_paths_never_surface() {
    let outside = TempDir::new().unwrap();
    let secret = outside.path().join("secret.txt");
    fs::write(&secret, "top secret\n").unwrap();

    let repo = git_repo();
    write(repo.path(), "src/lib.rs", "fn a() {}\n");

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&secret, repo.path().join("leak.info")).unwrap();
        write(
            repo.path(),
            "coverage/lcov.info",
            "SF:../outside-secret.txt\nDA:1,1\nend_of_record\nSF:/etc/passwd\nDA:2,2\nend_of_record\n",
        );
    }
    #[cfg(not(unix))]
    {
        write(
            repo.path(),
            "coverage/lcov.info",
            "SF:../outside-secret.txt\nDA:1,1\nend_of_record\nSF:/etc/passwd\nDA:2,2\nend_of_record\n",
        );
    }

    let report = scan(repo.path());
    for file in &report.files {
        assert!(
            !file.path.starts_with("..")
                && !file.path.starts_with('/')
                && !file.path.contains("secret"),
            "escaped path surfaced: {}",
            file.path
        );
    }
    assert!(report.files.iter().all(|f| f.path == "src/lib.rs"));
}

#[test]
fn oversized_artifact_is_skipped() {
    let repo = git_repo();
    write(repo.path(), "src/lib.rs", "fn a() {}\n");
    write(
        repo.path(),
        "lcov.info",
        "SF:src/lib.rs\nDA:1,1\nend_of_record\n",
    );
    let limits = ScanLimits {
        max_artifact_bytes: 4,
        ..ScanLimits::default()
    };
    let (report, _) =
        CoverageScanner::scan_with_limits(repo.path().to_str().unwrap(), limits).expect("scan");
    let artifact = report
        .artifacts
        .iter()
        .find(|a| a.path == "lcov.info")
        .expect("artifact listed");
    assert!(artifact.skipped);
    assert!(report.files.is_empty());
}

#[test]
fn artifact_cap_sets_truncated_flag() {
    let repo = git_repo();
    write(repo.path(), "src/app.ts", "export const a = 1;\n");
    write(repo.path(), "src/b.ts", "export const b = 2;\n");
    write(
        repo.path(),
        "coverage/lcov.info",
        "SF:src/app.ts\nDA:1,1\nend_of_record\n",
    );
    write(
        repo.path(),
        "lcov.info",
        "SF:src/b.ts\nDA:1,1\nend_of_record\n",
    );
    let limits = ScanLimits {
        max_artifacts: 1,
        ..ScanLimits::default()
    };
    let (report, _) =
        CoverageScanner::scan_with_limits(repo.path().to_str().unwrap(), limits).expect("scan");
    assert!(report.truncated);
}

#[test]
fn file_cap_trims_merged_set_and_flags_truncation() {
    let repo = git_repo();
    write(repo.path(), "src/lib.rs", "fn a() {}\n");
    let mut text = String::new();
    for i in 0..50usize {
        text.push_str(&format!("SF:src/gen/mod_{i}.rs\nDA:1,1\nend_of_record\n"));
    }
    write(repo.path(), "lcov.info", &text);
    let limits = ScanLimits {
        max_files: 10,
        ..ScanLimits::default()
    };
    let (report, _) =
        CoverageScanner::scan_with_limits(repo.path().to_str().unwrap(), limits).expect("scan");
    assert!(report.truncated);
    assert!(report.files.len() <= 10);
}

#[test]
fn concurrent_scans_agree() {
    let repo = git_repo();
    write(repo.path(), "src/lib.rs", "fn a() {}\n");
    write(
        repo.path(),
        "lcov.info",
        "SF:src/lib.rs\nDA:1,1\nDA:2,0\nend_of_record\n",
    );
    let path = repo.path().to_str().unwrap().to_string();
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let p = path.clone();
            std::thread::spawn(move || CoverageScanner::scan(&p).unwrap())
        })
        .collect();
    let mut baseline: Option<(usize, usize)> = None;
    for h in handles {
        let report = h.join().unwrap();
        let totals = (report.overall.lines_found, report.overall.lines_hit);
        match baseline {
            None => baseline = Some(totals),
            Some(prev) => assert_eq!(prev, totals),
        }
    }
    assert_eq!(baseline, Some((2, 1)));
}
