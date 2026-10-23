//! Adversarial tests for the storage walkers (W6-L5, ft-eeb28bf6 review).
//!
//! Tests named `BREAK_*` are expected to FAIL on the reviewed tree: each one
//! pins a claim the walkers' own doc comments make (or the engineering bar
//! demands) that the code does not keep. Tests named `held_*` pass and record
//! an attack that did not land.

use super::*;
use std::ffi::OsStr;
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;

fn write_file(path: &Path, size: usize) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, vec![b'x'; size]).unwrap();
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

/// Makes `path` unreadable for the duration of a test. Returns false when the
/// process can read it anyway (root), so the caller can skip.
#[cfg(unix)]
fn lock(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o000)).unwrap();
    std::fs::read_dir(path).is_err() && std::fs::read_to_string(path.join("gitdir")).is_err()
}

#[cfg(unix)]
fn unlock(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755));
}

// ---- 2. NotFound loophole -------------------------------------------------

/// BREAK: a walk root that does not exist reports (0, not truncated, nothing
/// counted): indistinguishable from an empty directory. A stale mount point or
/// a prunable linked worktree whose directory is gone is sized as "0 B,
/// complete" in `collect_worktree_usage`.
#[test]
fn break_missing_walk_root_is_a_clean_empty_walk() {
    let tmp = tempfile::tempdir().unwrap();
    let gone = tmp.path().join("stale-mount");

    let mut budget = WalkBudget::new();
    let (bytes, truncated) = size_tree(&gone, &mut budget);
    assert_eq!(bytes, 0);
    assert!(
        truncated || budget.truncated || budget.unreadable > 0,
        "size_tree: a missing ROOT is not a race with a delete; it must not read as a complete empty tree"
    );

    let mut budget = WalkBudget::new();
    assert_eq!(walk(&gone, &mut budget), 0);
    assert!(
        budget.truncated || budget.unreadable > 0,
        "walk_worktree: a missing ROOT must be counted"
    );
}

/// Held: a dangling symlink to a directory is a symlink (never followed); its
/// own size counts and nothing is flagged.
#[cfg(unix)]
#[test]
fn held_dangling_directory_symlink_is_neither_hidden_nor_flagged() {
    let tmp = tempfile::tempdir().unwrap();
    write_file(&tmp.path().join("real/a.bin"), 10);
    std::os::unix::fs::symlink(tmp.path().join("no-such-dir"), tmp.path().join("dangling"))
        .unwrap();
    let link_len = std::fs::symlink_metadata(tmp.path().join("dangling"))
        .unwrap()
        .len();

    let mut budget = WalkBudget::new();
    let (bytes, truncated) = size_tree(tmp.path(), &mut budget);
    assert_eq!(bytes, 10 + link_len);
    assert!(!truncated && !budget.truncated);
    assert_eq!((budget.unreadable, budget.permission_denied), (0, 0));

    let mut budget = WalkBudget::new();
    assert_eq!(walk(tmp.path(), &mut budget), 10 + link_len);
    assert!(!budget.truncated);
}

/// Held: NotFound on a file stat after it was listed is dropped without a
/// count or a truncation, in both walkers (injection cannot make NotFound, so
/// this drives `note_io_error`, the single classifier every arm calls).
#[test]
fn held_not_found_never_truncates() {
    let mut budget = WalkBudget::new();
    for _ in 0..3 {
        budget.note_io_error(&io::Error::from(io::ErrorKind::NotFound));
    }
    assert_eq!((budget.unreadable, budget.permission_denied), (0, 0));
    assert!(!budget.truncated);
}

// ---- 4. truncated semantics ----------------------------------------------

