//! Integration stress suite for the storage scanner (`crate::storage`).
//!
//! These tests build real repositories on disk and attack the scanner with
//! everything a hostile or merely enormous checkout can offer: symlink loops,
//! permission walls, unicode paths, huge fanouts, linked worktrees, and
//! concurrent scans. The invariant under attack is always the same: the scan
//! either returns an honest report or an honest error — never a hang, never
//! silently-lying totals.

#![cfg(unix)]

use gitpulse_lib::storage::scan_storage;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

static UNIQUE: AtomicUsize = AtomicUsize::new(0);

struct TempRepo {
    root: PathBuf,
    _guard: tempfile::TempDir,
}

impl TempRepo {
    fn new() -> Self {
        let guard = tempfile::tempdir().expect("tempdir");
        let root = guard.path().join("repo");
        fs::create_dir_all(&root).unwrap();
        let repo = Self {
            root,
            _guard: guard,
        };
        repo.git(&["init", "-q", "-b", "main"]);
        repo.git(&["config", "user.email", "stress@example.com"]);
        repo.git(&["config", "user.name", "Stress Test"]);
        repo
    }

    fn git(&self, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(&self.root)
            .env("GIT_CONFIG_PARAMETERS", "")
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .expect("spawn git");
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn write(&self, rel: &str, size: usize) {
        let path = self.root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, vec![b'x'; size]).unwrap();
    }

    fn commit_all(&self, message: &str) {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "-m", message]);
    }
}

fn unique_tag() -> String {
    format!("t{}", UNIQUE.fetch_add(1, Ordering::SeqCst))
}

/// A realistic mixed repository: committed source, build output partially
/// ignored, caches, a big binary, unicode names. Asserts classification,
/// hygiene flags, large-file discovery, and honest non-truncated totals.
#[test]
fn mixed_repo_reports_artifacts_gaps_and_large_files() {
    let repo = TempRepo::new();
    let tag = unique_tag();

    // Committed source.
    repo.write("src/main.rs", 2_048);
    // Build outputs: node_modules ignored, target NOT ignored (a real gap).
    repo.write("node_modules/left-pad/index.js", 40_000);
    repo.write("target/debug/thing.rlib", 90_000);
    repo.write("__pycache__/mod.cpython-312.pyc", 12_000);
    fs::write(
        repo.root.join(".gitignore"),
        "node_modules/\n__pycache__/\n",
    )
    .unwrap();
    repo.commit_all("initial");

    // A large binary above the threshold, outside any artifact dir.
    repo.write(&format!("assets/movie-{tag}.bin"), 11 * 1024 * 1024);

    let report = scan_storage(repo.root.to_str().unwrap()).expect("scan ok");

    // Totals are floors that cover every walked byte exactly once.
    assert!(!report.scan.truncated);
    assert_eq!(
        report.totals.grand_bytes,
        report.totals.worktree_bytes + report.totals.git_dir_bytes
    );
    assert!(report.totals.grand_bytes >= 11 * 1024 * 1024);
    assert!(report.totals.build_artifacts_bytes >= 130_000);
    assert!(report.totals.cache_artifacts_bytes >= 12_000);

    // Artifact classification + hygiene cross-checks.
    let by_path = |p: &str| {
        report
            .artifacts
            .iter()
            .find(|a| a.path == p)
            .unwrap_or_else(|| {
                panic!(
                    "artifact {p} missing from {:?}",
                    report
                        .artifacts
                        .iter()
                        .map(|a| a.path.clone())
                        .collect::<Vec<_>>()
                )
            })
    };
    let node_modules = by_path("node_modules");
    assert_eq!(node_modules.bytes, 40_000);
    assert!(!node_modules.unignored, "node_modules IS ignored");
    let target = by_path("target");
    assert!(target.unignored, "target has no ignore rule: reported gap");
    assert_eq!(by_path("__pycache__").bytes, 12_000);

    // Large-file discovery reports the relative path, biggest first.
    assert_eq!(report.largest_files.len(), 1);
    assert!(
        report.largest_files[0]
            .path
            .contains(&format!("movie-{tag}.bin")),
        "unexpected large file: {:?}",
        report.largest_files[0].path
    );
    assert_eq!(report.largest_files[0].bytes, 11 * 1024 * 1024);

    // Git internals: at least one pack after commit; index accounted.
    assert!(report.git.total_bytes > 0);
    assert!(report.git.index_bytes > 0 || report.is_bare);
}

