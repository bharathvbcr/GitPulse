//! CARD 27 (ft-9d043f74...4671), S6-08 and S6-10: what reflog replay records
//! across passes, across a crash and across two processes. Each test names the
//! acceptance criterion it holds. They use real Git, with the committer date
//! pinned so identical entries land in one second.

use super::*;
use crate::procguard::LockedSpawn;
use crate::test_support::{commit_at, git_in};
use std::process::Stdio;

pub(super) const T0: i64 = 1_790_000_000;
const CHILD: &str = "ingest::reflog_replay_tests::replay_child";
const ENV_REPO: &str = "GITPULSE_TEST_REPLAY_REPO";
const ENV_GO: &str = "GITPULSE_TEST_REPLAY_GO";
const ENV_KILL: &str = "GITPULSE_TEST_KILL_AFTER_RECORDED";

/// Test-only: a child replay process kills itself (SIGKILL, no unwinding, no
/// destructors) once it has recorded the number of rows [`ENV_KILL`] names.
#[cfg(unix)]
pub(super) fn crash_point(recorded: i64) {
    let after = std::env::var(ENV_KILL)
        .ok()
        .and_then(|n| n.parse::<i64>().ok());
    if after == Some(recorded) {
        // SAFETY: a signal to this very process; SIGKILL cannot be handled.
        unsafe { libc::kill(libc::getpid(), libc::SIGKILL) };
    }
}

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
        "REPLAY recorded={} duplicates={} failed={}",
        out.recorded, out.duplicates_refused, out.failed_records
    );
}

/// A command that runs [`replay_child`] from a private copy of this harness.
/// The guard is returned: it deletes the copy, so it must outlive the child.
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

    // 1 seed + 4 commits + the first reset = 6 rows, then the kill lands
    // between the two identical resets.
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
    assert_eq!(reflog_rows(&repo).len(), 6, "the crash point moved");

    let next = ingest_reflog_into(&repo, &repo, 200);
    assert_eq!(next.recorded, i64::try_from(total - 6).unwrap(), "{next:?}");
    assert_eq!(next.duplicates_refused, 6, "{next:?}");
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
    // Let both reach their poll loop before releasing them together.
    std::thread::sleep(std::time::Duration::from_millis(1500));
    std::fs::write(&go, b"go").expect("release the children");
    let outputs: Vec<_> = children
        .into_iter()
        .map(|child| child.wait_with_output().expect("child output"))
        .collect();

    let mut recorded = 0;
    let mut duplicates = 0;
    for output in &outputs {
        assert!(
            output.status.success(),
            "a replay process failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        recorded += parsed(&output.stdout, "recorded");
        duplicates += parsed(&output.stdout, "duplicates");
        assert_eq!(parsed(&output.stdout, "failed"), 0);
    }
    let total = i64::try_from(total).unwrap();
    assert_eq!(recorded, total, "every entry once across both processes");
    assert_eq!(
        recorded + duplicates,
        2 * total,
        "every entry accounted for"
    );
    assert_eq!(
        i64::try_from(reflog_rows(&repo).len()).unwrap(),
        total,
        "one row per entry"
    );
    assert_eq!(ledger::status(&repo).dropped, 0);
}
