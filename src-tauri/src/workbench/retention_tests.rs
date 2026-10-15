use super::*;
use serde_json::json;
use std::sync::atomic::AtomicUsize;

fn report() -> Value {
    json!({"ok": true, "item": {
        "receipts": {"deleted": 3, "eligible": 5, "truncated": true},
        "events": {"deleted": 1, "eligible": 1, "truncated": false},
        "revisions": {"deleted": 0, "eligible": 0, "truncated": false}}})
}

fn profile() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("workbench.sqlite");
    std::fs::write(&path, b"").unwrap();
    (dir, path)
}

#[test]
fn a_pass_runs_at_most_once_per_window_and_again_after_it() {
    let (_d, path) = profile();
    let running = AtomicBool::new(false);
    let calls = AtomicUsize::new(0);
    let go = |now| {
        run_once(&path, &running, now, || {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(report())
        })
    };
    let first = go(1_000_000);
    assert_eq!(
        first,
        Outcome::Ran(
            "receipts deleted=3 eligible=5 truncated=true; events deleted=1 eligible=1 truncated=false; revisions deleted=0 eligible=0 truncated=false".into()
        )
    );
    assert_eq!(go(1_000_000 + 60), Outcome::NotDue);
    assert_eq!(go(1_000_000 + WINDOW.as_secs() - 1), Outcome::NotDue);
    assert!(matches!(go(1_000_000 + WINDOW.as_secs()), Outcome::Ran(_)));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn a_failure_is_surfaced_and_retried_next_window_not_every_tick() {
    let (_d, path) = profile();
    let running = AtomicBool::new(false);
    let failed = run_once(&path, &running, 5_000_000, || Err("disk full".into()));
    assert_eq!(failed, Outcome::Failed("disk full".into()));
    let calls = AtomicUsize::new(0);
    let again = run_once(&path, &running, 5_000_060, || {
        calls.fetch_add(1, Ordering::SeqCst);
        Ok(report())
    });
    assert_eq!(again, Outcome::NotDue);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let next = run_once(&path, &running, 5_000_000 + WINDOW.as_secs(), || {
        Ok(report())
    });
    assert!(matches!(next, Outcome::Ran(_)), "{next:?}");
}

#[test]
fn an_answer_without_counts_is_a_failure_not_a_pass() {
    let (_d, path) = profile();
    let outcome = run_once(&path, &AtomicBool::new(false), 1, || {
        Ok(json!({"ok": true}))
    });
    assert!(matches!(outcome, Outcome::Failed(_)), "{outcome:?}");
}

#[test]
fn it_never_runs_beside_itself() {
    let (_d, path) = profile();
    let running = AtomicBool::new(false);
    let calls = AtomicUsize::new(0);
    let inner = run_once(&path, &running, 1, || {
        // A second caller while this pass is running.
        let nested = run_once(&path, &running, 1, || {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(report())
        });
        assert_eq!(nested, Outcome::Busy);
        Ok(report())
    });
    assert!(matches!(inner, Outcome::Ran(_)));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(!running.load(Ordering::SeqCst), "the flag is released");
    // Concurrent threads: exactly one pass runs.
    let (_d2, path2) = profile();
    let running = Arc::new(AtomicBool::new(false));
    let passes = Arc::new(AtomicUsize::new(0));
    let threads: Vec<_> = (0..8)
        .map(|_| {
            let (path, running, passes) = (path2.clone(), running.clone(), passes.clone());
            std::thread::spawn(move || {
                run_once(&path, &running, 9, || {
                    passes.fetch_add(1, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(50));
                    Ok(report())
                })
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    assert_eq!(passes.load(Ordering::SeqCst), 1);
}

#[test]
fn it_never_creates_a_profile_and_a_clock_set_back_does_not_wedge_it() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("workbench.sqlite");
    let outcome = run_once(&missing, &AtomicBool::new(false), 1, || Ok(report()));
    assert_eq!(outcome, Outcome::NoProfile);
    assert!(!missing.exists() && !stamp_path(&missing).exists());

    let (_d, path) = profile();
    std::fs::write(stamp_path(&path), (10 * WINDOW.as_secs()).to_string()).unwrap();
    assert!(
        due(&stamp_path(&path), 1_000),
        "a stamp far in the future is due"
    );
    std::fs::write(stamp_path(&path), "not a number").unwrap();
    assert!(due(&stamp_path(&path), 1_000));
}

#[test]
fn a_real_profile_compacts_through_store_compact() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("workbench.sqlite");
    let store = dc_store::Store::open(&path).unwrap();
    let outcome = run_once(&path, &AtomicBool::new(false), now_secs(), || {
        super::super::query(&store, "store.compact", "{}").map_err(|e| e.message)
    });
    assert!(matches!(outcome, Outcome::Ran(_)), "{outcome:?}");
}

/// The loop's clock must agree with the one `tick` used to keep: a look when
/// none has been made, none until a whole window has passed, one at exactly it.
#[test]
fn the_check_clock_is_due_first_then_exactly_at_the_window() {
    let start = Instant::now();
    let mut retention = Retention::new();
    assert_eq!(retention.next_check_in(start), Duration::ZERO);
    assert!(retention.due_to_check(start), "the first look is due");
    assert_eq!(retention.next_check_in(start), CHECK_EVERY);
    let almost = start + CHECK_EVERY - Duration::from_nanos(1);
    assert_eq!(retention.next_check_in(almost), Duration::from_nanos(1));
    assert!(!retention.due_to_check(almost), "a nanosecond early");
    assert_eq!(retention.next_check_in(start + CHECK_EVERY), Duration::ZERO);
    assert!(retention.due_to_check(start + CHECK_EVERY), "exactly due");
    assert_eq!(
        retention.next_check_in(start + CHECK_EVERY),
        CHECK_EVERY,
        "a look restarts the window"
    );
}

#[test]
fn the_check_clock_survives_a_clock_that_reads_earlier_and_a_huge_jump() {
    let start = Instant::now() + Duration::from_secs(3600);
    let mut retention = Retention::new();
    assert!(retention.due_to_check(start));
    // A reading before the last look: not due, and a full window still to go
    // rather than an underflow.
    let before = start - Duration::from_secs(3600);
    assert!(!retention.due_to_check(before));
    assert_eq!(retention.next_check_in(before), CHECK_EVERY);
    // A suspend that lasted days: due once, then not again at the same reading.
    let later = start + Duration::from_secs(5 * 86_400);
    assert_eq!(retention.next_check_in(later), Duration::ZERO);
    assert!(retention.due_to_check(later));
    assert!(!retention.due_to_check(later), "one look per window");
    assert_eq!(retention.next_check_in(later), CHECK_EVERY);
}
