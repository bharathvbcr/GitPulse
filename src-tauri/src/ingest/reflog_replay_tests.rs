//! CARD 27 (ft-9d043f74...4671), S6-08 and S6-10: what reflog replay records
//! across passes, across a crash and across two processes. Each test names the
//! acceptance criterion it holds. They use real Git, with the committer date
//! pinned so identical entries land in one second.

use super::*;
use crate::procguard::LockedSpawn;
use crate::test_support::{commit_at, git_in};
#[cfg(unix)]
use std::process::Stdio;

pub(super) const T0: i64 = 1_790_000_000;
#[cfg(unix)]
const CHILD: &str = "ingest::reflog_replay_tests::replay_child";
const ENV_REPO: &str = "GITPULSE_TEST_REPLAY_REPO";
const ENV_GO: &str = "GITPULSE_TEST_REPLAY_GO";
#[cfg(unix)]
const ENV_KILL: &str = "GITPULSE_TEST_KILL_AFTER_RECORDED";

/// `git` in `dir` with author and committer dated `epoch`.
pub(super) fn git_at(dir: &std::path::Path, args: &[&str], epoch: i64) {
    let stamp = format!("@{epoch} +0000");
    let output = std::process::Command::new("git")
        .args([
            "-c",
            "user.name=GitPulse",
            "-c",
            "user.email=gitpulse@test.local",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .env("GIT_AUTHOR_DATE", &stamp)
        .env("GIT_COMMITTER_DATE", &stamp)
        .current_dir(dir)
        .output_locked()
        .expect("spawn git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

pub(super) fn seeded_repo() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().expect("repo");
    git_in(dir.path(), &["init", "-b", "main"]);
    commit_at(dir.path(), "seed", T0, "gitpulse@test.local");
    let repo = dir
        .path()
        .canonicalize()
        .expect("canonical")
        .to_string_lossy()
        .into_owned();
    (dir, repo)
}

pub(super) fn reflog_rows(repo: &str) -> Vec<ledger::LedgerEvent> {
    ledger::tail(repo, 0, 100_000)
        .expect("ledger")
        .into_iter()
        .filter(|event| event.action.starts_with("reflog."))
        .collect()
}

fn head_entries(repo: &str) -> usize {
    crate::engine::git_reader::GitReader::get_reflog(repo, 10_000)
        .expect("reflog")
        .len()
}

/// S6-08: two identical checkouts are recorded once each across passes.
/// Before the fix the second `reset` of the same second was skipped because a
/// row with the same commit and message already existed.
#[test]
fn two_identical_checkouts_are_recorded_once_each_across_passes() {
    let (dir, repo) = seeded_repo();
    git_at(dir.path(), &["reset", "--hard", "HEAD"], T0 + 10);
    let first = ingest_reflog_into(&repo, &repo, 200);
    assert_eq!(first.recorded, 2, "{first:?}");

    git_at(dir.path(), &["reset", "--hard", "HEAD"], T0 + 10);
    let second = ingest_reflog_into(&repo, &repo, 200);
    assert_eq!(
        second.recorded, 1,
        "the second identical entry was lost: {second:?}"
    );

    let resets = reflog_rows(&repo)
        .into_iter()
        .filter(|event| event.action == "reflog.reset")
        .count();
    assert_eq!(resets, 2, "each reset is its own row");
    let third = ingest_reflog_into(&repo, &repo, 200);
    assert_eq!(
        (third.recorded, third.duplicates_refused),
        (0, 0),
        "{third:?}"
    );
    assert_eq!(reflog_rows(&repo).len(), head_entries(&repo));
}

/// S6-08: identical entries inside one pass are both recorded as well.
#[test]
fn identical_entries_in_one_pass_are_both_recorded() {
    let (dir, repo) = seeded_repo();
    for _ in 0..3 {
        git_at(dir.path(), &["reset", "--hard", "HEAD"], T0 + 10);
    }
    let out = ingest_reflog_into(&repo, &repo, 200);
    assert_eq!(out.recorded, 4, "{out:?}");
    assert_eq!(reflog_rows(&repo).len(), head_entries(&repo));
}

/// S6-08: 250 entries between passes are all recorded, or `truncated` with
/// counts, and never reported complete. The replay records the oldest 200,
/// says 50 are pending, and the next pass finishes them with no gap.
#[test]
fn two_hundred_fifty_entries_between_passes_are_all_recorded_or_counted() {
    let (dir, repo) = seeded_repo();
    let first = ingest_reflog_into(&repo, &repo, 200);
    assert_eq!(first.recorded, 1, "{first:?}");
    assert!(!first.truncated, "{first:?}");

    for n in 0..250 {
        commit_at(
            dir.path(),
            &format!("c{n}"),
            T0 + 100 + n,
            "gitpulse@test.local",
        );
    }
    let second = ingest_reflog_into(&repo, &repo, 200);
    let recorded_now = i64::try_from(reflog_rows(&repo).len()).unwrap() - 1;
    if second.recorded < 250 {
        assert!(
            second.truncated,
            "entries were left and the pass said nothing: {second:?}"
        );
        assert_eq!(
            second.recorded + second.reflog_pending,
            250,
            "the counts do not account for every entry: {second:?}"
        );
        assert!(second.error.contains("truncated"), "{second:?}");
    }
    assert_eq!(recorded_now, second.recorded, "{second:?}");

    let third = ingest_reflog_into(&repo, &repo, 200);
    assert!(!third.truncated, "{third:?}");
    let commits = reflog_rows(&repo)
        .into_iter()
        .filter(|event| event.action == "reflog.commit")
        .count();
    assert_eq!(commits, 250, "an entry was lost between passes");
    assert_eq!(reflog_rows(&repo).len(), head_entries(&repo));
}

/// S6-08: `before_ref` is the commit the ref held before the move.
#[test]
fn before_ref_names_the_previous_commit() {
    let (dir, repo) = seeded_repo();
    commit_at(dir.path(), "second", T0 + 5, "gitpulse@test.local");
    commit_at(dir.path(), "third", T0 + 6, "gitpulse@test.local");
    let entries = crate::engine::git_reader::GitReader::get_reflog(&repo, 200).expect("reflog");
    let oid = |index: usize| entries[index].commit_id.clone();
    let out = ingest_reflog_into(&repo, &repo, 200);
    assert_eq!(out.recorded, 3, "{out:?}");

    let rows = reflog_rows(&repo);
    let before_of = |commit: &str| {
        rows.iter()
            .find(|event| event.after_ref.as_deref() == Some(commit))
            .unwrap_or_else(|| panic!("no row for {commit}"))
            .before_ref
            .clone()
    };
    assert_eq!(before_of(&oid(0)), Some(oid(1)));
    assert_eq!(before_of(&oid(1)), Some(oid(2)));
    assert_eq!(
        before_of(&oid(2)),
        None,
        "the first entry has no earlier value"
    );
}

/// The window's oldest entry gets its before-image from the one past the
/// window, which is read but never recorded.
#[test]
fn before_ref_survives_the_window_edge() {
    let (dir, repo) = seeded_repo();
    for n in 0..5 {
        commit_at(
            dir.path(),
            &format!("c{n}"),
            T0 + 10 + n,
            "gitpulse@test.local",
        );
    }
    let entries = crate::engine::git_reader::GitReader::get_reflog(&repo, 200).expect("reflog");
    let out = ingest_reflog_into(&repo, &repo, 3);
    assert_eq!(out.recorded, 3, "{out:?}");
    assert!(out.truncated && out.reflog_unreached, "{out:?}");
    let rows = reflog_rows(&repo);
    let oldest = rows
        .iter()
        .find(|event| event.after_ref.as_deref() == Some(entries[2].commit_id.as_str()))
        .expect("the oldest recorded row");
    assert_eq!(
        oldest.before_ref.as_deref(),
        Some(entries[3].commit_id.as_str())
    );
}

/// The child half of the crash and two-process tests: replays HEAD's reflog,
/// dying at [`ENV_KILL`] if set, after the [`ENV_GO`] file appears.
#[test]
fn replay_child() {
    let Ok(repo) = std::env::var(ENV_REPO) else {
        return;
    };
    crate::test_support::trust_repo(std::path::Path::new(&repo));
    if let Ok(go) = std::env::var(ENV_GO) {
        // Tell the parent this process is at its poll loop, so the parent
        // releases both together instead of guessing how long a start takes.
        std::fs::write(format!("{go}.ready-{}", std::process::id()), b"")
            .expect("announce readiness");
        let deadline = std::time::Instant::now()
            + crate::test_support::coverage_relaxed(std::time::Duration::from_secs(60));
        while !std::path::Path::new(&go).exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "the go signal never came"
            );
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }
    let out = ingest_reflog_into(&repo, &repo, 200);
    println!(
        "REPLAY recorded={} duplicates={} skipped={} failed={}",
        out.recorded, out.duplicates_refused, out.reflog_skipped, out.failed_records
    );
    println!("REPLAY-DETAIL {out:?}");
}

/// A command that runs [`replay_child`] from a private copy of this harness.
/// The guard is returned: it deletes the copy, so it must outlive the child.
#[cfg(unix)]
fn child_command(
    repo: &str,
) -> (
    std::process::Command,
    crate::test_support::IsolatedHarnessGuard,
) {
    let (command, harness) = crate::test_support::isolated_libtest_command(CHILD);
    let mut command = std::process::Command::new(command.get_program());
    command
        .args(["--exact", CHILD, "--nocapture"])
        .env(ENV_REPO, repo)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    (command, harness)
}

#[cfg(unix)]
fn parsed(stdout: &[u8], key: &str) -> i64 {
    let text = String::from_utf8_lossy(stdout);
    text.split("REPLAY ")
        .nth(1)
        .and_then(|rest| rest.lines().next())
        .and_then(|line| {
            line.split_whitespace()
                .find_map(|part| part.strip_prefix(&format!("{key}=")))
        })
        .and_then(|value| value.parse().ok())
        .unwrap_or_else(|| panic!("the child printed no {key}: {text}"))
}

/// S6-08: a process killed mid-replay (SIGKILL after 6 rows, between the two
/// identical resets) leaves no duplicate and no gap for the next pass.
#[cfg(unix)]
#[test]
fn a_replay_killed_mid_pass_leaves_no_duplicates_and_no_gaps() {
    use std::os::unix::process::ExitStatusExt;
    let (dir, repo) = seeded_repo();
    for n in 0..4 {
        commit_at(
            dir.path(),
            &format!("a{n}"),
            T0 + 1 + n,
            "gitpulse@test.local",
        );
    }
    git_at(dir.path(), &["reset", "--hard", "HEAD"], T0 + 50);
    git_at(dir.path(), &["reset", "--hard", "HEAD"], T0 + 50);
    for n in 0..4 {
        commit_at(
            dir.path(),
            &format!("b{n}"),
            T0 + 60 + n,
            "gitpulse@test.local",
        );
    }
    let total = head_entries(&repo);

    // The kill lands after the sixth row (1 seed + 4 commits + the first reset),
    // between the two identical resets, inside the one transaction that holds
    // the rows and the watermark over them: neither survives it.
    let (mut command, _harness) = child_command(&repo);
    let output = command
        .env(ENV_KILL, "6")
        .output_locked()
        .expect("run the child");
    assert_eq!(
        output.status.signal(),
        Some(libc::SIGKILL),
        "the child was meant to die by SIGKILL: {:?} {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        reflog_rows(&repo).len(),
        0,
        "a killed transaction left rows behind"
    );

    let next = ingest_reflog_into(&repo, &repo, 200);
    assert_eq!(next.recorded, i64::try_from(total).unwrap(), "{next:?}");
    assert_eq!(next.duplicates_refused, 0, "{next:?}");
    assert_eq!(reflog_rows(&repo).len(), total, "a gap or a duplicate");
    let resets = reflog_rows(&repo)
        .into_iter()
        .filter(|event| event.action == "reflog.reset")
        .count();
    assert_eq!(resets, 2, "the identical reset after the kill was lost");
    let again = ingest_reflog_into(&repo, &repo, 200);
    assert_eq!(
        (again.recorded, again.duplicates_refused),
        (0, 0),
        "{again:?}"
    );
}

/// Every replayed entry is accounted for by exactly one of recorded, refused by
/// the ledger as a duplicate, or skipped because the pass saw it already
/// recorded: the three are what the two-process test sums, and this pins each
/// one on its own, with no timing in it.
#[test]
fn a_replayed_entry_is_recorded_refused_or_skipped_and_never_lost() {
    let (dir, repo) = seeded_repo();
    for n in 0..5 {
        commit_at(
            dir.path(),
            &format!("c{n}"),
            T0 + 1 + n,
            "gitpulse@test.local",
        );
    }
    let entries = crate::engine::git_reader::GitReader::get_reflog(&repo, 200).expect("reflog");
    let total = i64::try_from(entries.len()).unwrap();
    let replay = |seen: std::collections::HashSet<String>| {
        replay_reflog(&repo, &repo, entries.clone(), Ok(seen), CatchUp::default())
    };
    let tally = |out: &CatchUp| (out.recorded, out.duplicates_refused, out.reflog_skipped);

    let first = replay(std::collections::HashSet::new());
    assert_eq!(tally(&first), (total, 0, 0), "{first:?}");
    // A pass that built its seen-set before the first one wrote: the ledger
    // itself refuses every copy.
    let raced = replay(std::collections::HashSet::new());
    assert_eq!(tally(&raced), (0, total, 0), "{raced:?}");
    // A pass that found their identities already held (an older build's untimed
    // rows are what that set holds): it never offers them.
    let seen = entries
        .iter()
        .map(|entry| ledger::stored_object(&format!("{} {}", entry.commit_id, entry.message)))
        .collect();
    let late = replay(seen);
    assert_eq!(tally(&late), (0, 0, total), "{late:?}");
    assert_eq!(reflog_rows(&repo).len(), entries.len(), "one row per entry");
}

/// S6-10: two processes replaying one reflog at the same moment record exactly
/// one row per entry; the unique identity index refuses the other's copy.
#[cfg(unix)]
#[test]
fn two_processes_replaying_at_once_record_exactly_one_row_per_entry() {
    let (dir, repo) = seeded_repo();
    for n in 0..60 {
        commit_at(
            dir.path(),
            &format!("c{n}"),
            T0 + 1 + n / 3,
            "gitpulse@test.local",
        );
    }
    git_at(dir.path(), &["reset", "--hard", "HEAD"], T0 + 500);
    git_at(dir.path(), &["reset", "--hard", "HEAD"], T0 + 500);
    let total = head_entries(&repo);
    let signal = tempfile::tempdir().expect("signal dir");
    let go = signal.path().join("go");

    let (children, _harnesses): (Vec<_>, Vec<_>) = (0..2)
        .map(|_| {
            let (mut command, harness) = child_command(&repo);
            let child = command
                .env(ENV_GO, &go)
                .spawn_locked()
                .expect("spawn child");
            (child, harness)
        })
        .unzip();
    // Release them together once both are at their poll loop. A fixed sleep
    // here let a child that started late (a loaded host) begin after the other
    // had finished, which is a sequential replay and not the contest this test
    // is about.
    let ready_prefix = format!("{}.ready-", go.file_name().unwrap().to_string_lossy());
    let ready = || {
        std::fs::read_dir(signal.path())
            .expect("read the signal dir")
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(&ready_prefix)
            })
            .count()
    };
    let waiting_until = std::time::Instant::now()
        + crate::test_support::coverage_relaxed(std::time::Duration::from_secs(60));
    while ready() < 2 {
        assert!(
            std::time::Instant::now() < waiting_until,
            "only {} of 2 replay children reached their poll loop",
            ready()
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    std::fs::write(&go, b"go").expect("release the children");
    let outputs: Vec<_> = children
        .into_iter()
        .map(|child| child.wait_with_output().expect("child output"))
        .collect();

    let mut recorded = 0;
    let mut duplicates = 0;
    let mut skipped = 0;
    for output in &outputs {
        assert!(
            output.status.success(),
            "a replay process failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        recorded += parsed(&output.stdout, "recorded");
        duplicates += parsed(&output.stdout, "duplicates");
        skipped += parsed(&output.stdout, "skipped");
        assert_eq!(parsed(&output.stdout, "failed"), 0);
    }
    let total = i64::try_from(total).unwrap();
    assert_eq!(recorded, total, "every entry once across both processes");
    // Every entry is handled once by each process, and exactly one of three
    // things happens to it: recorded, refused by the ledger as a duplicate (a
    // raced copy), or skipped because the process saw it already recorded when
    // it looked. Which of the last two a loser reports depends on timing, so
    // they are summed; the total is not.
    assert_eq!(
        recorded + duplicates + skipped,
        2 * total,
        "every entry accounted for: recorded {recorded} + duplicates {duplicates} + skipped {skipped}\n{}",
        outputs
            .iter()
            .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
            .collect::<Vec<_>>()
            .join("\n")
    );
    assert_eq!(
        i64::try_from(reflog_rows(&repo).len()).unwrap(),
        total,
        "one row per entry"
    );
    assert_eq!(ledger::status(&repo).dropped, 0);
}

// --- adversarial review of CARD 27 (branch b17/card27-adv) ------------------

/// A reflog row as a generation-1 build wrote it: no event time, so no
/// `occurred_at`, which the unique identity index cannot match.
fn legacy_row(repo: &str, entry: &crate::engine::git_reader::ReflogEntry) {
    let action = format!("reflog.{}", ledger::action_segment(&entry.action));
    ledger::append(Draft {
        repo_path: repo.to_string(),
        action,
        object: Some(format!("{} {}", entry.commit_id, entry.message)),
        after_ref: Some(entry.commit_id.clone()),
        actor_kind: Some(ActorKind::System),
        actor_id: Some("reflog".into()),
        outcome: Some(Outcome::Ok),
        ..Default::default()
    })
    .expect("a generation-1 row");
}

/// Held: a ledger whose old reflog rows carry no `occurred_at` (written before
/// replay typed its time) is not recorded a second time on the first replay.
#[test]
fn adv_held_legacy_rows_without_a_time_are_not_recorded_again() {
    let (dir, repo) = seeded_repo();
    commit_at(dir.path(), "later", T0 + 5, "gitpulse@test.local");
    let entries = crate::engine::git_reader::GitReader::get_reflog(&repo, 200).expect("reflog");
    for entry in entries.iter().rev() {
        legacy_row(&repo, entry);
    }
    let out = ingest_reflog_into(&repo, &repo, 200);
    assert_eq!(out.recorded, 0, "{out:?}");
    assert_eq!(reflog_rows(&repo).len(), entries.len());
}

/// BREAK: a generation-1 process that was already running when the ledger was
/// migrated keeps writing reflog rows with no `occurred_at` (the WRITER_VERSION
/// doc says so). Once a watermark exists the replay reads no legacy rows
/// (`found.is_some()` empties the seen set), so an entry the old process
/// recorded after the watermark is recorded a second time by the new build.
#[test]
fn adv_a_generation_1_row_written_after_the_watermark_is_not_recorded_again() {
    let (dir, repo) = seeded_repo();
    let first = ingest_reflog_into(&repo, &repo, 200);
    assert_eq!(first.recorded, 1, "{first:?}");
    commit_at(dir.path(), "later", T0 + 5, "gitpulse@test.local");
    let entries = crate::engine::git_reader::GitReader::get_reflog(&repo, 200).expect("reflog");
    legacy_row(&repo, &entries[0]);

    let second = ingest_reflog_into(&repo, &repo, 200);
    let later = reflog_rows(&repo)
        .into_iter()
        .filter(|event| event.object.as_deref().is_some_and(|o| o.contains("later")))
        .count();
    assert_eq!(later, 1, "the entry is in the ledger twice: {second:?}");
}

/// BREAK: an entry whose time Git recorded more than a minute ahead of the
/// clock (or not at all) is stamped with ingest time, which is part of the
/// unique identity, so nothing refuses its second recording. A process killed
/// after the last row and before the watermark write (a separate statement)
/// leaves the entry to be recorded again by the next pass.
///
/// The kill is modelled by removing the watermark row after a completed pass:
/// the state a process leaves if it dies between the last insert and the
/// watermark write. (A self-sent SIGKILL is not instantaneous, so a child that
/// "dies" there sometimes still writes the watermark and the test flakes.)
#[test]
fn adv_a_future_dated_entry_is_not_recorded_twice_after_a_kill_before_the_watermark() {
    let (dir, repo) = seeded_repo();
    commit_at(dir.path(), "future", 4_100_000_000, "gitpulse@test.local");
    let total = head_entries(&repo);
    let first = ingest_reflog_into(&repo, &repo, 200);
    assert_eq!(first.recorded, i64::try_from(total).unwrap(), "{first:?}");
    let db = dir.path().join(".devcouncil").join("ledger.sqlite");
    rusqlite::Connection::open(db)
        .expect("open the ledger file")
        .execute("DELETE FROM reflog_watermarks", [])
        .expect("drop the watermark");

    std::thread::sleep(std::time::Duration::from_millis(20));
    let next = ingest_reflog_into(&repo, &repo, 200);
    assert_eq!(
        reflog_rows(&repo).len(),
        total,
        "the future-dated entry was recorded again: {next:?}"
    );
}

/// BREAK: occurrence numbers are counted inside the read, so an identical
/// entry whose older twin lies beyond the window is numbered 0 when it is 1,
/// and the unique index refuses it as the twin. Reflog times are not
/// monotonic after a clock step or a pinned committer date; here the later
/// reset carries the earlier second, which leaves the window's skipped-second
/// guard (it only skips the extra entry's own second) looking at the wrong run.
#[test]
#[ignore = "known limit: an identical entry with an older twin outside the read gets ordinal 0 and is refused as the twin"]
fn adv_a_backdated_twin_outside_the_window_keeps_its_ordinal() {
    let (dir, repo) = seeded_repo();
    let seed = crate::engine::git_reader::GitReader::get_reflog(&repo, 10)
        .expect("reflog")
        .remove(0)
        .commit_id;
    git_at(dir.path(), &["reset", "--hard", &seed], T0 + 10);
    commit_at(dir.path(), "g", T0 + 90, "gitpulse@test.local");
    commit_at(dir.path(), "e2", T0 + 100, "gitpulse@test.local");
    let first = ingest_reflog_into(&repo, &repo, 200);
    assert_eq!(first.recorded, 4, "{first:?}");

    git_at(dir.path(), &["reset", "--hard", &seed], T0 + 10);
    commit_at(dir.path(), "f", T0 + 110, "gitpulse@test.local");
    let second = ingest_reflog_into(&repo, &repo, 3);
    assert_eq!(
        reflog_rows(&repo).len(),
        head_entries(&repo),
        "an entry was refused as its own older twin: {second:?}"
    );
}

/// BREAK: `git reflog delete` of one of several identical entries renumbers
/// the rest, so the watermark's ordinal names another entry and a later
/// identical entry takes a number that is already in the ledger.
#[test]
#[ignore = "known limit: deleting one identical reflog entry shifts ordinals; the next identical entry is skipped as already recorded"]
fn adv_deleting_an_identical_entry_does_not_swallow_the_next_one() {
    let (dir, repo) = seeded_repo();
    git_at(dir.path(), &["reset", "--hard", "HEAD"], T0 + 10);
    git_at(dir.path(), &["reset", "--hard", "HEAD"], T0 + 10);
    let first = ingest_reflog_into(&repo, &repo, 200);
    assert_eq!(first.recorded, 3, "{first:?}");
    git_in(dir.path(), &["reflog", "delete", "HEAD@{1}"]);
    git_at(dir.path(), &["reset", "--hard", "HEAD"], T0 + 10);
    let second = ingest_reflog_into(&repo, &repo, 200);
    let resets = reflog_rows(&repo)
        .into_iter()
        .filter(|event| event.action == "reflog.reset")
        .count();
    assert_eq!(resets, 3, "three resets happened: {second:?}");
}
