//! Turn checkpoints against real repositories.

use super::*;
use crate::procguard::LockedSpawn;
use std::process::Command;

fn git(dir: &Path, args: &[&str]) -> std::process::Output {
    let mut command = Command::new("git");
    crate::test_support::pin_git(&mut command);
    command
        .args(args)
        .current_dir(dir)
        .output_locked()
        .unwrap_or_else(|e| panic!("spawn git {}: {e}", args.join(" ")))
}

fn git_ok(dir: &Path, args: &[&str]) -> String {
    let out = git(dir, args);
    assert!(
        out.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim_end().to_string()
}

fn init() -> tempfile::TempDir {
    let dir = tempfile::TempDir::new().unwrap();
    git_ok(dir.path(), &["init", "-q", "-b", "main"]);
    for (k, v) in [
        ("user.name", "t"),
        ("user.email", "t@t"),
        ("commit.gpgsign", "false"),
        ("core.autocrlf", "false"),
    ] {
        git_ok(dir.path(), &["config", k, v]);
    }
    std::fs::write(dir.path().join("tracked.txt"), "base\n").unwrap();
    git_ok(dir.path(), &["add", "tracked.txt"]);
    git_ok(dir.path(), &["commit", "-q", "-m", "init"]);
    crate::test_support::trust_repo(dir.path());
    dir
}

fn path_of(dir: &tempfile::TempDir) -> String {
    dir.path().to_str().unwrap().to_string()
}

fn secret() -> String {
    format!("{}{}", "ghp_", "a1B2c3D4e5F6g7H8i9J0k1L2m3N4o5P6q7R8")
}

struct Rig {
    index_dir: tempfile::TempDir,
    cfg: CaptureConfig,
}

fn rig(now_ms: u64) -> Rig {
    let index_dir = tempfile::TempDir::new().unwrap();
    let cfg = CaptureConfig {
        now_ms,
        index_dir: Some(index_dir.path().to_path_buf()),
        ..CaptureConfig::production()
    };
    Rig { index_dir, cfg }
}

fn take(dir: &tempfile::TempDir, cfg: &CaptureConfig) -> CaptureResult {
    take_as(
        dir,
        cfg,
        TurnEvent::TurnStart,
        Some("sess-1"),
        Some("label"),
    )
}

fn take_as(
    dir: &tempfile::TempDir,
    cfg: &CaptureConfig,
    event: TurnEvent,
    session: Option<&str>,
    label: Option<&str>,
) -> CaptureResult {
    capture_turn_checkpoint(
        &path_of(dir),
        &CaptureRequest {
            event,
            session_id: session,
            label,
        },
        cfg,
    )
}

fn tree_names(dir: &Path, commit: &str) -> Vec<String> {
    let out = git(dir, &["ls-tree", "-r", "-z", "--name-only", commit]);
    assert!(out.status.success());
    String::from_utf8_lossy(&out.stdout)
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

fn refs_under(dir: &Path, prefix: &str) -> Vec<String> {
    git_ok(dir, &["for-each-ref", "--format=%(refname)", prefix])
        .lines()
        .map(str::to_string)
        .collect()
}

fn dir_is_empty(dir: &Path) -> bool {
    std::fs::read_dir(dir).unwrap().next().is_none()
}

// ── the repository is left exactly as found ──────────────────────────────────

#[test]
fn capture_leaves_index_head_status_and_stash_exactly_as_found() {
    let dir = init();
    let root = dir.path();
    std::fs::write(root.join("seed.txt"), "s\n").unwrap();
    git_ok(root, &["add", "seed.txt"]);
    git_ok(root, &["commit", "-q", "-m", "two"]);
    std::fs::write(root.join("seed.txt"), "stashed\n").unwrap();
    git_ok(root, &["stash", "push", "-q", "-m", "mine"]);
    std::fs::write(root.join("tracked.txt"), "edited\n").unwrap();
    std::fs::write(root.join("staged.txt"), "staged\n").unwrap();
    git_ok(root, &["add", "staged.txt"]);
    std::fs::write(root.join("fresh.txt"), "fresh\n").unwrap();

    // `status` refreshes the index's stat data; settle it before the baseline.
    let status = |r: &Path| git_ok(r, &["status", "--porcelain=v2", "--untracked-files=all"]);
    let before_status = status(root);
    let index_before = std::fs::read(root.join(".git/index")).unwrap();
    let head_before = git_ok(root, &["rev-parse", "HEAD"]);
    let stash_before = git_ok(root, &["stash", "list"]);
    let refs_before = git_ok(root, &["for-each-ref", "--format=%(refname)"]);

    let rig = rig(1_000_000);
    let result = take(&dir, &rig.cfg);
    assert_eq!(result.outcome, CaptureOutcome::Captured, "{result:?}");

    assert_eq!(
        std::fs::read(root.join(".git/index")).unwrap(),
        index_before
    );
    assert_eq!(git_ok(root, &["rev-parse", "HEAD"]), head_before);
    assert_eq!(status(root), before_status);
    assert_eq!(git_ok(root, &["stash", "list"]), stash_before);
    let refs_after = git_ok(root, &["for-each-ref", "--format=%(refname)"]);
    let added: Vec<&str> = refs_after
        .lines()
        .filter(|r| !refs_before.lines().any(|b| b == *r))
        .collect();
    assert_eq!(added.len(), 1, "{added:?}");
    assert!(added[0].starts_with(TURN_REF_PREFIX));
    assert_eq!(
        std::fs::read_to_string(root.join("tracked.txt")).unwrap(),
        "edited\n"
    );
    assert!(dir_is_empty(rig.index_dir.path()));

    let commit = result.commit.unwrap();
    let names = tree_names(root, &commit);
    assert!(names.contains(&"fresh.txt".to_string()), "{names:?}");
    assert!(names.contains(&"staged.txt".to_string()));
    assert_eq!(
        git_ok(root, &["show", &format!("{commit}:tracked.txt")]),
        "edited"
    );
    assert_eq!(
        git_ok(root, &["rev-parse", &format!("{commit}^")]),
        head_before
    );
}

// ── secrets ──────────────────────────────────────────────────────────────────

#[test]
fn untracked_files_enter_unless_ignored_env_or_flagged_and_the_omissions_are_counted() {
    let dir = init();
    let root = dir.path();
    assert!(
        dc_redact::contains_secret(&secret()),
        "the fixture must be a shape"
    );
    std::fs::write(root.join(".gitignore"), "ignored.log\n").unwrap();
    std::fs::write(root.join("ignored.log"), "x\n").unwrap();
    std::fs::write(root.join("new.txt"), "n\n").unwrap();
    std::fs::create_dir(root.join("sub dir")).unwrap();
    std::fs::write(root.join("sub dir/na\u{ef}ve fil\u{e9}.txt"), "u\n").unwrap();
    std::fs::write(root.join(".env"), "A=1\n").unwrap();
    std::fs::write(root.join(".env.local"), "A=2\n").unwrap();
    std::fs::write(root.join("leak.txt"), format!("token = {}\n", secret())).unwrap();

    let rig = rig(1_000_000);
    let result = take(&dir, &rig.cfg);
    assert_eq!(result.outcome, CaptureOutcome::Captured, "{result:?}");
    let commit = result.commit.clone().unwrap();
    let names = tree_names(root, &commit);
    for kept in [
        "new.txt",
        "sub dir/na\u{ef}ve fil\u{e9}.txt",
        ".gitignore",
        "tracked.txt",
    ] {
        assert!(names.contains(&kept.to_string()), "{kept} in {names:?}");
    }
    for left_out in ["ignored.log", ".env", ".env.local", "leak.txt"] {
        assert!(
            !names.contains(&left_out.to_string()),
            "{left_out} in {names:?}"
        );
    }
    let om = &result.omissions;
    assert_eq!(om.untracked_found, 6);
    assert_eq!(om.untracked_admitted, 3);
    assert_eq!(om.env_files, 2);
    assert_eq!(om.secret_flagged, 1);
    assert!(om.is_partial());
    assert_eq!(om.untracked_omitted(), 3);

    // The same facts are in the commit message and survive a list.
    let message = git_ok(root, &["log", "-1", "--format=%B", &commit]);
    assert!(
        message.contains("GitPulse-Untracked: found=6 admitted=3 env=2 flagged=1"),
        "{message}"
    );
    let listed = list_turn_checkpoints(&path_of(&dir), 5).unwrap();
    assert_eq!(listed.entries[0].omissions.secret_flagged, 1);
    assert_eq!(listed.entries[0].omissions.env_files, 2);
}

#[test]
fn a_scanner_that_fails_admits_no_untracked_file_but_still_captures_tracked_changes() {
    let dir = init();
    let root = dir.path();
    std::fs::write(root.join("tracked.txt"), "edited\n").unwrap();
    std::fs::write(root.join("a.txt"), "a\n").unwrap();
    std::fs::write(root.join("b.txt"), "b\n").unwrap();
    let mut rig = rig(1_000_000);
    rig.cfg.scanner = |_| Err("scanner unavailable".into());
    let result = take(&dir, &rig.cfg);
    assert_eq!(result.outcome, CaptureOutcome::Captured, "{result:?}");
    let names = tree_names(root, &result.commit.clone().unwrap());
    assert_eq!(names, vec!["tracked.txt".to_string()]);
    assert_eq!(result.omissions.wholesale.as_deref(), Some("scan_failed"));
    assert_eq!(result.omissions.untracked_admitted, 0);
    assert_eq!(result.omissions.untracked_omitted(), 2);
    assert_eq!(
        git_ok(
            root,
            &["show", &format!("{}:tracked.txt", result.commit.unwrap())]
        ),
        "edited"
    );
}

#[test]
fn a_scan_that_outlasts_its_bound_admits_nothing() {
    let dir = init();
    std::fs::write(dir.path().join("a.txt"), "a\n").unwrap();
    std::fs::write(dir.path().join("b.txt"), "b\n").unwrap();
    let mut rig = rig(1_000_000);
    rig.cfg.scan_budget = Duration::ZERO;
    // A zero bound is passed by the time the second file is reached.
    rig.cfg.scanner = |_| Ok(false);
    let result = take(&dir, &rig.cfg);
    assert_eq!(
        result.omissions.wholesale.as_deref(),
        Some("scan_timeout"),
        "{result:?}"
    );
    assert_eq!(result.omissions.untracked_admitted, 0);
}

// ── bounds ───────────────────────────────────────────────────────────────────

#[test]
fn more_untracked_files_than_the_cap_captures_tracked_only_and_records_both_numbers() {
    let dir = init();
    let root = dir.path();
    for i in 0..3 {
        std::fs::write(root.join(format!("u{i}.txt")), "x\n").unwrap();
    }
    let mut rig = rig(1_000_000);
    rig.cfg.max_untracked_files = 2;
    let result = take(&dir, &rig.cfg);
    assert_eq!(result.outcome, CaptureOutcome::Captured);
    assert_eq!(
        tree_names(root, &result.commit.clone().unwrap()),
        ["tracked.txt"]
    );
    let om = &result.omissions;
    assert_eq!(om.wholesale.as_deref(), Some("cap_exceeded"));
    assert_eq!((om.untracked_found, om.cap_files), (3, 2));
    assert!(om.wholesale_detail.as_deref().unwrap().contains('3'));
}

#[test]
fn more_untracked_bytes_than_the_cap_captures_tracked_only_and_records_both_numbers() {
    let dir = init();
    let root = dir.path();
    std::fs::write(root.join("a.bin"), [b'a'; 8]).unwrap();
    std::fs::write(root.join("b.bin"), [b'b'; 8]).unwrap();
    let mut rig = rig(1_000_000);
    rig.cfg.max_untracked_bytes = 10;
    let result = take(&dir, &rig.cfg);
    assert_eq!(
        tree_names(root, &result.commit.clone().unwrap()),
        ["tracked.txt"]
    );
    let om = &result.omissions;
    assert_eq!(om.wholesale.as_deref(), Some("cap_exceeded"));
    assert_eq!(om.cap_bytes, 10);
    assert!(om.found_bytes >= 16, "{om:?}");
}

// ── no-ops, skips and failures ───────────────────────────────────────────────

#[test]
fn an_unchanged_tree_writes_nothing_and_a_changed_one_writes_a_new_checkpoint() {
    let dir = init();
    let rig = rig(1_000_000);
    let first = take(&dir, &rig.cfg);
    assert_eq!(first.outcome, CaptureOutcome::Captured);
    let refs = refs_under(dir.path(), TURN_REF_PREFIX);
    let objects_before = git_ok(dir.path(), &["count-objects", "-v"]);

    let second = take(&dir, &rig.cfg);
    assert_eq!(second.outcome, CaptureOutcome::Unchanged, "{second:?}");
    assert_eq!(second.commit, first.commit);
    assert_eq!(refs_under(dir.path(), TURN_REF_PREFIX), refs);
    assert_eq!(git_ok(dir.path(), &["count-objects", "-v"]), objects_before);

    std::fs::write(dir.path().join("tracked.txt"), "changed\n").unwrap();
    let third = take(&dir, &rig.cfg);
    assert_eq!(third.outcome, CaptureOutcome::Captured);
    assert_eq!(refs_under(dir.path(), TURN_REF_PREFIX).len(), 2);
}

#[test]
fn a_new_head_with_the_same_tree_is_a_new_checkpoint() {
    let dir = init();
    let rig = rig(1_000_000);
    assert_eq!(take(&dir, &rig.cfg).outcome, CaptureOutcome::Captured);
    git_ok(
        dir.path(),
        &["commit", "-q", "--allow-empty", "-m", "empty"],
    );
    assert_eq!(take(&dir, &rig.cfg).outcome, CaptureOutcome::Captured);
}

#[test]
fn an_unborn_branch_is_skipped_with_a_reason() {
    let dir = tempfile::TempDir::new().unwrap();
    git_ok(dir.path(), &["init", "-q", "-b", "main"]);
    crate::test_support::trust_repo(dir.path());
    std::fs::write(dir.path().join("a.txt"), "a\n").unwrap();
    let rig = rig(1_000_000);
    let result = take(&dir, &rig.cfg);
    assert_eq!(result.outcome, CaptureOutcome::Skipped);
    assert!(result.reason.unwrap().contains("unborn"));
    assert!(refs_under(dir.path(), TURN_REF_PREFIX).is_empty());
    assert!(dir_is_empty(rig.index_dir.path()));
}

#[test]
fn a_parked_merge_is_skipped_and_the_conflicted_file_is_not_staged_by_the_capture() {
    let dir = init();
    let root = dir.path();
    git_ok(root, &["checkout", "-q", "-b", "other"]);
    std::fs::write(root.join("tracked.txt"), "other\n").unwrap();
    git_ok(root, &["commit", "-q", "-am", "other"]);
    git_ok(root, &["checkout", "-q", "main"]);
    std::fs::write(root.join("tracked.txt"), "main\n").unwrap();
    git_ok(root, &["commit", "-q", "-am", "main"]);
    assert!(
        !git(root, &["merge", "other"]).status.success(),
        "a conflict is the point"
    );
    let index_before = std::fs::read(root.join(".git/index")).unwrap();

    let rig = rig(1_000_000);
    let result = take(&dir, &rig.cfg);
    assert_eq!(result.outcome, CaptureOutcome::Skipped, "{result:?}");
    assert!(result.reason.unwrap().contains("unmerged"));
    assert!(refs_under(root, TURN_REF_PREFIX).is_empty());
    assert_eq!(
        std::fs::read(root.join(".git/index")).unwrap(),
        index_before
    );
    assert!(dir_is_empty(rig.index_dir.path()));
}

#[test]
fn the_private_index_is_removed_after_success_and_after_an_injected_failure() {
    let dir = init();
    let mut rig = rig(1_000_000);
    assert_eq!(take(&dir, &rig.cfg).outcome, CaptureOutcome::Captured);
    assert!(dir_is_empty(rig.index_dir.path()), "after success");

    std::fs::write(dir.path().join("tracked.txt"), "changed\n").unwrap();
    rig.cfg.inject_failure = Some("after_tracked");
    let failed = take(&dir, &rig.cfg);
    assert_eq!(failed.outcome, CaptureOutcome::Failed, "{failed:?}");
    assert!(failed.reason.unwrap().contains("injected"));
    assert!(dir_is_empty(rig.index_dir.path()), "after failure");
    assert_eq!(
        refs_under(dir.path(), TURN_REF_PREFIX).len(),
        1,
        "a failure writes no ref"
    );
}

#[test]
fn the_private_index_is_removed_when_a_panic_unwinds_through_it() {
    let index_dir = tempfile::TempDir::new().unwrap();
    let caught = std::panic::catch_unwind(|| {
        let temp = TempIndex::create(Some(index_dir.path())).unwrap();
        assert!(temp.path.exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&temp.path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        panic!("unwind");
    });
    assert!(caught.is_err());
    assert!(dir_is_empty(index_dir.path()));
}

#[test]
fn the_private_index_carries_the_real_indexs_modified_time() {
    let dir = init();
    let real = dir.path().join(".git/index");
    // A time far from "now", so a copy stamped at copy time cannot match by luck.
    let stamp = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1_500_000_000);
    std::fs::OpenOptions::new()
        .write(true)
        .open(&real)
        .unwrap()
        .set_modified(stamp)
        .unwrap();
    let index_dir = tempfile::TempDir::new().unwrap();
    let temp = TempIndex::create(Some(index_dir.path())).unwrap();
    assert!(temp.seed_from(&real).unwrap());
    let seeded = std::fs::metadata(&temp.path).unwrap().modified().unwrap();
    assert_eq!(seeded, stamp, "the copy keeps the source's modified time");
    assert_eq!(
        std::fs::read(&temp.path).unwrap(),
        std::fs::read(&real).unwrap(),
        "and its bytes"
    );
}

#[test]
fn untracked_files_named_env_anything_are_omitted_and_counted() {
    let dir = init();
    let root = dir.path();
    for name in [".envrc", ".env-local", ".env_prod", ".env", ".env.local"] {
        std::fs::write(root.join(name), "A=1\n").unwrap();
    }
    std::fs::create_dir(root.join("sub")).unwrap();
    std::fs::write(root.join("sub/.envrc"), "A=2\n").unwrap();
    std::fs::write(root.join("environment.txt"), "not an env file\n").unwrap();
    let rig = rig(1_000_000);
    let result = take(&dir, &rig.cfg);
    assert_eq!(result.outcome, CaptureOutcome::Captured, "{result:?}");
    let names = tree_names(root, &result.commit.unwrap());
    for left_out in [
        ".envrc",
        ".env-local",
        ".env_prod",
        ".env",
        ".env.local",
        "sub/.envrc",
    ] {
        assert!(!names.contains(&left_out.to_string()), "{names:?}");
    }
    assert!(names.contains(&"environment.txt".to_string()), "{names:?}");
    assert_eq!(result.omissions.env_files, 6, "{:?}", result.omissions);
}

#[test]
fn a_worktree_with_no_index_file_is_seeded_from_head_not_from_nothing() {
    let dir = init();
    let root = dir.path();
    std::fs::remove_file(root.join(".git/index")).unwrap();
    std::fs::write(root.join("new.txt"), "n\n").unwrap();
    let rig = rig(1_000_000);
    let result = take(&dir, &rig.cfg);
    assert_eq!(result.outcome, CaptureOutcome::Captured, "{result:?}");
    let names = tree_names(root, &result.commit.unwrap());
    assert!(names.contains(&"tracked.txt".to_string()), "{names:?}");
    assert!(names.contains(&"new.txt".to_string()));
    assert!(
        !root.join(".git/index").exists(),
        "the real index is not recreated"
    );
    assert!(dir_is_empty(rig.index_dir.path()));
}

#[test]
fn a_second_request_while_one_runs_is_coalesced_and_the_first_still_finishes() {
    let dir = init();
    let canonical = std::fs::canonicalize(dir.path()).unwrap();
    let rig = rig(1_000_000);
    let held = CaptureGuard::try_acquire(&canonical).expect("free");
    assert!(CaptureGuard::try_acquire(&canonical).is_none());
    let coalesced = take(&dir, &rig.cfg);
    assert_eq!(coalesced.outcome, CaptureOutcome::Coalesced);
    assert!(refs_under(dir.path(), TURN_REF_PREFIX).is_empty());
    drop(held);
    assert_eq!(take(&dir, &rig.cfg).outcome, CaptureOutcome::Captured);
}

// ── refs, keys, pruning ──────────────────────────────────────────────────────

#[test]
fn the_ref_is_create_only() {
    let dir = init();
    let root = dir.path();
    let head = git_ok(root, &["rev-parse", "HEAD"]);
    let other = git_ok(
        root,
        &["commit-tree", "-p", &head, "-m", "x", "HEAD^{tree}"],
    );
    let ref_name = format!("{TURN_REF_PREFIX}0123456789abcdef/0000000000000001");
    let absent = absent_oid_for_len(head.len());
    create_ref_exclusive(root, &ref_name, &head, absent).unwrap();
    assert!(create_ref_exclusive(root, &ref_name, &other, absent).is_err());
    assert_eq!(
        git_ok(root, &["rev-parse", &ref_name]),
        head,
        "the first writer survives"
    );
}

#[test]
fn stamps_rise_even_when_the_clock_does_not() {
    let dir = init();
    let rig = rig(5_000_000);
    let first = take(&dir, &rig.cfg);
    std::fs::write(dir.path().join("tracked.txt"), "2\n").unwrap();
    let second = take(&dir, &rig.cfg);
    std::fs::write(dir.path().join("tracked.txt"), "3\n").unwrap();
    let mut earlier = rig.cfg.clone();
    earlier.now_ms = 1;
    let third = take(&dir, &earlier);
    let ids = [first.id.unwrap(), second.id.unwrap(), third.id.unwrap()];
    assert!(ids[0] < ids[1] && ids[1] < ids[2], "{ids:?}");
    assert!(ids
        .iter()
        .all(|id| id.len() == STAMP_DIGITS && valid_stamp(id)));
}

#[test]
fn pruning_keeps_the_newest_n_per_worktree() {
    let dir = init();
    let mut rig = rig(0);
    rig.cfg.max_keep = 3;
    let mut ids = Vec::new();
    for i in 0..5u64 {
        std::fs::write(dir.path().join("tracked.txt"), format!("v{i}\n")).unwrap();
        rig.cfg.now_ms = 10_000_000 + i * 1000;
        let result = take(&dir, &rig.cfg);
        assert_eq!(result.outcome, CaptureOutcome::Captured, "{result:?}");
        ids.push(result.id.unwrap());
    }
    let kept: Vec<String> = refs_under(dir.path(), TURN_REF_PREFIX)
        .into_iter()
        .map(|r| r.rsplit('/').next().unwrap().to_string())
        .collect();
    assert_eq!(kept.len(), 3, "{kept:?}");
    for id in &ids[2..] {
        assert!(kept.contains(id), "{id} newest must stay: {kept:?}");
    }
}

#[test]
fn pruning_drops_checkpoints_older_than_the_expiry_by_the_injected_clock() {
    let dir = init();
    let mut rig = rig(0);
    rig.cfg.expiry_secs = 100;
    rig.cfg.now_ms = 1_000_000;
    let old = take(&dir, &rig.cfg);
    std::fs::write(dir.path().join("tracked.txt"), "later\n").unwrap();
    rig.cfg.now_ms = 1_000_000 + 99_000;
    let young = take(&dir, &rig.cfg);
    assert_eq!(
        refs_under(dir.path(), TURN_REF_PREFIX).len(),
        2,
        "99 s old is kept"
    );
    std::fs::write(dir.path().join("tracked.txt"), "latest\n").unwrap();
    rig.cfg.now_ms = 1_000_000 + 200_000;
    let newest = take(&dir, &rig.cfg);
    assert_eq!(newest.pruned, 2, "{newest:?}");
    let left = refs_under(dir.path(), TURN_REF_PREFIX);
    assert_eq!(left, vec![newest.ref_name.unwrap()]);
    assert_ne!(old.id, young.id);
}

fn linked(dir: &tempfile::TempDir) -> tempfile::TempDir {
    let parent = tempfile::TempDir::new().unwrap();
    let path = parent.path().join("wt");
    git_ok(
        dir.path(),
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "other",
            path.to_str().unwrap(),
        ],
    );
    crate::test_support::trust_repo(&path);
    parent
}

#[test]
fn two_worktrees_of_one_repository_never_read_each_others_checkpoints() {
    let main = init();
    let parent = linked(&main);
    let wt = parent.path().join("wt");
    let rig = rig(1_000_000);

    let a = take(&main, &rig.cfg);
    std::fs::write(wt.join("only-in-wt.txt"), "w\n").unwrap();
    let b = capture_turn_checkpoint(
        wt.to_str().unwrap(),
        &CaptureRequest {
            event: TurnEvent::TurnEnd,
            session_id: None,
            label: None,
        },
        &rig.cfg,
    );
    assert_eq!(a.outcome, CaptureOutcome::Captured);
    assert_eq!(b.outcome, CaptureOutcome::Captured, "{b:?}");
    assert_ne!(a.key, b.key);
    assert_eq!(refs_under(main.path(), TURN_REF_PREFIX).len(), 2);

    let in_main = list_turn_checkpoints(&path_of(&main), 10).unwrap();
    let in_wt = list_turn_checkpoints(wt.to_str().unwrap(), 10).unwrap();
    assert_eq!(in_main.entries.len(), 1);
    assert_eq!(in_wt.entries.len(), 1);
    assert_eq!(in_main.entries[0].id, a.id.clone().unwrap());
    assert_ne!(in_main.key, in_wt.key);
    // An id from the other worktree resolves to nothing here.
    let other_id = in_wt.entries[0].id.clone();
    let main_id = in_main.entries[0].id.clone();
    if other_id != main_id {
        assert!(diff_turn_checkpoint(&path_of(&main), &other_id, None).is_err());
    }
    assert!(
        diff_turn_checkpoint(wt.to_str().unwrap(), &main_id, None).is_err() || other_id == main_id
    );
}

#[test]
fn a_detached_head_has_a_key_of_its_own() {
    let dir = init();
    let rig = rig(1_000_000);
    let on_branch = take(&dir, &rig.cfg);
    git_ok(dir.path(), &["checkout", "-q", "--detach"]);
    assert!(list_turn_checkpoints(&path_of(&dir), 10)
        .unwrap()
        .entries
        .is_empty());
    let detached = take(&dir, &rig.cfg);
    assert_eq!(detached.outcome, CaptureOutcome::Captured, "{detached:?}");
    assert_ne!(detached.key, on_branch.key);
    let listed = list_turn_checkpoints(&path_of(&dir), 10).unwrap();
    assert_eq!(listed.entries.len(), 1);
    assert_eq!(listed.entries[0].branch.as_deref(), Some("detached"));
    assert_eq!(
        worktree_key(None, Path::new("/a/.git")),
        worktree_key(None, Path::new("/a/.git"))
    );
    assert_ne!(
        worktree_key(None, Path::new("/a/.git")),
        worktree_key(None, Path::new("/b/.git"))
    );
    assert_ne!(
        worktree_key(Some("refs/heads/x"), Path::new("/a/.git")),
        worktree_key(Some("refs/heads/y"), Path::new("/a/.git"))
    );
}

// ── label, session, list, diff ───────────────────────────────────────────────

#[test]
fn labels_are_redacted_one_line_and_bounded() {
    assert_eq!(sanitize_label(""), "");
    assert_eq!(sanitize_label("  \n\t "), "");
    let messy = sanitize_label("a\u{1b}[31m\nb\tc\u{0}\u{202e}d\r\ne");
    assert!(!messy.chars().any(char::is_control), "{messy:?}");
    assert!(!messy.contains('\n'));
    let long = sanitize_label(&"x".repeat(100_000));
    assert_eq!(long.chars().count(), MAX_LABEL_CHARS);
    let multibyte = sanitize_label(&"\u{e9}".repeat(100_000));
    assert_eq!(multibyte.chars().count(), MAX_LABEL_CHARS);
    let with_secret = sanitize_label(&format!("use {} now", secret()));
    assert!(!with_secret.contains(&secret()), "{with_secret}");
}

#[test]
fn session_ids_with_odd_characters_are_reduced_to_a_safe_alphabet() {
    assert_eq!(sanitize_session(None), "none");
    assert_eq!(sanitize_session(Some("   ")), "none");
    let odd = sanitize_session(Some("a b/../\n\u{202e}x;$(id)"));
    assert!(odd
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "._:-".contains(c) || c == '_'));
    assert_eq!(
        sanitize_session(Some(&"s".repeat(500))).len(),
        MAX_SESSION_CHARS
    );
    assert_eq!(sanitize_session(Some("abc-123_x.y:z")), "abc-123_x.y:z");
}

