//! Walker timing probe (W6-L5 review). Run: `GITPULSE_BENCH_TREE=<dir> cargo
//! test --lib walk_bench -- --ignored --nocapture`. Builds a 200k-file tree on
//! first use, then prints min-of-N milliseconds for `size_tree` and
//! `walk_worktree`. Only uses items that exist on both sides of the review
//! (`WalkBudget::with_limits`, `size_tree`, `walk_worktree`).

use super::*;

fn ensure_tree(root: &Path) {
    if root.join(".built").exists() {
        return;
    }
    for d in 0..2_000 {
        let dir = root.join(format!("pkg{:02}/mod{d:04}", d % 20));
        std::fs::create_dir_all(&dir).unwrap();
        for f in 0..100 {
            std::fs::write(dir.join(format!("f{f:03}.rs")), b"x").unwrap();
        }
    }
    std::fs::write(root.join(".built"), b"").unwrap();
}

fn budget() -> WalkBudget {
    WalkBudget::with_limits(Instant::now() + Duration::from_secs(600), 1_000_000)
}

fn walk(root: &Path, budget: &mut WalkBudget) -> u64 {
    let mut collector = ArtifactCollector::new();
    let mut entered = Vec::new();
    let mut large = LargeFileCollector::new();
    let linked = std::collections::HashSet::new();
    let mut ancestors = Vec::new();
    #[cfg(unix)]
    let mut seen = std::collections::HashSet::new();
    #[cfg(unix)]
    let mut seen_dirs = std::collections::HashSet::new();
    let mut ctx = WorktreeWalkContext {
        root,
        collector: &mut collector,
        entered_depths: &mut entered,
        #[cfg(unix)]
        seen_inodes: &mut seen,
        large_collector: &mut large,
        linked_worktrees: &linked,
        ancestor_names: &mut ancestors,
        #[cfg(unix)]
        seen_dir_inodes: &mut seen_dirs,
    };
    walk_worktree(&mut ctx, root, 0, budget)
}

#[test]
#[ignore]
fn walk_bench() {
    let root = PathBuf::from(std::env::var("GITPULSE_BENCH_TREE").expect("GITPULSE_BENCH_TREE"));
    std::fs::create_dir_all(&root).unwrap();
    ensure_tree(&root);
    let (mut size_ms, mut walk_ms) = (u128::MAX, u128::MAX);
    for _ in 0..5 {
        let mut b = budget();
        let t = Instant::now();
        let (bytes, _) = size_tree(&root, &mut b);
        size_ms = size_ms.min(t.elapsed().as_micros());
        assert_eq!(b.files_visited, 200_001, "bytes {bytes}");

        let mut b = budget();
        let t = Instant::now();
        let bytes = walk(&root, &mut b);
        walk_ms = walk_ms.min(t.elapsed().as_micros());
        assert!(bytes >= 200_000);
    }
    println!("BENCH size_tree_us={size_ms} walk_worktree_us={walk_ms}");
}

/// Read-only diagnosis probe: runs a real `scan_storage` against
/// `GITPULSE_SCAN_REPO` and prints every signal that feeds `scan.truncated`,
/// so a "Partial repository scan" verdict can be attributed to a cause instead
/// of guessed at. Run:
/// `GITPULSE_SCAN_REPO=<repo> cargo test --lib scan_probe -- --ignored --nocapture`
#[test]
#[ignore]
fn scan_probe() {
    let repo = std::env::var("GITPULSE_SCAN_REPO").expect("GITPULSE_SCAN_REPO");
    // Session-scoped, in-process grant (`remember: false`): nothing is written
    // to the owner's trust store and nothing changes in the app.
    crate::test_support::trust_repo(Path::new(&repo));
    let t = Instant::now();
    let report = crate::storage::scan_storage(&repo).expect("scan");
    let s = &report.scan;
    println!(
        "PROBE repo={repo}\n  elapsed={}ms truncated={} files_visited={} \
         permission_denied={} unreadable={} non_utf8={} unaudited={}\n  \
         artifacts={} (cap {}) worktrees_sized={} worktree_bytes={:.2}GiB",
        t.elapsed().as_millis(),
        s.truncated,
        s.files_visited,
        s.permission_denied,
        s.unreadable,
        s.non_utf8,
        s.unaudited,
        report.artifacts.len(),
        MAX_ARTIFACT_DIRS,
        report.worktrees.len(),
        report.totals.worktree_bytes as f64 / (1u64 << 30) as f64,
    );
    for a in report.artifacts.iter().take(6) {
        println!(
            "  artifact {} bytes={:.2}GiB scan_complete={} unignored={} tracked={} unexamined={}",
            a.path,
            a.bytes as f64 / (1u64 << 30) as f64,
            a.scan_complete,
            a.unignored,
            a.tracked_files,
            a.checks_unexamined
        );
    }
    for wt in &report.worktrees {
        println!("  worktree {} truncated={}", wt.name, wt.truncated);
    }
}
