//! Round-3 adversarial review of lane W3-L5: the forget invariant and the resume
//! budget. A cursor may be forgotten only if its transcript is truly gone: a
//! pass walked every place it could be, completely, and did not find it. Every
//! fixture is synthetic. A failing test names a defect; a passing one records a
//! held invariant.

use super::*;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

fn has_cursor(pair: &Pair, path: &Path) -> bool {
    ledger::transcript_progress(&pair.wt[0], &pair.wt[0])
        .unwrap()
        .offsets
        .contains_key(&ledger::transcript_key(path))
}

fn offset_of(pair: &Pair, path: &Path) -> Option<u64> {
    ledger::transcript_progress(&pair.wt[0], &pair.wt[0])
        .unwrap()
        .offsets
        .get(&ledger::transcript_key(path))
        .copied()
}

fn codex_rollout(root: &Path, thread: &str, wt: &str) -> PathBuf {
    let path = rollout_path(root, thread);
    write_lines(&path, &[session_meta(thread, wt), run(1, "ls", wt)]);
    path
}

fn claude_file(root: &Path, dir: &str, session: &str, wt: &str) -> PathBuf {
    let path = root.join(dir).join(format!("{session}.jsonl"));
    write_lines(&path, &[claude_edit(wt, session, 1, &format!("{wt}/f.rs"))]);
    path
}

fn filler(root: &Path, dir: &str, count: usize) {
    let dir = root.join(dir);
    std::fs::create_dir_all(&dir).unwrap();
    for i in 0..count {
        std::fs::write(dir.join(format!("{i}.jsonl")), "").unwrap();
    }
}

fn lock(dir: &Path) {
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o000)).unwrap();
}

fn unlock(dir: &Path) {
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755)).unwrap();
}

// ------------------------------------------------------- 1. one root incomplete

#[test]
fn f01_claude_over_the_file_cap_with_codex_whole_forgets_no_claude_cursor() {
    for codex_shape in ["empty", "missing", "unset"] {
        let pair = Pair::new();
        let w1 = pair.wt[0].clone();
        let claude = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let codex_empty = scratch.path().join("codex");
        std::fs::create_dir_all(&codex_empty).unwrap();
        let codex_missing = scratch.path().join("nowhere");
        let first = claude_file(claude.path(), "0first", "CA", &w1);
        let last = claude_file(claude.path(), "zlast", "CZ", &w1);
        let codex_arg = match codex_shape {
            "empty" => Some(codex_empty.as_path()),
            "missing" => Some(codex_missing.as_path()),
            _ => None,
        };
        with_roots(Some(claude.path()), codex_arg, || pair.pass());
        assert!(has_cursor(&pair, &first) && has_cursor(&pair, &last));
        filler(claude.path(), "mid", MAX_WALK_FILES + 1);
        let out = with_roots(Some(claude.path()), codex_arg, || pair.pass());
        assert_eq!(out.walk_complete, Some(false), "{codex_shape}: {out:?}");
        assert!(
            has_cursor(&pair, &first) && has_cursor(&pair, &last),
            "{codex_shape}: a Claude cursor forgotten behind a capped Claude walk: {out:?}"
        );
    }
}

#[test]
fn f02_claude_with_an_unreadable_subdir_and_codex_whole_forgets_no_claude_cursor() {
    let pair = Pair::new();
    let w1 = pair.wt[0].clone();
    let claude = tempfile::tempdir().unwrap();
    let codex = tempfile::tempdir().unwrap();
    let hidden = claude_file(claude.path(), "locked", "CL", &w1);
    with_roots(Some(claude.path()), Some(codex.path()), || pair.pass());
    assert!(has_cursor(&pair, &hidden));
    lock(&claude.path().join("locked"));
    let out = with_roots(Some(claude.path()), Some(codex.path()), || pair.pass());
    unlock(&claude.path().join("locked"));
    assert!(
        has_cursor(&pair, &hidden),
        "forgotten behind an unreadable subdir: {out:?}"
    );
}