#[test]
fn a_hostile_label_and_session_cannot_forge_a_trailer() {
    let dir = init();
    let rig = rig(1_000_000);
    let result = take_as(
        &dir,
        &rig.cfg,
        TurnEvent::TurnStart,
        Some("s\nGitPulse-Event: turn_end"),
        Some("x\n\nGitPulse-Event: turn_end\nGitPulse-Untracked: found=999"),
    );
    assert_eq!(result.outcome, CaptureOutcome::Captured);
    let listed = list_turn_checkpoints(&path_of(&dir), 5).unwrap();
    let entry = &listed.entries[0];
    assert_eq!(entry.event.as_deref(), Some("turn_start"));
    assert_eq!(entry.omissions.untracked_found, 0);
    let session = entry.session_id.clone().unwrap();
    assert!(!session.contains('\n'), "{session}");
    let message = git_ok(
        dir.path(),
        &["log", "-1", "--format=%B", &result.commit.unwrap()],
    );
    assert_eq!(
        message
            .lines()
            .filter(|l| l.starts_with("GitPulse-Event:"))
            .count(),
        1,
        "{message}"
    );
}

#[test]
fn an_empty_and_an_absent_label_still_make_a_checkpoint() {
    let dir = init();
    let rig = rig(1_000_000);
    let a = take_as(&dir, &rig.cfg, TurnEvent::TurnEnd, None, Some(""));
    assert_eq!(a.outcome, CaptureOutcome::Captured);
    let listed = list_turn_checkpoints(&path_of(&dir), 5).unwrap();
    assert_eq!(listed.entries[0].label, "");
    assert_eq!(listed.entries[0].event.as_deref(), Some("turn_end"));
    assert_eq!(listed.entries[0].session_id.as_deref(), Some("none"));
}