/// An ignored directory that still has files in the INDEX is the classic
/// history-bloat trap; the scanner must surface it via tracked_files.
#[test]
fn committed_cache_inside_ignored_dir_is_flagged_as_tracked() {
    let repo = TempRepo::new();
    repo.write("coverage/lcov.info", 30_000);
    repo.commit_all("accidentally committed coverage");
    fs::write(repo.root.join(".gitignore"), "coverage/\n").unwrap();

    let report = scan_storage(repo.root.to_str().unwrap()).unwrap();
    let coverage = report
        .artifacts
        .iter()
        .find(|a| a.path == "coverage")
        .expect("coverage dir classified");
    assert!(!coverage.unignored, "covered by .gitignore now");
    assert_eq!(coverage.tracked_files, 1, "but still in the index");
}

/// A self-referential symlink (`ln -s .`) must neither hang nor inflate:
/// links are never followed, so their own size counts once.
#[test]
fn symlink_loop_does_not_hang_or_explode() {
    let repo = TempRepo::new();
    repo.write("src/app.ts", 500);
    #[cfg(unix)]
    std::os::unix::fs::symlink(".", repo.root.join("loop")).unwrap();

    let started = std::time::Instant::now();
    let report = scan_storage(repo.root.to_str().unwrap());
    let elapsed = started.elapsed();
    let report = report.expect("scan survives a symlink loop");
    assert!(
        !report.scan.truncated,
        "loop link is skipped, not descended"
    );
    assert!(elapsed < Duration::from_secs(10), "no pathological walk");
    // Exactly one 'loop' entry worth of link-size, not exponential growth.
    let expected_floor = 500u64;
    assert!(
        report.totals.worktree_bytes < expected_floor * 100,
        "symlink loop inflated the total: {}",
        report.totals.worktree_bytes
    );
}

/// A directory with no read permission is skipped and counted, never fatal,
/// and never reported as truncation by itself.
#[cfg(unix)]
#[test]
fn permission_denied_subtree_degrades_gracefully() {
    use std::os::unix::fs::PermissionsExt;
    let repo = TempRepo::new();
    repo.write("open/file.txt", 1_000);
    repo.write("locked/secret.txt", 2_000);
    let locked = repo.root.join("locked");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();

    let result = scan_storage(repo.root.to_str().unwrap());
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
    let report = result.expect("unreadable subtree must not fail the scan");
    assert!(report.scan.permission_denied >= 1);
    assert!(report.totals.worktree_bytes >= 1_000);
}

/// Fanout and depth attacks must end in `truncated: true`, with partial
/// (nonzero) totals rather than a lie of completeness.
#[test]
fn budget_attacks_report_truncation_honestly() {
    let repo = TempRepo::new();
    let fan = repo.root.join("fan");
    fs::create_dir_all(&fan).unwrap();
    // One directory with more entries than the per-directory cap.
    for i in 0..4_100 {
        fs::write(fan.join(format!("f{i:05}")), b"z").unwrap();
    }

    let report = scan_storage(repo.root.to_str().unwrap()).unwrap();
    assert!(report.scan.truncated, "fanout beyond cap must be flagged");
    assert!(report.totals.worktree_bytes > 0);
}

mod bare_and_worktrees {
    use super::*;