/// BREAK (design): an EACCES subtree leaves its bytes out of every total, yet
/// `truncated` stays false, so `scan.truncated` (StoragePanel "partial scan"
/// badge, fleet `storage_truncated`, `reclaim_summary.partial`) reads the
/// total as complete. EIO sets `truncated`; EACCES, which loses the same
/// bytes, does not. The two classes disagree about what a floor is.
#[cfg(unix)]
#[test]
fn break_permission_denied_subtree_leaves_totals_looking_complete() {
    let tmp = tempfile::tempdir().unwrap();
    write_file(&tmp.path().join("ok/a.bin"), 100);
    write_file(&tmp.path().join("locked/b.bin"), 10_000);
    let locked = tmp.path().join("locked");
    if !lock(&locked) {
        unlock(&locked);
        return; // running as root: cannot make a directory unreadable
    }
    let mut budget = WalkBudget::new();
    let (bytes, _) = size_tree(tmp.path(), &mut budget);
    unlock(&locked);
    assert_eq!(bytes, 100, "the locked subtree is not in the total");
    assert_eq!(budget.permission_denied, 1);
    assert!(
        budget.truncated,
        "10 000 bytes are missing from the total but the scan is not marked partial"
    );
}

// ---- 1. remaining silent arms --------------------------------------------

/// BREAK: `orphaned_worktree_admin` reads `gitdir` with `Err(_) =>` for every
/// error, so an unreadable gitdir (EACCES here, EIO in the field) is reported
/// as an orphaned admin dir with `ReclaimSafety::Safe` / "git worktree prune"
/// and nothing is counted in `unreadable` / `permission_denied`.
#[cfg(unix)]
#[test]
fn break_unreadable_gitdir_is_reported_orphaned_and_uncounted() {
    let tmp = tempfile::tempdir().unwrap();
    let git_dir = tmp.path().join(".git");
    let admin = git_dir.join("worktrees/live");
    std::fs::create_dir_all(&admin).unwrap();
    let live_target = tmp.path().join("live-checkout/.git");
    write_file(&live_target, 1);
    std::fs::write(admin.join("gitdir"), live_target.to_str().unwrap()).unwrap();
    let gitdir_file = admin.join("gitdir");
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&gitdir_file, std::fs::Permissions::from_mode(0o000)).unwrap();
    }
    if std::fs::read_to_string(&gitdir_file).is_ok() {
        return; // root
    }
    let mut budget = WalkBudget::new();
    let found = orphaned_worktree_admin(&git_dir, &mut budget);
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&gitdir_file, std::fs::Permissions::from_mode(0o644)).unwrap();
    }
    assert!(
        found.is_empty() || budget.unreadable + budget.permission_denied > 0,
        "a LIVE worktree's admin dir was reported as prunable ({found:?}) and the failed read was counted nowhere"
    );
}

/// BREAK: the audit stops at `MAX_SIZED_WORKTREES * 8` admin entries with
/// `take(..)` and says nothing: a capped sample presented as the whole list.
#[test]
fn break_orphan_audit_cap_is_silent() {
    let tmp = tempfile::tempdir().unwrap();
    let git_dir = tmp.path().join(".git");
    let total = MAX_SIZED_WORKTREES * 8 + 6;
    for i in 0..total {
        let admin = git_dir.join(format!("worktrees/wt{i:03}"));
        std::fs::create_dir_all(&admin).unwrap();
        std::fs::write(
            admin.join("gitdir"),
            tmp.path().join(format!("gone{i}/.git")).to_str().unwrap(),
        )
        .unwrap();
    }
    let mut budget = WalkBudget::new();
    let found = orphaned_worktree_admin(&git_dir, &mut budget);
    assert!(
        found.len() == total || budget.truncated,
        "{} of {total} orphans listed and the scan is not marked partial",
        found.len()
    );
}