#[test]
fn list_is_newest_first_with_stats_and_says_when_it_was_cut() {
    let dir = init();
    let mut rig = rig(0);
    for i in 0..3u64 {
        std::fs::write(dir.path().join("tracked.txt"), format!("v{i}\nline\n")).unwrap();
        std::fs::write(dir.path().join(format!("n{i}.txt")), "n\n").unwrap();
        rig.cfg.now_ms = 20_000_000 + i * 1000;
        assert_eq!(take(&dir, &rig.cfg).outcome, CaptureOutcome::Captured);
    }
    let all = list_turn_checkpoints(&path_of(&dir), 10).unwrap();
    assert_eq!(all.total, 3);
    assert!(!all.truncated);
    assert!(all.entries[0].id > all.entries[1].id && all.entries[1].id > all.entries[2].id);
    // Oldest: against its parent commit (tracked.txt edited, n0.txt added).
    assert_eq!(all.entries[2].files, 2);
    assert!(all.entries[2].additions >= 3);
    // Newer ones are against the previous checkpoint: one new file, one edit.
    assert_eq!(all.entries[0].files, 2);
    assert_eq!(all.entries[0].created_at, 20_002);
    assert!(all.entries.iter().all(|e| e.stats_complete));

    let cut = list_turn_checkpoints(&path_of(&dir), 2).unwrap();
    assert_eq!(cut.entries.len(), 2);
    assert_eq!(cut.total, 3);
    assert!(cut.truncated);
    assert_eq!(
        cut.entries[1].files, all.entries[1].files,
        "the baseline sits past the page"
    );
    assert_eq!(
        list_turn_checkpoints(&path_of(&dir), 0)
            .unwrap()
            .entries
            .len(),
        1
    );
    assert!(
        list_turn_checkpoints(&path_of(&dir), 10_000)
            .unwrap()
            .entries
            .len()
            <= MAX_LIST_LIMIT
    );
}

