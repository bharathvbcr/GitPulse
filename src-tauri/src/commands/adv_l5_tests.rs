//! Adversarial probes of lane L5: how often a bind counts a catch-up failure
//! once the corpus has more transcripts than the walk lists. Tests only.

use super::audit::bind_after_catch_up;
use crate::ingest::CatchUp;
use crate::ledger;
use crate::test_support::git_in;

fn git_repo() -> (tempfile::TempDir, String) {
    let _env = crate::test_support::env::env_serial();
    let dir = tempfile::tempdir().expect("temp repository");
    let path = dir.path().canonicalize().expect("canonical path");
    git_in(&path, &["init", "-b", "main"]);
    std::fs::write(path.join("seed.txt"), "seed").expect("seed repository");
    git_in(&path, &["add", "seed.txt"]);
    git_in(&path, &["commit", "-m", "seed"]);
    let text = path.to_string_lossy().into_owned();
    (dir, text)
}

/// 6000 transcripts, 1000 more than the walk lists. Every bind runs the real
/// bounded catch-up over them and the result never changes: the same 1000 files
/// stay unlisted. Counts how many failures N binds record.
#[test]
#[ignore = "ADV-L5 BROKEN: catch_up_failures grows by one per bind for as long as any file is unlisted"]
fn adv_binds_over_a_6000_file_corpus_count_one_failure_each_without_bound() {
    let (_dir, repo) = git_repo();
    let root = tempfile::tempdir().unwrap();
    let slug = root.path().join("slug");
    std::fs::create_dir_all(&slug).unwrap();
    for n in 0..6000 {
        std::fs::File::create(slug.join(format!("{n}.jsonl"))).unwrap();
    }
    let serial = crate::test_support::env::env_serial();
    let _env =
        crate::test_support::env::bind_env(&serial).set("GITPULSE_TRANSCRIPT_ROOT", root.path());
    let mut counts = Vec::new();
    for round in 0..6 {
        let task = format!("T-{round}");
        bind_after_catch_up(&repo, &repo, &task, |address| {
            crate::ingest::catch_up_into_bounded(&address.anchor, &address.worktree, None)
        })
        .expect("the bind goes ahead");
        counts.push(ledger::status(&repo).catch_up_failures);
    }
    eprintln!("catch_up_failures after each of 6 binds over 6000 transcripts: {counts:?}");
    assert!(
        counts.last().copied().unwrap_or(0) <= 1,
        "catch_up_failures grows with every bind over a corpus that never changes: {counts:?}"
    );
}

/// The same bind with a pass that left files unlisted and nothing else wrong:
/// the lane's own note text starts with "catch-up truncated: ", the prefix
/// `is_partial` accepts, but `is_partial` also needs a reflog backlog.
#[test]
#[ignore = "ADV-L5 BROKEN: is_partial needs a reflog backlog, so an unlisted-files-only pass is counted as a failure"]
fn adv_a_pass_whose_only_truncation_is_unlisted_files_is_not_partial() {
    let (_dir, repo) = git_repo();
    bind_after_catch_up(&repo, &repo, "T-1", |_| CatchUp {
        truncated: true,
        transcripts_unlisted: 1000,
        unlisted_cap: 1000,
        error: "catch-up truncated: the transcript walk left 1000 file(s) unlisted and unread"
            .into(),
        ..CatchUp::default()
    })
    .unwrap();
    assert_eq!(
        ledger::status(&repo).catch_up_failures,
        0,
        "unlisted files with a clean pass were counted as a failed catch-up"
    );
}