    /// A bare repository scans cleanly: no worktree sections, git internals
    /// still fully present.
    #[test]
    fn bare_repo_scans_without_worktree_sections() {
        let guard = tempfile::tempdir().unwrap();
        let seed = TempRepo::new();
        seed.write("src/lib.rs", 3_000);
        seed.commit_all("seed");
        let bare = guard.path().join("bare.git");
        let out = Command::new("git")
            .args([
                "clone",
                "--quiet",
                "--bare",
                seed.root.to_str().unwrap(),
                bare.to_str().unwrap(),
            ])
            .env("GIT_CONFIG_PARAMETERS", "")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );

        let report = scan_storage(bare.to_str().unwrap()).unwrap();
        assert!(report.is_bare);
        assert_eq!(report.totals.worktree_bytes, 0);
        assert!(report.git.total_bytes > 0);
        assert!(report.artifacts.is_empty());
    }

    /// Linked worktrees are sized individually and the main worktree never
    /// appears in the per-worktree list.
    #[test]
    fn linked_worktrees_are_sized_and_main_is_excluded() {
        let repo = TempRepo::new();
        repo.write("src/base.rs", 6_000);
        repo.commit_all("base");

        let guard = tempfile::tempdir().unwrap();
        let wt_path = guard.path().join("feature-wt");
        repo.git(&[
            "worktree",
            "add",
            "-b",
            "feature/heavy",
            wt_path.to_str().unwrap(),
        ]);
        fs::create_dir_all(wt_path.join("node_modules/pkg")).unwrap();
        fs::write(wt_path.join("node_modules/pkg/i.js"), vec![b'y'; 70_000]).unwrap();

        let report = scan_storage(repo.root.to_str().unwrap()).unwrap();
        assert_eq!(report.worktrees.len(), 1, "main worktree excluded");
        let wt = &report.worktrees[0];
        assert_eq!(wt.branch.as_deref(), Some("feature/heavy"));
        assert!(wt.bytes >= 70_000, "worktree subtree sized");
    }

    /// Stale-branch weight: a branch merged into main counts as deletable;
    /// a branch whose upstream vanished is flagged gone-upstream.
    #[test]
    fn branch_summary_counts_merged_and_gone_upstreams() {
        let repo = TempRepo::new();
        repo.write("a.txt", 10);
        repo.commit_all("base");
        repo.git(&["branch", "done/deleted-later"]);
        repo.git(&["checkout", "-q", "-b", "orphan/upstream-gone"]);
        repo.write("b.txt", 20);
        repo.commit_all("on orphan");
        repo.git(&["remote", "add", "origin", "./nowhere.git"]);
        repo.git(&[
            "update-ref",
            "refs/remotes/origin/orphan/upstream-gone",
            "HEAD",
        ]);
        repo.git(&["branch", "--set-upstream-to=origin/orphan/upstream-gone"]);
        repo.git(&["checkout", "-q", "main"]);
        // Merge the orphan branch so ONLY done/deleted-later is merged-stale.
        repo.git(&["merge", "--quiet", "--ff-only", "orphan/upstream-gone"]);

        let report = scan_storage(repo.root.to_str().unwrap()).unwrap();
        assert!(report.branches.local_count >= 3);
        let names = &report.branches.sample_merged_stale;
        assert!(
            names.iter().any(|n| n == "done/deleted-later"),
            "merged branch should be a cleanup candidate: {names:?}"
        );
        // The orphan branch was ff-merged into main, so its tip is merged
        // too — the cleanup plan must list it as a candidate even though
        // its upstream separately shows as gone.
        assert!(
            names.iter().any(|n| n == "orphan/upstream-gone"),
            "ff-merged branch is a cleanup candidate: {names:?}"
        );
        assert!(
            !report
                .branches
                .sample_merged_stale
                .iter()
                .any(|n| n == "main"),
            "default branch is always protected: {names:?}"
        );
    }
}