#[test]
fn f03_claude_entry_budget_and_depth_bound_forget_no_claude_cursor() {
    let pair = Pair::new();
    let w1 = pair.wt[0].clone();
    let claude = tempfile::tempdir().unwrap();
    let codex = tempfile::tempdir().unwrap();
    let kept = claude_file(claude.path(), "a", "CD", &w1);
    with_roots(Some(claude.path()), Some(codex.path()), || pair.pass());
    // A deep chain past MAX_WALK_DEPTH and many directories (entry budget).
    let mut deep = claude.path().join("a");
    for i in 0..(MAX_WALK_DEPTH + 3) {
        deep = deep.join(format!("d{i}"));
    }
    std::fs::create_dir_all(&deep).unwrap();
    std::fs::write(deep.join("x.jsonl"), "").unwrap();
    for i in 0..3000 {
        std::fs::create_dir_all(claude.path().join(format!("wide/{i}"))).unwrap();
    }
    let out = with_roots(Some(claude.path()), Some(codex.path()), || pair.pass());
    assert!(has_cursor(&pair, &kept), "{out:?}");
}

#[test]
fn f04_codex_incomplete_with_claude_whole_forgets_no_codex_cursor() {
    let pair = Pair::new();
    let w1 = pair.wt[0].clone();
    let claude = tempfile::tempdir().unwrap();
    let codex = tempfile::tempdir().unwrap();
    let rollout = codex_rollout(codex.path(), "F4", &w1);
    with_roots(Some(claude.path()), Some(codex.path()), || pair.pass());
    assert!(has_cursor(&pair, &rollout));
    lock(&codex.path().join("2026"));
    let out = with_roots(Some(claude.path()), Some(codex.path()), || pair.pass());
    unlock(&codex.path().join("2026"));
    assert!(has_cursor(&pair, &rollout), "unreadable: {out:?}");

    filler(codex.path(), "zz", MAX_WALK_FILES + 1);
    let out = with_roots(Some(claude.path()), Some(codex.path()), || pair.pass());
    assert!(has_cursor(&pair, &rollout), "capped: {out:?}");
    let again = with_roots(Some(claude.path()), Some(codex.path()), || {
        std::fs::remove_dir_all(codex.path().join("zz")).unwrap();
        pair.pass()
    });
    assert_eq!(rows(&w1).len(), 1, "{again:?}");
}

// --------------------------------------------------- 2. reachable via both roots

#[test]
fn g01_a_rollout_reachable_through_both_roots_is_not_replayed_or_forgotten_early() {
    let pair = Pair::new();
    let w1 = pair.wt[0].clone();
    let claude = tempfile::tempdir().unwrap();
    let codex = claude.path().join("nested-codex");
    let rollout = codex_rollout(&codex, "G1", &w1);
    let empty = tempfile::tempdir().unwrap();
    with_roots(Some(claude.path()), Some(&codex), || pair.pass());
    assert!(has_cursor(&pair, &rollout));
    let rows_after_first = rows(&w1).len();
    // Codex moves away: the Claude walk still reaches the file.
    with_roots(Some(claude.path()), Some(empty.path()), || pair.pass());
    with_roots(Some(claude.path()), Some(&codex), || pair.pass());
    assert_eq!(rows(&w1).len(), rows_after_first, "replayed");
    assert!(has_cursor(&pair, &rollout));
    // Claude moves away: the Codex walk still has it.
    with_roots(Some(empty.path()), Some(&codex), || pair.pass());
    with_roots(Some(claude.path()), Some(&codex), || pair.pass());
    assert_eq!(
        rows(&w1).len(),
        rows_after_first,
        "replayed after Claude left"
    );
}