#[test]
fn diff_shows_the_checkpoint_against_its_parent_or_against_another_checkpoint() {
    let dir = init();
    let mut rig = rig(30_000_000);
    std::fs::write(dir.path().join("tracked.txt"), "first\n").unwrap();
    let first = take(&dir, &rig.cfg);
    std::fs::write(dir.path().join("tracked.txt"), "second\n").unwrap();
    rig.cfg.now_ms += 1000;
    let second = take(&dir, &rig.cfg);

    let to_parent =
        diff_turn_checkpoint(&path_of(&dir), &second.id.clone().unwrap(), None).unwrap();
    assert!(to_parent.patch.contains("+second"), "{}", to_parent.patch);
    assert!(to_parent.patch.contains("-base"));
    assert!(!to_parent.truncated);

    let between = diff_turn_checkpoint(
        &path_of(&dir),
        &second.id.unwrap(),
        Some(&first.id.unwrap()),
    )
    .unwrap();
    assert!(between.patch.contains("-first") && between.patch.contains("+second"));
    assert!(!between.patch.contains("-base"));

    for bad in [
        "",
        "abc",
        "../x",
        "99999999999999999999999",
        "0000000000000001",
    ] {
        assert!(
            diff_turn_checkpoint(&path_of(&dir), bad, None).is_err(),
            "{bad:?}"
        );
    }
}

