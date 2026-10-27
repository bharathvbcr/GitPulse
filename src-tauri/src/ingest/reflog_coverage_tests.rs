//! CARD 27 (ft-9d043f74...4671), S6-08 and S6-09: a failed record surfaces, and
//! one catch-up covers the whole worktree family (every worktree's HEAD
//! reflog and the default branch's own) with a bounded number of Git children.

use super::reflog_replay_tests::{git_at, reflog_rows, seeded_repo, T0};
use super::*;
use crate::engine::git_cli::spawn_log;
use crate::test_support::{commit_at, git_in, repo_with_worktrees};

fn with_empty_transcripts<T>(f: impl FnOnce() -> T) -> T {
    let corpus = tempfile::tempdir().expect("corpus");
    let serial = crate::test_support::env::env_serial();
    let _env =
        crate::test_support::env::bind_env(&serial).set("GITPULSE_TRANSCRIPT_ROOT", corpus.path());
    f()
}

fn plain(path: &std::path::Path) -> String {
    crate::engine::git_cli::canonicalize_plain(path)
        .expect("canonical")
        .to_string_lossy()
        .into_owned()
}

fn ref_of(event: &ledger::LedgerEvent) -> Option<String> {
    let detail: serde_json::Value = serde_json::from_str(event.detail_json.as_deref()?).ok()?;
    detail.get("ref")?.as_str().map(str::to_string)
}

/// S6-08: a `ledger::record` that fails surfaces in `CatchUp.failed_records`,
/// the pass says it is incomplete, the watermark stays behind the failed entry,
/// and the next pass records it and everything after it.
#[test]
fn a_failed_record_surfaces_and_the_next_pass_retries_it() {
    let (dir, repo) = seeded_repo();
    let first = ingest_reflog_into(&repo, &repo, 200);
    assert_eq!(first.recorded, 1, "{first:?}");

    commit_at(dir.path(), "ok1", T0 + 10, "gitpulse@test.local");
    commit_at(dir.path(), "boom", T0 + 11, "gitpulse@test.local");
    commit_at(dir.path(), "after", T0 + 12, "gitpulse@test.local");
    let db = dir.path().join(".devcouncil").join("ledger.sqlite");
    let conn = rusqlite::Connection::open(&db).expect("open the ledger file");
    conn.execute_batch(
        "CREATE TRIGGER inject_failure BEFORE INSERT ON events
         WHEN NEW.object LIKE '%boom%'
         BEGIN SELECT RAISE(ABORT, 'injected failure'); END;",
    )
    .expect("inject the failure");

    let second = ingest_reflog_into(&repo, &repo, 200);
    assert_eq!(second.failed_records, 1, "{second:?}");
    assert_eq!(
        second.recorded, 1,
        "only the entry before the failure: {second:?}"
    );
    assert_eq!(
        second.reflog_pending, 2,
        "the failed entry and the one after: {second:?}"
    );
    assert!(second.truncated, "{second:?}");
    assert!(second.error.contains("could not be written"), "{second:?}");

    conn.execute_batch("DROP TRIGGER inject_failure;")
        .expect("lift the failure");
    let third = ingest_reflog_into(&repo, &repo, 200);
    assert_eq!(third.recorded, 2, "{third:?}");
    assert_eq!(
        (third.failed_records, third.reflog_pending),
        (0, 0),
        "{third:?}"
    );
    assert!(!third.truncated, "{third:?}");
    let subjects: Vec<_> = reflog_rows(&repo)
        .into_iter()
        .filter_map(|event| event.object)
        .collect();
    for wanted in ["ok1", "boom", "after"] {
        assert_eq!(
            subjects.iter().filter(|o| o.contains(wanted)).count(),
            1,
            "{wanted} must be recorded exactly once: {subjects:?}"
        );
    }
}

/// S6-09: a commit in a linked worktree, and a main fast-forward made from
/// that worktree while main is checked out nowhere, both appear after one
/// catch-up. The fast-forward moves only `refs/heads/main`'s log.
#[test]
fn a_linked_worktree_commit_and_a_default_branch_fast_forward_appear_after_one_catch_up() {
    let (main, _parent, linked) = repo_with_worktrees(1);
    let anchor = plain(main.path());
    let wt = linked[0].clone();
    // Main is checked out nowhere, so another worktree may move it.
    git_in(main.path(), &["switch", "-c", "dev"]);
    commit_at(&wt, "linked change", T0 + 20, "gitpulse@test.local");
    git_at(&wt, &["fetch", ".", "agent/00:main"], T0 + 21);

    let out = with_empty_transcripts(|| catch_up_into_bounded(&anchor, &anchor, None));
    assert!(!out.truncated, "{out:?}");
    assert_eq!(out.failed_records, 0, "{out:?}");

    let rows = reflog_rows(&anchor);
    let wt_str = wt.to_string_lossy().into_owned();
    assert!(
        rows.iter().any(|event| {
            event.worktree_path.as_deref() == Some(wt_str.as_str())
                && event.action == "reflog.commit"
                && event
                    .object
                    .as_deref()
                    .is_some_and(|o| o.contains("linked change"))
        }),
        "the linked worktree's own commit is missing: {rows:#?}"
    );
    assert!(
        rows.iter().any(|event| {
            event.worktree_path.is_none()
                && ref_of(event).as_deref() == Some("refs/heads/main")
                && event
                    .object
                    .as_deref()
                    .is_some_and(|o| o.contains("fast-forward"))
        }),
        "the fast-forward of main is missing: {rows:#?}"
    );
}

