//! Adversarial probes of lane L5 (ledger freshness honesty). Tests only.

use crate::ledger::*;
use crate::procguard::LockedSpawn as _;
use std::os::unix::fs::PermissionsExt;
use std::sync::{Arc, Barrier};
use std::time::Instant;

fn draft(repo: &str) -> Draft {
    Draft {
        repo_path: repo.to_string(),
        action: "git.commit".to_string(),
        actor_kind: Some(ActorKind::Human),
        outcome: Some(Outcome::Ok),
        ..Default::default()
    }
}

fn repo_dir() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().to_str().unwrap().to_string();
    (dir, repo)
}

fn newest_ms(repo: &str) -> u64 {
    let ts = freshness_readonly_at(repo, 0).as_of.expect("a row");
    ids::parse_event_time(&ts).expect("a ledger timestamp")
}

fn done() -> CatchUpRecord {
    CatchUpRecord {
        completed: true,
        ..CatchUpRecord::default()
    }
}

#[test]
fn adv_stale_boundary_599_600_601_seconds_to_the_millisecond() {
    let (_dir, repo) = repo_dir();
    append(draft(&repo)).unwrap();
    let t = newest_ms(&repo) + 5_000;
    record_catch_up(&repo, &repo, &done(), t).unwrap();
    // The pass is the newest evidence (the row is older).
    for (delta, stale) in [
        (599_000, false),
        (599_999, false),
        (600_000, false),
        (600_001, true),
        (601_000, true),
    ] {
        let f = freshness_readonly_at(&repo, t + delta);
        assert_eq!(f.stale, stale, "delta {delta}ms: {f:?}");
        assert_eq!(f.stale, !f.stale_reason.is_empty(), "reason iff stale");
    }
}

#[test]
fn adv_future_dated_evidence_is_never_fresh_at_any_later_time_within_the_window_only() {
    let (_dir, repo) = repo_dir();
    append(draft(&repo)).unwrap();
    let now = newest_ms(&repo);
    // A pass stamped an hour ahead (a writer with a skewed clock).
    record_catch_up(&repo, &repo, &done(), now + 3_600_000).unwrap();
    let f = freshness_readonly_at(&repo, now);
    assert!(f.stale, "future-dated evidence called fresh: {f:?}");
    assert!(f.stale_reason.contains("future"), "{}", f.stale_reason);
    // 59 s ahead is inside the tolerance and ages normally; 61 s is not.
    let (_d2, repo2) = repo_dir();
    append(draft(&repo2)).unwrap();
    let n2 = newest_ms(&repo2);
    record_catch_up(&repo2, &repo2, &done(), n2 + 59_000).unwrap();
    assert!(!freshness_readonly_at(&repo2, n2).stale);
    assert!(
        freshness_readonly_at(&repo2, n2 + 59_000 + 600_001).stale,
        "a +59s skewed stamp must still age out"
    );
}

#[test]
fn adv_one_future_dated_row_does_not_mask_a_genuinely_fresh_pass_as_stale() {
    // Reverse lie: pessimistic. A fresh, correctly stamped pass and an old
    // future-dated one in another worktree row: is the ledger called stale?
    let (_dir, repo) = repo_dir();
    append(draft(&repo)).unwrap();
    let now = newest_ms(&repo);
    let wt_skewed = format!("{repo}/skewed");
    let wt_ok = format!("{repo}/ok");
    record_catch_up(&repo, &wt_skewed, &done(), now + 7_200_000).unwrap();
    record_catch_up(&repo, &wt_ok, &done(), now).unwrap();
    let f = freshness_readonly_at(&repo, now + 1_000);
    assert!(
        !f.stale,
        "a pass one second ago exists; one skewed row should not hide it: {f:?}"
    );
}