#[test]
fn diff_output_is_bounded_and_redacted() {
    let dir = init();
    let rig = rig(40_000_000);
    let big: String = (0..40_000).map(|i| format!("line number {i}\n")).collect();
    assert!(big.len() > MAX_DIFF_BYTES);
    std::fs::write(
        dir.path().join("tracked.txt"),
        format!("{big}key = {}\n", secret()),
    )
    .unwrap();
    let taken = take(&dir, &rig.cfg);
    let diff = diff_turn_checkpoint(&path_of(&dir), &taken.id.unwrap(), None).unwrap();
    assert!(diff.truncated);
    assert!(
        diff.patch.len() <= MAX_DIFF_BYTES + 64,
        "{}",
        diff.patch.len()
    );

    let dir2 = init();
    std::fs::write(
        dir2.path().join("tracked.txt"),
        format!("key = {}\n", secret()),
    )
    .unwrap();
    let taken = take(&dir2, &rig.cfg);
    let diff = diff_turn_checkpoint(&path_of(&dir2), &taken.id.unwrap(), None).unwrap();
    assert!(diff.redacted);
    assert!(!diff.patch.contains(&secret()));
}

// ── adversarial worktrees ────────────────────────────────────────────────────

#[cfg(unix)]
#[test]
fn a_fifo_a_symlink_an_unreadable_and_a_huge_file_among_the_untracked_do_not_stall_or_leak() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let dir = init();
    let root = dir.path();
    let fifo = std::ffi::CString::new(root.join("pipe").to_str().unwrap()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    symlink("tracked.txt", root.join("link")).unwrap();
    symlink("/definitely/not/here", root.join("dangling")).unwrap();
    std::fs::write(
        root.join("big.bin"),
        vec![0u8; (MAX_SCAN_FILE_BYTES + 1) as usize],
    )
    .unwrap();
    std::fs::write(root.join("locked.txt"), "x\n").unwrap();
    std::fs::set_permissions(
        root.join("locked.txt"),
        std::fs::Permissions::from_mode(0o000),
    )
    .unwrap();
    std::fs::write(root.join("fine.txt"), "ok\n").unwrap();
    // A name with every awkward byte git allows.
    std::fs::write(root.join("sp ace \u{201c}q\u{201d} \u{4e2d}.txt"), "z\n").unwrap();

    let rig = rig(1_000_000);
    let path = path_of(&dir);
    let cfg = rig.cfg.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let result = capture_turn_checkpoint(
            &path,
            &CaptureRequest {
                event: TurnEvent::TurnEnd,
                session_id: None,
                label: None,
            },
            &cfg,
        );
        let _ = tx.send(result);
    });
    let result = rx
        .recv_timeout(Duration::from_secs(60))
        .expect("a FIFO among the untracked files must not stall the capture");
    std::fs::set_permissions(
        root.join("locked.txt"),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    assert_eq!(result.outcome, CaptureOutcome::Captured, "{result:?}");

    let names = tree_names(root, &result.commit.clone().unwrap());
    for kept in [
        "fine.txt",
        "link",
        "dangling",
        "sp ace \u{201c}q\u{201d} \u{4e2d}.txt",
    ] {
        assert!(names.contains(&kept.to_string()), "{kept} in {names:?}");
    }
    assert!(!names.contains(&"pipe".to_string()));
    assert!(
        !names.contains(&"big.bin".to_string()),
        "too large to scan: omitted"
    );
    let mode = git_ok(root, &["ls-tree", &result.commit.unwrap(), "link"]);
    assert!(
        mode.starts_with("120000"),
        "a symlink is stored as a link: {mode}"
    );
    // big.bin is too large to scan; the FIFO always, and locked.txt unless the
    // tests run as root, are not readable regular files.
    assert_eq!(result.omissions.too_large, 1, "{:?}", result.omissions);
    assert!(
        result.omissions.unreadable_or_special >= 1,
        "{:?}",
        result.omissions
    );
    assert!(dir_is_empty(rig.index_dir.path()));
}