#[test]
fn g02_a_file_renamed_from_the_claude_root_into_the_codex_root_is_not_double_counted() {
    let pair = Pair::new();
    let w1 = pair.wt[0].clone();
    let claude = tempfile::tempdir().unwrap();
    let codex = tempfile::tempdir().unwrap();
    let old = claude.path().join("p/rollout-2026-09-01T12-00-00-G2.jsonl");
    write_lines(&old, &[session_meta("G2", &w1), run(1, "ls", &w1)]);
    with_roots(Some(claude.path()), Some(codex.path()), || pair.pass());
    let before = rows(&w1).len();
    let new = rollout_path(codex.path(), "G2");
    std::fs::create_dir_all(new.parent().unwrap()).unwrap();
    std::fs::rename(&old, &new).unwrap();
    with_roots(Some(claude.path()), Some(codex.path()), || pair.pass());
    with_roots(Some(claude.path()), Some(codex.path()), || pair.pass());
    // The old path is truly gone and the file is new at its new path: its rows
    // are read exactly once, however many passes follow.
    assert_eq!(before, 0, "the Claude source read a Codex rollout");
    assert_eq!(rows(&w1).len(), 1, "read {} times", rows(&w1).len());
}

// ----------------------------------------------------- 3. fingerprint eviction

#[test]
fn h01_nine_roots_over_time_never_forget_the_evicted_roots_cursors() {
    let pair = Pair::new();
    let w1 = pair.wt[0].clone();
    let claude = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let roots: Vec<PathBuf> = (0..9)
        .map(|i| scratch.path().join(format!("r{i}")))
        .collect();
    let rollouts: Vec<PathBuf> = roots
        .iter()
        .enumerate()
        .map(|(i, r)| codex_rollout(r, &format!("H{i}"), &w1))
        .collect();
    for root in &roots {
        with_roots(Some(claude.path()), Some(root), || pair.pass());
    }
    assert_eq!(rows(&w1).len(), 9);
    let empty = scratch.path().join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    // The oldest root is gone, and the remaining passes read other roots, whole.
    std::fs::remove_dir_all(&roots[0]).unwrap();
    for read in [&empty, &roots[8], &roots[3], &empty] {
        let out = with_roots(Some(claude.path()), Some(read), || pair.pass());
        for (i, path) in rollouts.iter().enumerate() {
            assert!(
                has_cursor(&pair, path),
                "cursor of root r{i} forgotten reading {read:?}: {out:?}"
            );
        }
    }
    // It comes back: nothing is replayed.
    codex_rollout(&roots[0], "H0", &w1);
    for root in &roots {
        with_roots(Some(claude.path()), Some(root), || pair.pass());
    }
    assert_eq!(rows(&w1).len(), 9, "rows were replayed");
}

#[test]
fn h02_the_evicted_root_that_still_exists_elsewhere_is_not_forgotten_by_a_whole_walk_of_another() {
    let pair = Pair::new();
    let w1 = pair.wt[0].clone();
    let claude = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let roots: Vec<PathBuf> = (0..10)
        .map(|i| scratch.path().join(format!("r{i}")))
        .collect();
    for (i, r) in roots.iter().enumerate() {
        codex_rollout(r, &format!("J{i}"), &w1);
    }
    for root in &roots {
        with_roots(Some(claude.path()), Some(root), || pair.pass());
    }
    // Read only the newest root repeatedly: r0 and r1 are evicted and exist.
    for _ in 0..3 {
        with_roots(Some(claude.path()), Some(&roots[9]), || pair.pass());
    }
    with_roots(Some(claude.path()), Some(&roots[0]), || pair.pass());
    with_roots(Some(claude.path()), Some(&roots[1]), || pair.pass());
    assert_eq!(rows(&w1).len(), 10, "rows were replayed");
}

// ------------------------------------------------ 4. root empty when recorded