#[test]
fn adv_pass_rows_of_another_worktree_are_reported_and_a_removed_worktrees_failure_is_sticky() {
    let (_dir, repo) = repo_dir();
    append(draft(&repo)).unwrap();
    let now = newest_ms(&repo);
    let a = format!("{repo}/wt-a");
    let b = format!("{repo}/wt-b");
    // A: an old, bad pass (the worktree was later removed).
    record_catch_up(
        &repo,
        &a,
        &CatchUpRecord {
            completed: true,
            truncated: true,
            failed_records: 5,
            skipped_lines: 9,
            error: "old failure in a removed worktree".into(),
            ..CatchUpRecord::default()
        },
        now - 86_400_000,
    )
    .unwrap();
    // B: a clean pass just now.
    record_catch_up(&repo, &b, &done(), now).unwrap();
    let f = freshness_readonly_at(&repo, now + 1_000);
    let c = f.catch_up.expect("rows");
    assert_eq!(c.worktrees, 2, "the view is per ledger, not per worktree");
    assert!(
        !c.truncated && c.failed_records == 0 && c.error.is_empty(),
        "a day-old failure of another worktree is still reported as the current coverage: {c:?}"
    );
}

#[test]
fn adv_basis_falls_back_honestly_when_catch_up_state_is_absent_in_a_read_only_db() {
    let (_dir, repo) = repo_dir();
    append(draft(&repo)).unwrap();
    record_catch_up(&repo, &repo, &done(), ids::now_millis()).unwrap();
    {
        let conn = rusqlite::Connection::open(ledger_path(&repo)).unwrap();
        conn.execute("DROP TABLE catch_up_state", []).unwrap();
    }
    let state_dir = ledger_path(&repo).parent().unwrap().to_path_buf();
    let mut entries = vec![state_dir.clone()];
    for e in std::fs::read_dir(&state_dir).unwrap() {
        entries.push(e.unwrap().path());
    }
    // Checkpoint so that no WAL is needed, then make everything read-only.
    {
        let conn = rusqlite::Connection::open(ledger_path(&repo)).unwrap();
        let _ = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()));
    }
    for p in &entries {
        let mode = if p.is_dir() { 0o555 } else { 0o444 };
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode)).unwrap();
    }
    let f = freshness_readonly_at(&repo, ids::now_millis());
    for p in entries.iter().rev() {
        let mode = if p.is_dir() { 0o755 } else { 0o644 };
        let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode));
    }
    // Either it reads (basis newest_row, no catch_up) or it says it could not.
    if f.freshness_ok {
        assert_eq!(f.last_catch_up_basis, "newest_row", "{f:?}");
        assert!(f.catch_up.is_none(), "{f:?}");
    } else {
        assert!(f.stale && !f.freshness_error.is_empty(), "{f:?}");
    }
}

fn snapshot_tree(dir: &std::path::Path) -> Vec<(String, u64, std::time::SystemTime, Vec<u8>)> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        let meta = std::fs::metadata(&p).unwrap();
        if meta.is_file() {
            out.push((
                p.file_name().unwrap().to_string_lossy().into_owned(),
                meta.len(),
                meta.modified().unwrap(),
                std::fs::read(&p).unwrap(),
            ));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

#[test]
fn adv_readers_do_not_write_or_panic_on_a_read_only_ledger_directory() {
    let (_dir, repo) = repo_dir();
    append(draft(&repo)).unwrap();
    record_catch_up(&repo, &repo, &done(), ids::now_millis()).unwrap();
    let state_dir = ledger_path(&repo).parent().unwrap().to_path_buf();
    // Fold the WAL in first so the baseline is what a clean shutdown leaves.
    {
        let conn = rusqlite::Connection::open(ledger_path(&repo)).unwrap();
        let _ = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()));
    }
    let files: Vec<_> = std::fs::read_dir(&state_dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    for p in &files {
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o444)).unwrap();
    }
    std::fs::set_permissions(&state_dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    let before = snapshot_tree(&state_dir);

    let started = Instant::now();
    let f = freshness_readonly_at(&repo, ids::now_millis());
    let s = status_readonly(&repo);
    let view = LedgerView::from_readonly(s, f.clone());
    let page = query_readonly_page(&repo, &EventQuery::default(), PAGE_MAX_BYTES);
    let elapsed = started.elapsed();

    let after = snapshot_tree(&state_dir);
    std::fs::set_permissions(&state_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    for p in &files {
        let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o644));
    }
    assert_eq!(before, after, "a reader changed the ledger directory");
    assert!(elapsed.as_secs() < 10, "{elapsed:?}");
    assert!(!view.recording);
    if !f.freshness_ok {
        assert!(f.stale && !f.freshness_error.is_empty(), "{f:?}");
    }
    let _ = page;
}