/// BREAK (count inflation): `.git` is walked by `size_tree_scoped`, then
/// `logs`, `refs`, `worktrees`... are walked AGAIN by `git_subdir_bytes` on the
/// same budget, so one unreadable location under `.git/logs` is counted twice
/// (three times for `.git/worktrees`, via `orphaned_worktree_admin`). The
/// panel's "N location(s) failed to read" overstates. No effect on truncation.
#[test]
fn break_unreadable_git_subdir_is_counted_once_per_walk_not_once() {
    let tmp = tempfile::tempdir().unwrap();
    let git_dir = tmp.path().join(".git");
    write_file(&git_dir.join("logs/refs/heads/main"), 10);
    write_file(&git_dir.join("logs/bad/x"), 10);
    let mut budget = WalkBudget::new();
    budget.fail_paths = vec![git_dir.join("logs/bad")];
    let _ = size_tree_scoped(&git_dir, &mut budget, true);
    let _ = git_subdir_bytes(&git_dir, "logs", &mut budget);
    assert_eq!(budget.unreadable, 1, "one bad location, counted once");
}

// ---- 3. non-UTF-8 --------------------------------------------------------

/// BREAK: two different non-UTF-8 names get the same label, and the label is
/// what `walk_worktree` uses as `child_rel`, the `ancestor_names` repetition
/// key, and (through `ArtifactDir.path`) the `check-ignore` / `ls-files`
/// candidate. Distinct directories become one key.
#[cfg(unix)]
#[test]
fn break_distinct_non_utf8_names_share_one_label() {
    let mut budget = WalkBudget::new();
    let a = budget.label(OsStr::from_bytes(b"caf\xe9"));
    let b = budget.label(OsStr::from_bytes(b"caf\xe8"));
    assert_ne!(a, b, "two directories collapsed to {a:?}");
}

/// BREAK: the artifact path built by the walker for a non-UTF-8 ancestor does
/// not name the directory. This replays exactly what `walk_worktree` does
/// (label -> `child_rel` -> `on_dir_enter(abs, rel, name)`); APFS refuses to
/// create the directory (EILSEQ) so the real walk cannot be run here.
/// `ArtifactDir.path` is `rel`, which is what `batch_tracked_counts` and
/// `batch_check_ignore` hand to git: a path that does not exist, so
/// `tracked_files == 0` and `unignored` are computed for nothing, and an
/// artifact holding tracked files can come out `ReclaimSafety::Safe`.
#[cfg(unix)]
#[test]
fn break_non_utf8_artifact_path_does_not_resolve_to_the_directory() {
    let root = Path::new("/repo");
    let raw = OsStr::from_bytes(b"caf\xe9");
    let mut budget = WalkBudget::new();
    let label = budget.label(raw);
    let rel = format!("{label}/node_modules");
    let abs = root.join(raw).join("node_modules");

    let mut collector = ArtifactCollector::new();
    collector.on_dir_enter(&abs, &rel, "node_modules");
    collector.on_file(4096);
    let (found_rel, _, found_abs, bytes) = &collector.found[0];
    assert_eq!(*bytes, 4096);
    assert_eq!(
        root.join(found_rel),
        *found_abs,
        "the artifact's repo-relative path must name the directory that was measured"
    );
}

/// Held (what could run): the label is only a label. The traversal path is
/// built from `entry.path()` (OsString, byte exact), never from the label, and
/// a valid name round-trips unchanged and uncounted.
#[cfg(unix)]
#[test]
fn held_label_counts_only_invalid_names_and_keeps_valid_ones_exact() {
    let mut budget = WalkBudget::new();
    assert_eq!(budget.label(OsStr::new("plain-\u{e9}")), "plain-\u{e9}");
    assert_eq!(budget.non_utf8, 0);
    let lossy = budget.label(OsStr::from_bytes(b"\xff\xfe"));
    assert_eq!(lossy, "\u{fffd}\u{fffd}");
    assert_eq!(budget.non_utf8, 1);
    // `name()` and `label()` count per CALL: the walker asks for it once per
    // entry, but a directory listed by two walks is counted by each.
    assert_eq!(budget.label(OsStr::from_bytes(b"\xff\xfe")), lossy);
    assert_eq!(budget.non_utf8, 2);
}
