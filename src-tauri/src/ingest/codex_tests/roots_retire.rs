//! Retiring a recorded transcript root that is confirmed absent (ft-02a20716):
//! a source that has used two roots must go on forgetting the cursors of
//! transcripts that are gone, and a root that is merely unreadable must never
//! be retired. Every fixture is synthetic.

use super::*;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

const N: u32 = pass::RETIRE_ROOT_AFTER_ABSENT_PASSES;

fn has_cursor(pair: &Pair, path: &Path) -> bool {
    ledger::transcript_progress(&pair.wt[0], &pair.wt[0])
        .unwrap()
        .offsets
        .contains_key(&ledger::transcript_key(path))
}

fn rollout(root: &Path, thread: &str, wt: &str) -> PathBuf {
    let path = rollout_path(root, thread);
    write_lines(&path, &[session_meta(thread, wt), run(1, "ls", wt)]);
    path
}

/// Two roots used in turn, the first holding a Codex rollout, and a Claude
/// transcript that is then deleted. Returns the cursors that should end up
/// forgotten once root A is retired, and root B (still read).
struct Fixture {
    pair: Pair,
    claude: tempfile::TempDir,
    scratch: tempfile::TempDir,
    root_a: PathBuf,
    root_b: PathBuf,
    rollout_a: PathBuf,
    deleted_claude: PathBuf,
}

fn fixture() -> Fixture {
    let pair = Pair::new();
    let w1 = pair.wt[0].clone();
    let claude = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let root_a = scratch.path().join("a/sessions");
    let root_b = scratch.path().join("b");
    let rollout_a = rollout(&root_a, "A", &w1);
    rollout(&root_b, "B", &w1);
    let deleted_claude = claude.path().join("p/D.jsonl");
    write_lines(
        &deleted_claude,
        &[claude_edit(&w1, "D", 1, &format!("{w1}/d.rs"))],
    );
    for root in [&root_a, &root_b] {
        with_roots(Some(claude.path()), Some(root), || pair.pass());
    }
    std::fs::remove_file(&deleted_claude).unwrap();
    Fixture {
        pair,
        claude,
        scratch,
        root_a,
        root_b,
        rollout_a,
        deleted_claude,
    }
}

impl Fixture {
    fn pass_reading_b(&self) -> CatchUp {
        with_roots(Some(self.claude.path()), Some(&self.root_b), || {
            self.pair.pass()
        })
    }
}

#[test]
fn r01_a_root_absent_for_n_passes_is_retired_and_forgetting_resumes() {
    let f = fixture();
    std::fs::remove_dir_all(f.scratch.path().join("a")).unwrap();
    for pass in 1..N {
        let out = f.pass_reading_b();
        assert!(has_cursor(&f.pair, &f.rollout_a), "pass {pass}: {out:?}");
        assert!(has_cursor(&f.pair, &f.deleted_claude), "pass {pass}");
        assert!(out.stale_cursors >= 2, "pass {pass}: {out:?}");
    }
    let out = f.pass_reading_b();
    assert!(!has_cursor(&f.pair, &f.rollout_a), "{out:?}");
    assert!(!has_cursor(&f.pair, &f.deleted_claude), "{out:?}");
    assert_eq!(out.stale_cursors, 0, "{out:?}");
}

#[test]
fn r02_an_unreadable_root_is_never_retired_and_never_permits_forgetting() {
    let f = fixture();
    let holder = f.scratch.path().join("a");
    std::fs::set_permissions(&holder, std::fs::Permissions::from_mode(0o000)).unwrap();
    let mut outs = Vec::new();
    for _ in 0..(N + 3) {
        outs.push(f.pass_reading_b());
    }
    std::fs::set_permissions(&holder, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(has_cursor(&f.pair, &f.rollout_a));
    assert!(has_cursor(&f.pair, &f.deleted_claude));
    assert!(outs.iter().all(|out| out.stale_cursors >= 2), "{outs:?}");
}

#[test]
fn r03_a_root_that_became_a_file_is_never_retired() {
    let f = fixture();
    std::fs::remove_dir_all(f.scratch.path().join("a")).unwrap();
    std::fs::create_dir_all(f.scratch.path().join("a")).unwrap();
    std::fs::write(&f.root_a, "not a directory").unwrap();
    for _ in 0..(N + 3) {
        f.pass_reading_b();
    }
    assert!(has_cursor(&f.pair, &f.rollout_a));
    assert!(has_cursor(&f.pair, &f.deleted_claude));
}

#[test]
fn r04_a_root_that_comes_back_restarts_the_run_of_absent_passes() {
    let f = fixture();
    let gone = f.scratch.path().join("a");
    let kept = f.scratch.path().join("a-parked");
    std::fs::rename(&gone, &kept).unwrap();
    for _ in 1..N {
        f.pass_reading_b();
    }
    // Present again, though not the root being read: the run starts over.
    std::fs::rename(&kept, &gone).unwrap();
    f.pass_reading_b();
    std::fs::rename(&gone, &kept).unwrap();
    for pass in 1..N {
        let out = f.pass_reading_b();
        assert!(has_cursor(&f.pair, &f.rollout_a), "pass {pass}: {out:?}");
    }
    f.pass_reading_b();
    assert!(!has_cursor(&f.pair, &f.rollout_a));
}

#[test]
fn r05_a_retired_root_that_returns_records_nothing_twice() {
    let f = fixture();
    let w1 = f.pair.wt[0].clone();
    let parked = f.scratch.path().join("a-parked");
    std::fs::rename(f.scratch.path().join("a"), &parked).unwrap();
    for _ in 0..N {
        f.pass_reading_b();
    }
    assert!(!has_cursor(&f.pair, &f.rollout_a));
    let before = rows(&w1).len();
    std::fs::rename(&parked, f.scratch.path().join("a")).unwrap();
    let out = with_roots(Some(f.claude.path()), Some(&f.root_a), || f.pair.pass());
    assert_eq!(out.recorded, 0, "{out:?}");
    assert_eq!(out.duplicates_refused, 1, "{out:?}");
    assert_eq!(rows(&w1).len(), before);
}
