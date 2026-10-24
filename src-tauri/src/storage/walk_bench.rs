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