#[test]
fn an_untracked_file_that_vanishes_between_the_scan_and_the_staging_does_not_fail_the_capture() {
    let dir = init();
    let root = dir.path().to_path_buf();
    std::fs::write(root.join("keep.txt"), "k\n").unwrap();
    std::fs::write(root.join("vanish.txt"), "v\n").unwrap();
    let mut rig = rig(1_000_000);
    // The scanner runs after the listing and before the staging.
    static VANISH: OnceLock<PathBuf> = OnceLock::new();
    VANISH.get_or_init(|| root.join("vanish.txt"));
    rig.cfg.scanner = |_| {
        if let Some(path) = VANISH.get() {
            let _ = std::fs::remove_file(path);
        }
        Ok(false)
    };
    let result = take(&dir, &rig.cfg);
    assert_eq!(result.outcome, CaptureOutcome::Captured, "{result:?}");
    let names = tree_names(&root, &result.commit.unwrap());
    assert!(names.contains(&"keep.txt".to_string()));
    assert!(!names.contains(&"vanish.txt".to_string()));
}

#[test]
fn gitpulse_state_nested_repos_and_symlinked_ignores_are_counted_not_captured() {
    let dir = init();
    let root = dir.path();
    std::fs::create_dir(root.join(".devcouncil")).unwrap();
    std::fs::write(root.join(".devcouncil/ledger.sqlite"), "db").unwrap();
    std::fs::create_dir(root.join("nested")).unwrap();
    git_ok(&root.join("nested"), &["init", "-q"]);
    std::fs::write(root.join("nested/f.txt"), "f\n").unwrap();
    let rig = rig(1_000_000);
    let result = take(&dir, &rig.cfg);
    assert_eq!(result.omissions.tool_state, 1, "{:?}", result.omissions);
    assert_eq!(result.omissions.nested_repos, 1);
    let names = tree_names(root, &result.commit.unwrap());
    assert_eq!(names, vec!["tracked.txt".to_string()]);
}