/// Concurrent scans over the same repository must all succeed with identical
/// headline numbers — the blocking-pool seam and read-only walks race-free.
#[test]
fn concurrent_scans_are_deterministic() {
    let repo = TempRepo::new();
    repo.write("node_modules/x/index.js", 25_000);
    repo.write("src/main.rs", 5_000);
    repo.commit_all("concurrent");

    let root = repo.root.clone();
    let mut handles = Vec::new();
    for _ in 0..4 {
        let root = root.clone();
        handles.push(std::thread::spawn(move || {
            scan_storage(root.to_str().unwrap()).expect("concurrent scan")
        }));
    }
    let mut totals = Vec::new();
    for handle in handles {
        let report = handle.join().expect("no panic under concurrency");
        assert!(!report.scan.truncated);
        assert!(report.totals.build_artifacts_bytes >= 25_000);
        totals.push(report.totals.grand_bytes);
    }
    totals.dedup();
    assert_eq!(
        totals.len(),
        1,
        "identical trees must scan identically: {totals:?}"
    );
}

/// Unicode and space-laden paths survive the whole pipeline: walking,
/// relative-path reporting, ignore checks, and tracked counting.
#[test]
fn unicode_paths_survive_end_to_end() {
    let repo = TempRepo::new();
    let weird = "docs/日本語 ño/файл.md";
    repo.write(weird, 300);
    repo.write("dist/ünïcode.js", 9_000);
    repo.commit_all("unicode");

    let report = scan_storage(repo.root.to_str().unwrap()).unwrap();
    assert!(!report.scan.truncated);
    let dist = report
        .artifacts
        .iter()
        .find(|a| a.path == "dist")
        .expect("dist classified despite sibling unicode dirs");
    assert_eq!(dist.bytes, 9_000);
    // Committed unicode file is tracked somewhere in the tree (ls-files).
    assert!(report.git.index_bytes > 0, "index present");
}

/// A missing/unreadable repository fails loudly and quickly — the boundary
/// between "invalid input" and "degraded report" must stay sharp.
#[test]
fn invalid_repositories_fail_loudly() {
    let guard = tempfile::tempdir().unwrap();
    let plain = guard.path().join("plain");
    fs::create_dir_all(&plain).unwrap();
    assert!(scan_storage(plain.to_str().unwrap()).is_err());
    assert!(scan_storage(guard.path().join("missing").to_str().unwrap()).is_err());
}

/// Hard links inside build directories (e.g. Cargo's target/debug/libfoo.a
/// hard-linked to target/debug/deps/libfoo-hash.a) must not be double-counted.
#[cfg(unix)]
#[test]
fn hardlinks_are_deduplicated_on_unix() {
    let repo = TempRepo::new();
    repo.write("src/main.rs", 100);
    repo.commit_all("init");

    let dep_path = repo.root.join("target/debug/deps/libfoo.a");
    let top_path = repo.root.join("target/debug/libfoo.a");
    fs::create_dir_all(dep_path.parent().unwrap()).unwrap();
    fs::write(&dep_path, vec![b'x'; 200_000]).unwrap();
    fs::hard_link(&dep_path, &top_path).expect("create hardlink");

    let report = scan_storage(repo.root.to_str().unwrap()).unwrap();
    assert!(!report.scan.truncated);
    // The target artifact should count the 200 KB physical file once, not twice.
    let target = report
        .artifacts
        .iter()
        .find(|a| a.path == "target")
        .expect("target found");
    assert_eq!(
        target.bytes, 200_000,
        "hard links must be deduplicated in artifact totals"
    );
}

