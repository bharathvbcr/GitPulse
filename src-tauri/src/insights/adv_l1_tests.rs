//! Adversarial checks on the commit-rhythm walk: `--since-as-filter` against
//! the count-bounded fallback, out-of-order committer dates, a window edge at
//! an exact commit timestamp, and what `truncated` means.

use super::*;
use crate::procguard::LockedSpawn;
use crate::test_support::{commit_at, git_repo};
use std::io::Write;

const ANCHOR: i64 = 1_800_000_000;
const DAY: i64 = 86_400;

fn both(dir: &Path) -> (FleetCommitStats, FleetCommitStats) {
    let walk = |filtered| {
        fleet_commit_stats_walking(dir, ANCHOR, FLEET_COMMIT_WINDOW_DAYS, filtered).unwrap()
    };
    (walk(true), walk(false))
}

fn same(filtered: &FleetCommitStats, counted: &FleetCommitStats) {
    assert_eq!(filtered.commits, counted.commits);
    assert_eq!(filtered.daily, counted.daily);
    assert_eq!(filtered.authors, counted.authors);
    assert_eq!(filtered.active_days, counted.active_days);
    assert_eq!(filtered.commits_7d, counted.commits_7d);
    assert_eq!(filtered.commits_prior_7d, counted.commits_prior_7d);
    assert_eq!(filtered.last_commit_epoch, counted.last_commit_epoch);
}

/// A recent commit sits under an ancient-dated tip, and commits land exactly on,
/// one second inside and one second outside the window's edge.
#[test]
fn adv_l1_out_of_order_dates_and_an_exact_edge_bucket_identically() {
    let dir = git_repo();
    let edge = ANCHOR - i64::from(FLEET_COMMIT_WINDOW_DAYS) * DAY;
    let plan = [
        (ANCHOR - 5 * DAY, "a@x"),
        (edge, "edge@x"),
        (edge - 1, "outside@x"),
        (edge + 1, "inside@x"),
        (ANCHOR - 400 * DAY, "skewed@x"),
        (ANCHOR - 2 * DAY, "b@x"),
        (ANCHOR + 3 * DAY, "future@x"),
        (ANCHOR - 600 * DAY, "tipold@x"),
    ];
    for (i, (epoch, email)) in plan.into_iter().enumerate() {
        commit_at(dir.path(), &format!("c{i}"), epoch, email);
    }
    let (filtered, counted) = both(dir.path());
    same(&filtered, &counted);
    // a, inside and b are in the window. The edge commit has age == window,
    // which `bucket_commit_log` drops even though git's filter keeps it. The
    // future-dated row is COUNTED in the newest bucket (age <= 0), although the
    // comment above `bucket_commit_log` says such a row is dropped. Predates
    // the lane; both walks agree on it.
    assert_eq!(filtered.commits, 4, "{filtered:?}");
    assert_eq!(filtered.authors, 4);
    assert_eq!(filtered.last_commit_epoch, ANCHOR + 3 * DAY);
    assert!(!filtered.truncated && !counted.truncated);
}

fn fast_import(dir: &Path, epochs_oldest_first: &[i64]) {
    let mut stream = String::new();
    for (i, epoch) in epochs_oldest_first.iter().enumerate() {
        stream.push_str(&format!(
            "commit refs/heads/main\ncommitter T <t@x> {epoch} +0000\ndata 1\n{}\n",
            i % 10
        ));
    }
    let mut child = std::process::Command::new("git")
        .args(["fast-import", "--quiet"])
        .current_dir(dir)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn_locked()
        .expect("spawn fast-import");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stream.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// More than the cap of commits in TOTAL, of which only five are inside the
/// window and all five are at the tip. The count-bounded walk has seen the whole
/// window, yet reports `truncated`.
#[test]
fn adv_l1_truncated_is_true_for_a_big_history_even_when_the_window_was_fully_seen() {
    let dir = git_repo();
    let total = MAX_FLEET_COMMITS + 500;
    let mut epochs: Vec<i64> = (0..total - 5)
        .map(|i| ANCHOR - 300 * DAY + i as i64)
        .collect();
    epochs.extend((0..5).map(|i| ANCHOR - 5 * DAY + i));
    fast_import(dir.path(), &epochs);
    let (filtered, counted) = both(dir.path());
    same(&filtered, &counted);
    assert_eq!(counted.commits, 5);
    assert!(!filtered.truncated, "the filtered walk saw 5 rows");
    assert!(
        counted.truncated,
        "the fallback flags a window it fully saw: history spends the cap"
    );
}

/// The converse that makes the flag necessary: in-window commits buried under
/// more than the cap of ancient-dated commits are lost by the fallback, and
/// only the flag says so.
#[test]
fn adv_l1_the_fallback_loses_a_buried_window_and_flags_it() {
    let dir = git_repo();
    let mut epochs: Vec<i64> = (0..3).map(|i| ANCHOR - 5 * DAY + i).collect();
    epochs.extend((0..MAX_FLEET_COMMITS + 10).map(|i| ANCHOR - 400 * DAY + i as i64));
    fast_import(dir.path(), &epochs);
    let (filtered, counted) = both(dir.path());
    assert_eq!(filtered.commits, 3, "filter finds them: {filtered:?}");
    assert_eq!(counted.commits, 0, "fallback cannot reach them");
    assert!(counted.truncated, "and says so");
    assert!(!filtered.truncated);
}