// ── the app handler and the mailbox ──────────────────────────────────────────

fn request(root: &Path, event: &str) -> crate::desktop::instance::Request {
    crate::desktop::instance::Request::RecordTurnCheckpoint {
        id: "turn-test-1".into(),
        cwd: root.to_str().unwrap().into(),
        session_id: Some("sess-9".into()),
        event: event.into(),
        label: Some("a label".into()),
    }
}

/// The handler with the setting on, without reading the developer's real
/// `tools.json`.
fn serve_on(request: &crate::desktop::instance::Request) -> Result<serde_json::Value, String> {
    serve_turn_checkpoint_with(request, &|| true)
}

#[test]
fn the_app_refuses_to_capture_when_the_setting_is_off() {
    let dir = init();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    std::fs::write(root.join("new.txt"), "n\n").unwrap();
    for event in ["turn_start", "turn_end"] {
        let reply = serve_turn_checkpoint_with(&request(&root, event), &|| false).unwrap();
        assert_eq!(reply["outcome"], "skipped", "{reply}");
        assert!(
            reply["reason"]
                .as_str()
                .unwrap()
                .contains("turn checkpoints are off"),
            "{reply}"
        );
        assert!(reply["commit"].is_null(), "{reply}");
    }
    assert!(
        refs_under(&root, TURN_REF_PREFIX).is_empty(),
        "no ref is written while the setting is off"
    );
    let rows = crate::ledger::tail(root.to_str().unwrap(), 0, 100)
        .unwrap()
        .into_iter()
        .filter(|e| e.action == crate::ledger::TURN_CHECKPOINT_ACTION)
        .count();
    assert_eq!(rows, 0, "an off setting leaves no ledger row either");

    // No git process: a directory that is not a repository is answered
    // `skipped` for being off, not failed for being unreadable by git.
    let nowhere = tempfile::TempDir::new().unwrap();
    let nowhere_root = std::fs::canonicalize(nowhere.path()).unwrap();
    let reply =
        serve_turn_checkpoint_with(&request(&nowhere_root, "turn_start"), &|| false).unwrap();
    assert_eq!(reply["outcome"], "skipped");
    assert!(reply["reason"].as_str().unwrap().contains("are off"));

    // The same request with the setting on is served.
    let reply = serve_turn_checkpoint_with(&request(&root, "turn_start"), &|| true).unwrap();
    assert_eq!(reply["outcome"], "captured", "{reply}");
}

