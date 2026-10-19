//! Adversarial review of OD 1 at the replay seam: what the ordinal-0 unique
//! index does to a pass that holds two identical entries.

use super::*;
use crate::engine::git_reader::ReflogEntry;

fn entry(index: usize, timestamp: i64) -> ReflogEntry {
    ReflogEntry {
        index,
        commit_id: "a".repeat(40),
        selector: format!("HEAD@{{{index}}}"),
        action: "reset".into(),
        message: "reset: moving to HEAD".into(),
        timestamp,
    }
}

#[test]
fn two_identical_entries_in_one_pass_are_both_recorded_and_the_shortfall_is_not_silent() {
    let dir = tempfile::tempdir().expect("repo");
    let repo = dir.path().canonicalize().expect("canonical");
    let repo = repo.to_string_lossy();
    let out = replay_reflog(
        &repo,
        &repo,
        vec![entry(1, 1_790_000_000), entry(0, 1_790_000_000)],
        Ok(std::collections::HashSet::new()),
        CatchUp::default(),
    );
    assert_eq!(out.reflog_entries, 2, "{out:?}");
    assert_eq!(
        out.recorded, 2,
        "one entry vanished with no error, no counter and no truncated flag: {out:?}"
    );
    assert_eq!(out.duplicates_refused, 0, "{out:?}");
}

/// Two processes replaying the same pass number its entries the same way, so
/// the second records nothing and says how many it refused: every replayed
/// entry is accounted for as recorded, refused as a duplicate, or failed.
#[test]
fn a_second_process_replaying_the_same_pass_refuses_every_entry_and_counts_them() {
    let dir = tempfile::tempdir().expect("repo");
    let repo = dir.path().canonicalize().expect("canonical");
    let repo = repo.to_string_lossy();
    let pass = || {
        vec![
            entry(2, 1_790_000_000),
            entry(1, 1_790_000_000),
            entry(0, 1_790_000_005),
        ]
    };
    let first = replay_reflog(
        &repo,
        &repo,
        pass(),
        Ok(std::collections::HashSet::new()),
        CatchUp::default(),
    );
    assert_eq!(
        (first.recorded, first.duplicates_refused),
        (3, 0),
        "{first:?}"
    );
    // The other process built its seen-set before the first one wrote.
    let second = replay_reflog(
        &repo,
        &repo,
        pass(),
        Ok(std::collections::HashSet::new()),
        CatchUp::default(),
    );
    assert_eq!(second.reflog_entries, 3, "{second:?}");
    assert_eq!(
        (second.recorded, second.duplicates_refused),
        (0, 3),
        "{second:?}"
    );
    assert_eq!(ledger::status(&repo).dropped, 0);
}