#[test]
fn adv_a_reader_against_a_ledger_locked_by_a_writer_is_bounded_and_says_why() {
    let (_dir, repo) = repo_dir();
    append(draft(&repo)).unwrap();
    // A separate process holds the database in exclusive locking mode inside an
    // open transaction: the strongest hold a writer has.
    let mut writer = std::process::Command::new("sqlite3")
        .arg(ledger_path(&repo))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn_locked()
        .expect("sqlite3 on PATH");
    {
        use std::io::Write;
        let stdin = writer.stdin.as_mut().unwrap();
        stdin
            .write_all(b".timeout 5000\nBEGIN IMMEDIATE;\nUPDATE events SET ts_utc = ts_utc WHERE id = 1;\n")
            .unwrap();
        stdin.flush().unwrap();
    }
    std::thread::sleep(std::time::Duration::from_millis(800));

    let started = Instant::now();
    let f = freshness_readonly_at(&repo, ids::now_millis());
    let elapsed = started.elapsed();
    let s = status_readonly(&repo);
    let total = started.elapsed();
    writer.kill().unwrap();
    writer.wait().unwrap();

    assert!(
        total.as_secs() < 15,
        "freshness {elapsed:?}, freshness+status {total:?}: not bounded by the busy timeout"
    );
    if f.freshness_ok {
        // A WAL reader may legitimately proceed; then it must not say stale-unknown.
        assert!(f.as_of.is_some(), "{f:?}");
    } else {
        assert!(f.stale && !f.freshness_error.is_empty(), "{f:?}");
        assert!(!s.recording || s.error.is_empty() || !s.error.is_empty());
    }
}

#[test]
fn adv_concurrent_catch_up_writers_leave_untorn_rows_and_a_consistent_summary() {
    let (_dir, repo) = repo_dir();
    append(draft(&repo)).unwrap();
    let repo = Arc::new(repo);
    let gate = Arc::new(Barrier::new(5));
    let mut handles = Vec::new();
    for t in 0..4_i64 {
        let repo = repo.clone();
        let gate = gate.clone();
        handles.push(std::thread::spawn(move || {
            gate.wait();
            for i in 0..40_i64 {
                let n = t * 1000 + i + 1;
                record_catch_up(
                    &repo,
                    &repo,
                    &CatchUpRecord {
                        completed: true,
                        recorded: n,
                        transcripts: n,
                        transcripts_listed: n,
                        transcripts_unlisted: n,
                        skipped_lines: n,
                        failed_records: n,
                        ..CatchUpRecord::default()
                    },
                    ids::now_millis(),
                )
                .unwrap();
            }
        }));
    }
    let reader_repo = repo.clone();
    let reader_gate = gate.clone();
    let reader = std::thread::spawn(move || {
        reader_gate.wait();
        let mut torn = Vec::new();
        for _ in 0..150 {
            let f = freshness_readonly_at(&reader_repo, ids::now_millis());
            if let Some(c) = f.catch_up {
                let all = [
                    c.recorded,
                    c.transcripts,
                    c.transcripts_listed,
                    c.transcripts_unlisted,
                    c.skipped_lines,
                    c.failed_records,
                ];
                if all.iter().any(|v| *v != all[0]) || c.worktrees != 1 {
                    torn.push(format!("{c:?}"));
                }
            }
        }
        torn
    });
    for h in handles {
        h.join().unwrap();
    }
    let torn = reader.join().unwrap();
    assert!(torn.is_empty(), "torn reads: {torn:?}");
}