/// S6-09: the default branch is ingested once per family, whichever worktree
/// triggers the catch-up, and never twice.
#[test]
fn the_default_branch_is_ingested_once_per_family() {
    let (main, _parent, linked) = repo_with_worktrees(2);
    let anchor = plain(main.path());
    git_in(main.path(), &["switch", "-c", "dev"]);
    commit_at(&linked[0], "moves main", T0 + 29, "gitpulse@test.local");
    git_at(&linked[0], &["fetch", ".", "agent/00:main"], T0 + 30);

    let on_main = || {
        reflog_rows(&anchor)
            .into_iter()
            .filter(|event| ref_of(event).as_deref() == Some("refs/heads/main"))
            .count()
    };
    with_empty_transcripts(|| {
        catch_up_into_bounded(&anchor, &anchor, None);
        let once = on_main();
        let from_linked = linked[1].to_string_lossy().into_owned();
        let second = catch_up_into_bounded(&anchor, &from_linked, None);
        let third = catch_up_into_bounded(&anchor, &anchor, None);
        assert_eq!(on_main(), once, "{second:?} {third:?}");
        // The commit that created main is also its own checkout's HEAD entry,
        // which the unique identity index already held, so only the
        // fast-forward is main's own row.
        assert_eq!(once, 1, "main's own log recorded once per entry");
    });
    let entries = crate::engine::git_reader::GitReader::get_reflog_within(
        &anchor,
        "refs/heads/main",
        100,
        None,
    )
    .expect("main's reflog");
    assert_eq!(entries.len(), 2, "the fixture must have moved main");
}

/// S6-09: the number of reflog children a pass spawns is bounded by the family
/// bound plus the default branch, and a family past the bound is reported as
/// not fully read rather than read without limit.
#[test]
fn the_reflog_spawn_count_is_bounded_and_a_larger_family_is_reported() {
    let (main, _parent, linked) = repo_with_worktrees(4);
    let anchor = plain(main.path());
    let siblings: Vec<String> = linked
        .iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect();
    let family = Ok(siblings);
    let dirs: Vec<std::path::PathBuf> = std::iter::once(std::path::PathBuf::from(&anchor))
        .chain(linked.iter().cloned())
        .collect();
    let shows = || {
        dirs.iter()
            .flat_map(|dir| spawn_log::spawns_in(dir))
            .filter(|argv| argv.iter().any(|arg| arg == "reflog"))
            .filter(|argv| argv.iter().any(|arg| arg == "show"))
            .count()
    };

    // Five worktrees, bound 2: two HEAD logs and the default branch.
    let before = shows();
    let capped = ingest_reflogs(&anchor, &anchor, &family, 2, &Budget::unbounded());
    assert_eq!(shows() - before, 2 + 1, "{capped:?}");
    assert_eq!(capped.reflog_refs_skipped, 3, "{capped:?}");
    assert!(
        capped.truncated,
        "a family cut short must not read as complete: {capped:?}"
    );
    assert!(capped.error.contains("not read"), "{capped:?}");

    // Within the bound: every HEAD once and the default branch once, and
    // nothing new to record means no second look.
    let before = shows();
    let whole = ingest_reflogs(
        &anchor,
        &anchor,
        &family,
        MAX_REFLOG_WORKTREES,
        &Budget::unbounded(),
    );
    assert_eq!(shows() - before, 5 + 1, "{whole:?}");
    assert_eq!(whole.reflog_refs_skipped, 0, "{whole:?}");
    assert!(!whole.truncated, "{whole:?}");
    // The command-timings bench builds 18 worktrees and refuses a truncated
    // pass. A cap under that makes the bench fail before it measures anything.
    const _: () = assert!(
        MAX_REFLOG_WORKTREES >= 18,
        "command_timings fixture does not fit one catch-up pass"
    );
    let before = shows();
    let again = ingest_reflogs(
        &anchor,
        &anchor,
        &family,
        MAX_REFLOG_WORKTREES,
        &Budget::unbounded(),
    );
    assert_eq!(
        shows() - before,
        5 + 1,
        "a steady pass is one read per ref: {again:?}"
    );
    assert_eq!(again.recorded, 0, "{again:?}");
}

/// BREAK: the family is cut to the first `max` members in a fixed order on
/// every pass, so the members past the bound are never read, however many
/// passes run, and every pass reports `truncated` for ever. This repository's
/// own agent lanes keep more than sixteen worktrees alive.
#[test]
fn adv_a_worktree_past_the_reflog_bound_is_read_within_a_few_passes() {
    let (main, _parent, linked) = repo_with_worktrees(4);
    let anchor = plain(main.path());
    let last = linked[3].clone();
    commit_at(&last, "starved", T0 + 40, "gitpulse@test.local");
    let siblings: Vec<String> = linked
        .iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect();
    let family = Ok(siblings);
    let mut recorded_starved = false;
    for _ in 0..6 {
        ingest_reflogs(&anchor, &anchor, &family, 2, &Budget::unbounded());
        recorded_starved = reflog_rows(&anchor).iter().any(|event| {
            event
                .object
                .as_deref()
                .is_some_and(|object| object.contains("starved"))
        });
        if recorded_starved {
            break;
        }
    }
    assert!(
        recorded_starved,
        "six passes never reached the fifth worktree's HEAD"
    );
}
