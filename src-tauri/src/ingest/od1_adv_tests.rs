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
#[ignore = "adv: generation 1 recorded both identical same-second entries of one pass (the seen-set is built once); the unique index now drops the second and `recorded` silently falls short of `reflog_entries`"]
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
}