#[test]
fn every_attempt_that_reaches_the_app_leaves_one_ledger_row_with_its_outcome() {
    let dir = init();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    std::fs::write(root.join("new.txt"), "n\n").unwrap();
    let reply = serve_on(&request(&root, "turn_start")).unwrap();
    assert_eq!(reply["outcome"], "captured");
    let again = serve_on(&request(&root, "turn_end")).unwrap();
    assert_eq!(again["outcome"], "unchanged");

    let unborn = tempfile::TempDir::new().unwrap();
    git_ok(unborn.path(), &["init", "-q", "-b", "main"]);
    crate::test_support::trust_repo(unborn.path());
    let unborn_root = std::fs::canonicalize(unborn.path()).unwrap();
    let skipped = serve_on(&request(&unborn_root, "turn_start")).unwrap();
    assert_eq!(skipped["outcome"], "skipped");

    let rows = |repo: &Path| -> Vec<crate::ledger::LedgerEvent> {
        crate::ledger::tail(repo.to_str().unwrap(), 0, 100)
            .unwrap()
            .into_iter()
            .filter(|e| e.action == crate::ledger::TURN_CHECKPOINT_ACTION)
            .collect()
    };
    let rows_main = rows(&root);
    assert_eq!(rows_main.len(), 2, "{rows_main:?}");
    assert_eq!(rows_main[0].outcome, "ok");
    assert_eq!(rows_main[0].session_id.as_deref(), Some("sess-9"));
    assert!(rows_main[0]
        .after_ref
        .as_deref()
        .unwrap()
        .starts_with(TURN_REF_PREFIX));
    let detail: serde_json::Value =
        serde_json::from_str(rows_main[0].detail_json.as_deref().unwrap()).unwrap();
    assert_eq!(detail["checkpoint"], "captured");
    assert_eq!(detail["omissions"]["untracked_admitted"], 1);
    let unchanged: serde_json::Value =
        serde_json::from_str(rows_main[1].detail_json.as_deref().unwrap()).unwrap();
    assert_eq!(unchanged["checkpoint"], "unchanged");

    let rows_unborn = rows(&unborn_root);
    assert_eq!(rows_unborn.len(), 1);
    assert_eq!(
        rows_unborn[0].outcome, "failed",
        "a skipped attempt never reads as ok"
    );
    let skipped_detail: serde_json::Value =
        serde_json::from_str(rows_unborn[0].detail_json.as_deref().unwrap()).unwrap();
    assert_eq!(skipped_detail["checkpoint"], "skipped");
    assert!(skipped_detail["reason"]
        .as_str()
        .unwrap()
        .contains("unborn"));
}

#[test]
fn an_unknown_event_is_refused_by_the_handler() {
    let dir = init();
    assert!(serve_on(&request(dir.path(), "turn_middle")).is_err());
    assert!(request(dir.path(), "turn_middle").validate().is_err());
    assert!(request(dir.path(), "turn_end").validate().is_ok());
    let relative = crate::desktop::instance::Request::RecordTurnCheckpoint {
        id: "x".into(),
        cwd: "relative/path".into(),
        session_id: None,
        event: "turn_end".into(),
        label: None,
    };
    assert!(relative.validate().is_err());
    let oversized = crate::desktop::instance::Request::RecordTurnCheckpoint {
        id: "x".into(),
        cwd: dir.path().to_str().unwrap().into(),
        session_id: None,
        event: "turn_end".into(),
        label: Some("l".repeat(100_000)),
    };
    assert!(oversized.validate().is_err());
}

#[cfg(target_os = "macos")]
mod mailbox {
    use super::*;
    use crate::desktop::instance::{claim, reach_owner, Claim, SOCKET_FILE};
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;
    use std::sync::Arc;

    fn primary(profile: &Path) -> crate::desktop::instance::Primary {
        match claim(profile, Duration::from_secs(2)) {
            Claim::Primary(primary) => primary,
            _ => panic!("expected to own the mailbox"),
        }
    }

    #[test]
    fn the_owner_answers_a_request_on_its_connection_and_coalesces_a_second() {
        let profile = tempfile::tempdir().unwrap();
        let owner = primary(profile.path());
        owner.attach_turn_checkpointer(Arc::new(serve_on));
        let dir = init();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let reply = reach_owner(profile.path(), Duration::from_secs(2))
            .expect("owner")
            .record_turn_checkpoint(&request(&root, "turn_start"), Duration::from_secs(30))
            .unwrap();
        assert_eq!(reply["outcome"], "captured", "{reply}");

        let held = CaptureGuard::try_acquire(&root).unwrap();
        let reply = reach_owner(profile.path(), Duration::from_secs(2))
            .expect("owner")
            .record_turn_checkpoint(&request(&root, "turn_end"), Duration::from_secs(30))
            .unwrap();
        assert_eq!(reply["outcome"], "coalesced", "{reply}");
        drop(held);
        assert_eq!(refs_under(dir.path(), TURN_REF_PREFIX).len(), 1);
    }

    #[test]
    fn an_owner_without_the_sink_refuses_cleanly() {
        let profile = tempfile::tempdir().unwrap();
        let _owner = primary(profile.path());
        let dir = init();
        let started = std::time::Instant::now();
        let err = reach_owner(profile.path(), Duration::from_secs(2))
            .expect("owner")
            .record_turn_checkpoint(&request(dir.path(), "turn_start"), Duration::from_secs(10))
            .unwrap_err();
        assert!(err.contains("not attached"), "{err}");
        assert!(started.elapsed() < crate::test_support::coverage_relaxed(Duration::from_secs(5)));
    }

    /// The other mixed-version direction: an owner that does not know the
    /// request's `kind` (an older app) refuses the frame with an answer, does
    /// not hang, and keeps serving what it does know.
    #[test]
    fn an_owner_that_does_not_know_the_kind_refuses_without_hanging_or_dying() {
        let profile = tempfile::tempdir().unwrap();
        let owner = primary(profile.path());
        owner.attach_turn_checkpointer(Arc::new(serve_on));
        let mut stream = UnixStream::connect(profile.path().join(SOCKET_FILE)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut hello = String::new();
        reader.read_line(&mut hello).unwrap();
        let version = serde_json::from_str::<serde_json::Value>(&hello).unwrap()["v"].clone();
        let frame = serde_json::json!({
            "v": version,
            "request": {"kind": "record_turn_checkpoint_from_the_future", "id": "x"}
        });
        writeln!(stream, "{frame}").unwrap();
        let mut ack = String::new();
        reader.read_line(&mut ack).unwrap();
        let ack: serde_json::Value = serde_json::from_str(&ack).unwrap();
        assert_eq!(ack["accepted"], false, "{ack}");
        assert!(ack["reason"]
            .as_str()
            .unwrap()
            .contains("malformed request"));

        let dir = init();
        let reply = reach_owner(profile.path(), Duration::from_secs(2))
            .expect("the owner survives")
            .record_turn_checkpoint(&request(dir.path(), "turn_start"), Duration::from_secs(30))
            .unwrap();
        assert_eq!(reply["outcome"], "captured");
    }
}