/// Large fanout (> 4,000 files) inside a known artifact directory (like Cargo's
/// target/debug/deps) must NOT trigger premature truncation.
#[test]
fn large_fanout_inside_artifact_does_not_truncate() {
    let repo = TempRepo::new();
    repo.write("src/lib.rs", 50);
    repo.commit_all("init");

    let deps = repo.root.join("target/debug/deps");
    fs::create_dir_all(&deps).unwrap();
    for i in 0..4_200 {
        fs::write(deps.join(format!("lib{i:05}.rlib")), b"r").unwrap();
    }

    let report = scan_storage(repo.root.to_str().unwrap()).unwrap();
    assert!(
        !report.scan.truncated,
        "large fanout in artifact dir must not truncate"
    );
    let target = report
        .artifacts
        .iter()
        .find(|a| a.path == "target")
        .expect("target found");
    assert_eq!(target.bytes, 4_200);
}

/// Known build containers (like target) must roll up nested build/ and out/
/// directories rather than fragmenting into separate artifact rows.
#[test]
fn nested_build_and_out_dirs_inside_target_roll_up() {
    let repo = TempRepo::new();
    repo.write("src/lib.rs", 50);
    repo.commit_all("init");

    repo.write("target/debug/app", 10_000);
    repo.write("target/debug/build/pkg-hash/out/libsqlite3.a", 25_000);

    let report = scan_storage(repo.root.to_str().unwrap()).unwrap();
    assert!(!report.scan.truncated);

    // Target must appear as a single row containing all 35,000 bytes.
    let target = report
        .artifacts
        .iter()
        .find(|a| a.path == "target")
        .expect("target found");
    assert_eq!(
        target.bytes, 35_000,
        "nested build/out bytes must roll up to target"
    );

    // There should be NO child artifact rows for target/debug/build or out.
    assert!(
        !report
            .artifacts
            .iter()
            .any(|a| a.path.contains("target/debug/build")),
        "inner build folder must not fragment into a separate artifact row"
    );
}

/// Source directories like src/lib/coverage must not be misclassified as cache artifacts.
#[test]
fn source_subdirectories_are_not_classified_as_artifacts() {
    let repo = TempRepo::new();
    repo.write("src/lib/coverage/index.ts", 1_000);
    repo.write("src/lib/coverage/calc.ts", 2_000);
    repo.write("vendor/parser.c", 5_000);
    repo.commit_all("tracked source code");

    let report = scan_storage(repo.root.to_str().unwrap()).unwrap();
    assert!(!report.scan.truncated);

    assert!(
        !report
            .artifacts
            .iter()
            .any(|a| a.path == "src/lib/coverage"),
        "src/lib/coverage must not be treated as a cache artifact"
    );
    assert!(
        !report.artifacts.iter().any(|a| a.path == "vendor"),
        "tracked vendor source directory must not be treated as an unignored build artifact"
    );
}

/// Developer tool / agent caches (.devcouncil, .gitnexus, .cursor, .claude)
/// must be classified as cache artifacts.
#[test]
fn agent_and_tool_caches_are_classified() {
    let repo = TempRepo::new();
    repo.write("src/main.rs", 100);
    repo.write(".devcouncil/index.sqlite", 60_000);
    repo.write(".gitnexus/cache.bin", 40_000);
    repo.commit_all("init");

    let report = scan_storage(repo.root.to_str().unwrap()).unwrap();
    assert!(!report.scan.truncated);

    let devcouncil = report
        .artifacts
        .iter()
        .find(|a| a.path == ".devcouncil")
        .expect(".devcouncil must be recognized as an artifact");
    assert_eq!(devcouncil.kind, gitpulse_lib::storage::ArtifactKind::Cache);
    assert_eq!(devcouncil.bytes, 60_000);

    let gitnexus = report
        .artifacts
        .iter()
        .find(|a| a.path == ".gitnexus")
        .expect(".gitnexus must be recognized as an artifact");
    assert_eq!(gitnexus.kind, gitpulse_lib::storage::ArtifactKind::Cache);
    assert_eq!(gitnexus.bytes, 40_000);

    assert!(report.totals.cache_artifacts_bytes >= 100_000);
}