#[test]
fn i01_a_root_that_was_empty_then_gained_files_then_vanished_is_not_replayed() {
    let pair = Pair::new();
    let w1 = pair.wt[0].clone();
    let claude = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let root = scratch.path().join("sessions");
    std::fs::create_dir_all(&root).unwrap();
    let other = scratch.path().join("other");
    std::fs::create_dir_all(&other).unwrap();
    with_roots(Some(claude.path()), Some(&root), || pair.pass());
    let rollout = codex_rollout(&root, "I1", &w1);
    with_roots(Some(claude.path()), Some(&root), || pair.pass());
    assert_eq!(rows(&w1).len(), 1);
    std::fs::rename(&root, scratch.path().join("moved")).unwrap();
    let out = with_roots(Some(claude.path()), Some(&other), || pair.pass());
    assert!(has_cursor(&pair, &rollout), "{out:?}");
    std::fs::rename(scratch.path().join("moved"), &root).unwrap();
    with_roots(Some(claude.path()), Some(&root), || pair.pass());
    assert_eq!(rows(&w1).len(), 1, "replayed");
}

#[test]
fn i02_a_root_that_gains_files_between_passes_it_is_not_read_is_not_replayed_later() {
    let pair = Pair::new();
    let w1 = pair.wt[0].clone();
    let claude = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let a = scratch.path().join("a");
    let b = scratch.path().join("b");
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    let rb = codex_rollout(&b, "I2", &w1);
    // b is read (and has a file), a is read empty; a then gains a file while
    // unread, b disappears for a pass, a is read.
    with_roots(Some(claude.path()), Some(&b), || pair.pass());
    with_roots(Some(claude.path()), Some(&a), || pair.pass());
    codex_rollout(&a, "I2a", &w1);
    std::fs::rename(&b, scratch.path().join("bb")).unwrap();
    let out = with_roots(Some(claude.path()), Some(&a), || pair.pass());
    assert!(has_cursor(&pair, &rb), "{out:?}");
    std::fs::rename(scratch.path().join("bb"), &b).unwrap();
    with_roots(Some(claude.path()), Some(&b), || pair.pass());
    assert_eq!(rows(&w1).len(), 2, "replayed");
}

// ---------------------------------------------------------- 5. deferred resume

#[test]
fn j01_a_deferred_resume_keeps_its_cursor_and_is_read_next_pass() {
    let pair = Pair::new();
    let w1 = pair.wt[0].clone();
    let claude = tempfile::tempdir().unwrap();
    let codex = tempfile::tempdir().unwrap();
    let pad_mib = 100usize;
    let paths: Vec<PathBuf> = (0..3)
        .map(|i| {
            let path = rollout_path(codex.path(), &format!("J{i}"));
            write_lines(&path, &[session_meta(&format!("J{i}"), &w1)]);
            let mut file = std::fs::File::options().append(true).open(&path).unwrap();
            let block = vec![b'\n'; 1 << 20];
            for _ in 0..pad_mib {
                std::io::Write::write_all(&mut file, &block).unwrap();
            }
            path
        })
        .collect();
    with_roots(Some(claude.path()), Some(codex.path()), || pair.pass());
    let before: Vec<Option<u64>> = paths.iter().map(|p| offset_of(&pair, p)).collect();
    assert!(before.iter().all(|o| o.is_some()), "{before:?}");
    for path in &paths {
        append_text(path, &(run(5, "ls", &w1) + "\n"));
    }
    let out = with_roots(Some(claude.path()), Some(codex.path()), || pair.pass());
    let after: Vec<Option<u64>> = paths.iter().map(|p| offset_of(&pair, p)).collect();
    eprintln!("j01: {out:?} before={before:?} after={after:?}");
    assert!(
        after.iter().all(|o| o.is_some()),
        "a deferred file's cursor was forgotten: {after:?} {out:?}"
    );
    let unmoved = after.iter().zip(&before).filter(|(a, b)| a == b).count();
    assert_eq!(
        unmoved, 1,
        "exactly one file defers: {after:?} vs {before:?}"
    );
    assert_eq!(rows(&w1).len(), 2, "{out:?}");
    let third = with_roots(Some(claude.path()), Some(codex.path()), || pair.pass());
    assert_eq!(
        rows(&w1).len(),
        3,
        "the deferred file is read next pass: {third:?}"
    );
    let fourth = with_roots(Some(claude.path()), Some(codex.path()), || pair.pass());
    assert_eq!(rows(&w1).len(), 3, "{fourth:?}");
}
