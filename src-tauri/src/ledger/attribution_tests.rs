//! Commit-to-session attribution: a transcript's `git commit` command and the
//! reflog entry that followed it. Rows go in through the ledger's own writer
//! (`append`), stamped the way ingest stamps them, so what is matched is what a
//! real catch-up writes.

use super::attribution::{
    basis, link_commits, runs_git_mutation, CommitLink, LINK_MAX_COMMITS, LINK_SCAN_ROWS,
    LINK_WINDOW_MS,
};
use super::*;
use std::path::Path;

const SEC: u64 = 1_000;

fn repo_dir() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().to_str().unwrap().to_string();
    (dir, repo)
}

/// "Now", to the second: git's own resolution for the reflog entry, and close
/// enough to the append clock that a row written by a test is not older than
/// the window it is asked about.
fn t0() -> u64 {
    ids::now_millis() / SEC * SEC
}

fn sha(n: usize) -> String {
    format!("{n:040x}")
}

fn stamp(ms: u64, basis: &str) -> EventStamp {
    EventStamp {
        occurred_at: ids::iso8601_utc(ms),
        occurred_at_basis: basis.into(),
        task_basis: "unbound".into(),
    }
}

/// A transcript `session.command` row.
fn command_row(repo: &str, worktree: Option<&str>, session: &str, text: &str, at: u64) {
    append(Draft {
        repo_path: repo.into(),
        worktree_path: worktree.map(str::to_string),
        actor_kind: Some(ActorKind::Agent),
        actor_id: Some("claude".into()),
        session_id: Some(session.into()),
        action: "session.command".into(),
        object: Some(text.into()),
        outcome: Some(Outcome::Ok),
        detail_json: Some(r#"{"result":"ok"}"#.into()),
        event_stamp: Some(stamp(at, "transcript")),
        ..Draft::default()
    })
    .expect("append command");
}

/// A reflog row: what replay writes for a ref move, git's own second.
fn reflog_row(repo: &str, worktree: Option<&str>, commit: &str, verb: &str, at: u64) {
    // A bare verb is a commit-making entry (`commit: work`); anything with a
    // colon is git's own message, as `git reflog --format=%gs` prints it.
    let message = if verb.contains(':') {
        verb.to_string()
    } else {
        format!("{verb}: work")
    };
    let mut action = action_segment(message.split(':').next().unwrap_or(""));
    if !action.starts_with(|c: char| c.is_ascii_lowercase()) {
        action = "move".into();
    }
    append(Draft {
        repo_path: repo.into(),
        worktree_path: worktree.map(str::to_string),
        actor_kind: Some(ActorKind::System),
        actor_id: Some("reflog".into()),
        action: format!("reflog.{action}"),
        object: Some(format!("{commit} {message}")),
        outcome: Some(Outcome::Ok),
        after_ref: Some(commit.into()),
        event_stamp: Some(stamp(at, "reflog")),
        ..Draft::default()
    })
    .expect("append reflog");
}

fn link(repo: &str, commit: &str) -> CommitLink {
    link_commits(repo, &[commit.to_string()])
        .remove(commit)
        .expect("every asked commit is answered")
}

#[test]
fn a_bash_git_commit_and_its_reflog_entry_are_linked_as_inferred() {
    let (_dir, repo) = repo_dir();
    let at = t0();
    command_row(&repo, None, "S1", "git commit -m 'fix it'", at - 3 * SEC);
    reflog_row(&repo, None, &sha(1), "commit", at);

    let got = link(&repo, &sha(1));

    assert_eq!(got.session_id.as_deref(), Some("S1"), "{got:?}");
    assert_eq!(got.basis, basis::INFERRED);
    assert!(!got.ambiguous);
    assert_eq!(got.window_ms, LINK_WINDOW_MS);
    assert_eq!(got.command_at_ms, Some(at - 3 * SEC));
    assert_eq!(got.command.as_deref(), Some("git commit -m 'fix it'"));
}

#[test]
fn a_hand_commit_with_no_session_command_stays_unattributed() {
    let (_dir, repo) = repo_dir();
    let at = t0();
    // Other commands in the window are not git commits.
    command_row(&repo, None, "S1", "cargo test", at - 3 * SEC);
    command_row(&repo, None, "S1", "git status", at - 2 * SEC);
    reflog_row(&repo, None, &sha(1), "commit", at);

    let got = link(&repo, &sha(1));

    assert_eq!(got.session_id, None, "{got:?}");
    assert_eq!(got.basis, basis::NO_SESSION_COMMAND);
    assert!(!got.ambiguous, "nothing was ambiguous: nothing fit");
}

#[test]
fn two_candidate_sessions_in_the_window_give_none_and_ambiguous() {
    let (_dir, repo) = repo_dir();
    let at = t0();
    command_row(&repo, None, "S1", "git commit -m a", at - 10 * SEC);
    command_row(&repo, None, "S2", "git commit -m b", at - 2 * SEC);
    reflog_row(&repo, None, &sha(1), "commit", at);

    let got = link(&repo, &sha(1));

    assert_eq!(got.session_id, None, "never a guess between two: {got:?}");
    assert!(got.ambiguous);
    assert_eq!(got.basis, basis::AMBIGUOUS);
}

#[test]
fn the_window_is_inclusive_at_its_edge_and_closed_just_past_it() {
    let (_dir, repo) = repo_dir();
    let at = t0();
    command_row(&repo, None, "EDGE", "git commit -m a", at - LINK_WINDOW_MS);
    reflog_row(&repo, None, &sha(1), "commit", at);
    assert_eq!(
        link(&repo, &sha(1)).session_id.as_deref(),
        Some("EDGE"),
        "a command exactly one window before the entry still fits"
    );

    let (_dir2, repo2) = repo_dir();
    command_row(
        &repo2,
        None,
        "LATE",
        "git commit -m a",
        at - LINK_WINDOW_MS - SEC,
    );
    reflog_row(&repo2, None, &sha(2), "commit", at);
    let got = link(&repo2, &sha(2));
    assert_eq!(got.session_id, None, "{got:?}");
    assert_eq!(
        got.basis,
        basis::COMMAND_OUTSIDE_WINDOW,
        "a command that came too early says so, not 'no command'"
    );
}

#[test]
fn a_command_in_a_different_worktree_does_not_link() {
    let (_dir, repo) = repo_dir();
    let at = t0();
    command_row(&repo, Some("/w/other"), "S1", "git commit -m a", at - SEC);
    reflog_row(&repo, Some("/w/mine"), &sha(1), "commit", at);
    // The anchor checkout (a NULL worktree) is a worktree of its own.
    command_row(&repo, None, "S2", "git commit -m b", at - SEC);

    let got = link(&repo, &sha(1));

    assert_eq!(got.session_id, None, "{got:?}");
    assert_eq!(got.basis, basis::NO_SESSION_COMMAND);
}

#[test]
fn a_command_dated_after_the_entry_is_clock_skew_not_a_link() {
    let (_dir, repo) = repo_dir();
    let at = t0();
    command_row(&repo, None, "S1", "git commit -m a", at + 5 * SEC);
    reflog_row(&repo, None, &sha(1), "commit", at);

    let got = link(&repo, &sha(1));

    assert_eq!(got.session_id, None, "{got:?}");
    assert_eq!(got.basis, basis::COMMAND_AFTER_REFLOG);

    // Git writes the entry to the whole second, so a command that started
    // later in that same second is not after its own commit.
    let (_dir2, repo2) = repo_dir();
    command_row(&repo2, None, "S1", "git commit -m a", at + 999);
    reflog_row(&repo2, None, &sha(2), "commit", at);
    assert_eq!(link(&repo2, &sha(2)).session_id.as_deref(), Some("S1"));
}

#[test]
fn a_row_with_no_event_time_cannot_be_matched_and_says_so() {
    let (_dir, repo) = repo_dir();
    let at = t0();
    // An old build's row: no stamp, so no `occurred_at` anywhere.
    append(Draft {
        repo_path: repo.clone(),
        actor_kind: Some(ActorKind::Agent),
        session_id: Some("OLD".into()),
        action: "session.command".into(),
        object: Some("git commit -m a".into()),
        outcome: Some(Outcome::Ok),
        ..Draft::default()
    })
    .unwrap();
    // A row dated by ingest rather than by the transcript is as untimed.
    append(Draft {
        repo_path: repo.clone(),
        actor_kind: Some(ActorKind::Agent),
        session_id: Some("INGEST".into()),
        action: "session.command".into(),
        object: Some("git commit -m b".into()),
        outcome: Some(Outcome::Ok),
        event_stamp: Some(stamp(at - SEC, "ingest")),
        ..Draft::default()
    })
    .unwrap();
    reflog_row(&repo, None, &sha(1), "commit", at);

    let got = link(&repo, &sha(1));

    assert_eq!(
        got.session_id, None,
        "an untimed row is never matched: {got:?}"
    );
    assert_eq!(got.basis, basis::COMMAND_UNTIMED);

    // An untimed row appended long before the window opened cannot have
    // made a commit that came after it.
    with_conn(&repo, |conn| {
        conn.execute(
            "UPDATE events SET ts_utc = '2020-01-01T00:00:00.000Z'
             WHERE action = 'session.command'",
            [],
        )
        .map_err(|e| LedgerError::new("t", e.to_string()))
    })
    .unwrap();
    assert_eq!(link(&repo, &sha(1)).basis, basis::NO_SESSION_COMMAND);
}

#[test]
fn an_amend_and_a_rebase_each_entry_links_to_the_session_that_ran_them() {
    let (_dir, repo) = repo_dir();
    let at = t0();
    command_row(&repo, None, "S1", "git commit -m a", at - 2 * SEC);
    reflog_row(&repo, None, &sha(1), "commit", at);
    command_row(
        &repo,
        None,
        "S1",
        "git commit --amend --no-edit",
        at + 18 * SEC,
    );
    reflog_row(&repo, None, &sha(2), "commit (amend): work", at + 20 * SEC);

    let first = link(&repo, &sha(1));
    let amended = link(&repo, &sha(2));
    assert_eq!(first.session_id.as_deref(), Some("S1"));
    assert_eq!(amended.session_id.as_deref(), Some("S1"));
    assert!(!amended.ambiguous, "one session twice is one session");
    assert_eq!(
        amended.command.as_deref(),
        Some("git commit --amend --no-edit"),
        "the nearest of that session's commands is the one named"
    );

    // One rebase command, then a reflog entry per pick and one for the finish.
    let (_dir2, repo2) = repo_dir();
    command_row(
        &repo2,
        None,
        "S9",
        &format!("cd {repo2} && git -C {repo2} rebase main"),
        at,
    );
    for n in 0..6 {
        reflog_row(
            &repo2,
            None,
            &sha(100 + n),
            "rebase (pick): work",
            at + (n as u64 + 1) * SEC,
        );
    }
    let all: Vec<String> = (0..6).map(|n| sha(100 + n)).collect();
    let linked = link_commits(&repo2, &all);
    assert_eq!(linked.len(), 6);
    for (commit, got) in &linked {
        assert_eq!(got.session_id.as_deref(), Some("S9"), "{commit}: {got:?}");
    }
}

#[test]
fn an_amend_by_a_second_session_inside_the_window_is_ambiguous_not_guessed() {
    let (_dir, repo) = repo_dir();
    let at = t0();
    command_row(&repo, None, "S1", "git commit -m a", at - 2 * SEC);
    command_row(
        &repo,
        None,
        "S2",
        "git commit --amend --no-edit",
        at + 18 * SEC,
    );
    reflog_row(&repo, None, &sha(2), "commit (amend): work", at + 20 * SEC);

    let got = link(&repo, &sha(2));

    assert!(got.ambiguous, "{got:?}");
    assert_eq!(got.session_id, None);
}

#[test]
fn a_blocked_command_and_a_command_with_no_session_are_not_candidates() {
    let (_dir, repo) = repo_dir();
    let at = t0();
    append(Draft {
        repo_path: repo.clone(),
        actor_kind: Some(ActorKind::Agent),
        session_id: Some("S1".into()),
        action: "session.command".into(),
        object: Some("git commit -m a".into()),
        outcome: Some(Outcome::Blocked),
        event_stamp: Some(stamp(at - SEC, "transcript")),
        ..Draft::default()
    })
    .unwrap();
    append(Draft {
        repo_path: repo.clone(),
        actor_kind: Some(ActorKind::Agent),
        action: "session.command".into(),
        object: Some("git commit -m b".into()),
        outcome: Some(Outcome::Ok),
        event_stamp: Some(stamp(at - SEC, "transcript")),
        ..Draft::default()
    })
    .unwrap();
    reflog_row(&repo, None, &sha(1), "commit", at);

    let got = link(&repo, &sha(1));

    assert_eq!(got.session_id, None, "{got:?}");
    assert_eq!(got.basis, basis::NO_SESSION_COMMAND);
}

#[test]
fn the_entry_that_made_a_commit_is_the_earliest_one_naming_it() {
    let (_dir, repo) = repo_dir();
    let at = t0();
    command_row(&repo, Some("/w/a"), "S1", "git commit -m a", at - SEC);
    reflog_row(&repo, Some("/w/a"), &sha(1), "commit", at);
    // A fast-forward in another worktree, much later, onto the same commit.
    command_row(&repo, Some("/w/b"), "S2", "git pull", at + 3_600 * SEC);
    reflog_row(
        &repo,
        Some("/w/b"),
        &sha(1),
        "pull: Fast-forward",
        at + 3_601 * SEC,
    );

    let got = link(&repo, &sha(1));

    assert_eq!(got.session_id.as_deref(), Some("S1"), "{got:?}");
}

#[test]
fn a_commit_the_ledger_never_replayed_is_not_called_unattributed() {
    let (_dir, repo) = repo_dir();
    command_row(&repo, None, "S1", "git commit -m a", t0());

    assert_eq!(link(&repo, &sha(7)).basis, basis::NO_REFLOG_ENTRY);

    // A reflog entry git dated 0 or never dated has no time to match on.
    append(Draft {
        repo_path: repo.clone(),
        actor_kind: Some(ActorKind::System),
        action: "reflog.commit".into(),
        object: Some(format!("{} commit: x", sha(8))),
        outcome: Some(Outcome::Ok),
        after_ref: Some(sha(8)),
        event_stamp: Some(stamp(t0(), "ingest")),
        ..Draft::default()
    })
    .unwrap();
    assert_eq!(link(&repo, &sha(8)).basis, basis::REFLOG_UNTIMED);
}

#[test]
fn there_is_no_ledger_to_read_and_none_is_created() {
    let (dir, repo) = repo_dir();

    let got = link(&repo, &sha(1));

    assert_eq!(got.basis, basis::LEDGER_ABSENT);
    assert!(
        !dir.path().join(".devcouncil").exists(),
        "a read must not create the ledger"
    );
}

#[test]
fn a_read_that_hit_its_row_bound_says_so_instead_of_no_session() {
    let (_dir, repo) = repo_dir();
    let at = t0();
    command_row(&repo, None, "S1", "git commit -m a", at - SEC);
    reflog_row(&repo, None, &sha(1), "commit", at);
    with_conn(&repo, |conn| {
        let tx = conn
            .unchecked_transaction()
            .map_err(|e| LedgerError::new("t", e.to_string()))?;
        {
            let mut insert = tx
                .prepare(
                    "INSERT INTO events (ulid, ts_utc, repo_path, actor_kind, action, outcome)
                     VALUES (?1, '2026-01-01T00:00:00.000Z', 'r', 'system', 'noise', 'ok')",
                )
                .map_err(|e| LedgerError::new("t", e.to_string()))?;
            for n in 0..(LINK_SCAN_ROWS + 10) {
                insert
                    .execute([format!("{n:026}")])
                    .map_err(|e| LedgerError::new("t", e.to_string()))?;
            }
        }
        tx.commit()
            .map_err(|e| LedgerError::new("t", e.to_string()))
    })
    .unwrap();

    let got = link(&repo, &sha(1));

    assert_eq!(got.session_id, None);
    assert_eq!(
        got.basis,
        basis::SCAN_CAPPED,
        "the rows that would answer were past the bound: unexamined, not unattributed"
    );
}

#[test]
fn a_batch_answers_every_commit_and_bounds_how_many_it_examines() {
    let (_dir, repo) = repo_dir();
    let at = t0();
    command_row(&repo, None, "S1", "git commit -m a", at - SEC);
    reflog_row(&repo, None, &sha(1), "commit", at);

    let asked: Vec<String> = (1..=LINK_MAX_COMMITS + 5).map(sha).collect();
    let mut doubled = asked.clone();
    doubled.extend(asked.iter().take(3).cloned());
    let got = link_commits(&repo, &doubled);

    assert_eq!(got.len(), LINK_MAX_COMMITS + 5, "duplicates answer once");
    assert_eq!(got[&sha(1)].session_id.as_deref(), Some("S1"));
    assert_eq!(got[&sha(2)].basis, basis::NO_REFLOG_ENTRY);
    assert_eq!(
        got[&sha(LINK_MAX_COMMITS + 5)].basis,
        basis::NOT_EXAMINED,
        "past the bound is named, not dropped"
    );
    assert!(link_commits(&repo, &[]).is_empty());
}

#[test]
fn only_git_commands_that_move_head_onto_a_commit_count() {
    for yes in [
        "git commit -m x",
        "git add -A && git commit -m x",
        "cd /wt; git -C . commit --amend",
        "git -c user.name=x merge main",
        "/usr/bin/git rebase main",
        "git cherry-pick abc",
        "git pull --rebase",
        "(git commit -m x)",
    ] {
        assert!(runs_git_mutation(yes, Path::new("/wt")), "{yes}");
    }
    for no in [
        "git status",
        "git log --oneline",
        "git diff HEAD",
        "cargo test commit",
        "echo commit",
        "gitk commit",
        "git",
        "git --version",
        "",
    ] {
        assert!(!runs_git_mutation(no, Path::new("/wt")), "{no}");
    }
}

// ---- adversarial review additions ----
//
// Each of these produces a confident wrong link, or a false miss, when the
// matcher is looser than it should be.

fn command_row_full(
    repo: &str,
    wt: Option<&str>,
    session: &str,
    text: &str,
    stamp_text: String,
    outcome: Outcome,
) {
    append(Draft {
        repo_path: repo.into(),
        worktree_path: wt.map(str::to_string),
        actor_kind: Some(ActorKind::Agent),
        actor_id: Some("claude".into()),
        session_id: Some(session.into()),
        action: "session.command".into(),
        object: Some(text.into()),
        outcome: Some(outcome),
        detail_json: Some(
            if outcome == Outcome::Failed {
                r#"{"result":"error"}"#
            } else {
                r#"{"result":"ok"}"#
            }
            .into(),
        ),
        event_stamp: Some(EventStamp {
            occurred_at: stamp_text,
            occurred_at_basis: "transcript".into(),
            task_basis: "unbound".into(),
        }),
        ..Draft::default()
    })
    .expect("append command");
}

fn noise(repo: &str, n: i64) {
    with_conn(repo, |conn| {
        let tx = conn.unchecked_transaction().map_err(|e| LedgerError::new("t", e.to_string()))?;
        {
            let mut insert = tx
                .prepare("INSERT INTO events (ulid, ts_utc, repo_path, actor_kind, action, outcome) VALUES (?1, '2026-01-01T00:00:00.000Z', 'r', 'system', 'noise', 'ok')")
                .map_err(|e| LedgerError::new("t", e.to_string()))?;
            for k in 0..n {
                insert.execute([format!("{:026}", k + 1_000_000)]).map_err(|e| LedgerError::new("t", e.to_string()))?;
            }
        }
        tx.commit().map_err(|e| LedgerError::new("t", e.to_string()))
    })
    .unwrap();
}

#[test]
fn adv_failed_command_is_not_a_candidate() {
    let (_d, repo) = repo_dir();
    let at = t0();
    command_row_full(
        &repo,
        None,
        "S1",
        "git commit -m a",
        ids::iso8601_utc(at - SEC),
        Outcome::Failed,
    );
    reflog_row(&repo, None, &sha(1), "commit", at); // human hand commit
    let got = link(&repo, &sha(1));
    assert_eq!(
        got.session_id, None,
        "failed agent commit credited with human commit: {got:?}"
    );
}

#[test]
fn adv_git_dash_c_other_repo_is_not_cwd_worktree() {
    let (_d, repo) = repo_dir();
    let at = t0();
    command_row(
        &repo,
        None,
        "S1",
        "git -C /somewhere/else commit -m a",
        at - SEC,
    );
    reflog_row(&repo, None, &sha(1), "commit", at);
    let got = link(&repo, &sha(1));
    assert_eq!(got.session_id, None, "{got:?}");
}

#[test]
fn adv_non_commit_forms_do_not_match() {
    for no in [
        "git commit --dry-run",
        "git rebase --abort",
        "git merge --abort",
        "git commit -h",
        "git merge-base a b",
        "git commit-tree HEAD^{tree}",
        "git pull --help",
        "git rebase --quit",
    ] {
        assert!(
            !runs_git_mutation(no, Path::new("/wt")),
            "falsely matches: {no}"
        );
    }
}

#[test]
fn adv_edges_999_1000_and_subsecond_and_offsets() {
    let at = t0();
    for (delta, want) in [
        (999i64, true),
        (1000, false),
        (-120_000, true),
        (-120_001, false),
    ] {
        let (_d, repo) = repo_dir();
        command_row(
            &repo,
            None,
            "S1",
            "git commit -m a",
            (at as i64 + delta) as u64,
        );
        reflog_row(&repo, None, &sha(1), "commit", at);
        let got = link(&repo, &sha(1));
        assert_eq!(got.session_id.is_some(), want, "delta {delta}: {got:?}");
    }
    // non-UTC offset spelling of the same instant
    let (_d, repo) = repo_dir();
    let cmd = at - SEC;
    let iso = ids::iso8601_utc(cmd); // ...Z
    let secs_local = ids::parse_event_time(&iso).unwrap();
    assert_eq!(secs_local, cmd);
    // same instant, +05:30 spelling: shift civil time by 5.5h
    let shifted = ids::iso8601_utc(cmd + 19_800_000).replace('Z', "+05:30");
    command_row_full(&repo, None, "S1", "git commit -m a", shifted, Outcome::Ok);
    reflog_row(&repo, None, &sha(1), "commit", at);
    assert_eq!(link(&repo, &sha(1)).session_id.as_deref(), Some("S1"));
    // missing offset
    let (_d, repo) = repo_dir();
    command_row_full(
        &repo,
        None,
        "S1",
        "git commit -m a",
        ids::iso8601_utc(cmd).replace('Z', ""),
        Outcome::Ok,
    );
    reflog_row(&repo, None, &sha(1), "commit", at);
    let got = link(&repo, &sha(1));
    assert_eq!(
        got.session_id, None,
        "offsetless time should not link: {got:?}"
    );
    println!("offsetless basis: {}", got.basis);
}

#[test]
fn adv_pull_fast_forward_credits_puller_with_someone_elses_commit() {
    let (_d, repo) = repo_dir();
    let at = t0();
    command_row(&repo, None, "S1", "git pull", at - 2 * SEC);
    reflog_row(&repo, None, &sha(7), "pull: Fast-forward", at); // onto a commit authored elsewhere
    let got = link(&repo, &sha(7));
    assert_eq!(
        got.session_id, None,
        "puller credited with authorship: {got:?}"
    );
}

#[test]
fn adv_cap_hides_earliest_reflog_entry_so_ff_is_credited() {
    let (_d, repo) = repo_dir();
    let at = t0();
    // earliest entry: worktree A, hand commit, no command
    reflog_row(&repo, Some("/wt/A"), &sha(1), "commit", at - 600 * SEC);
    noise(&repo, LINK_SCAN_ROWS + 10);
    // later: another worktree fast-forwards onto it, S2 ran git merge
    command_row(&repo, Some("/wt/B"), "S2", "git merge lane-a", at - SEC);
    reflog_row(
        &repo,
        Some("/wt/B"),
        &sha(1),
        "merge lane-a: Fast-forward",
        at,
    );
    let got = link(&repo, &sha(1));
    assert_eq!(
        got.session_id, None,
        "ff merge credited as author after cap hid origin: {got:?}"
    );
}

#[test]
fn adv_cap_hides_command_but_reports_no_session_command() {
    let (_d, repo) = repo_dir();
    let at = t0();
    command_row(&repo, None, "S1", "git commit -m a", at - SEC);
    noise(&repo, LINK_SCAN_ROWS + 10);
    reflog_row(&repo, None, &sha(1), "commit", at);
    let got = link(&repo, &sha(1));
    assert_eq!(got.basis, basis::SCAN_CAPPED, "{got:?}");
}

#[test]
fn adv_worktree_spelling() {
    let (_d, repo) = repo_dir();
    let at = t0();
    // Only a machine where /var is a symlink (macOS) has two spellings of it.
    if std::fs::canonicalize("/var").is_ok_and(|real| real == Path::new("/private/var")) {
        command_row(
            &repo,
            Some("/private/var/x/lane"),
            "S1",
            "git commit -m a",
            at - SEC,
        );
        reflog_row(&repo, Some("/var/x/lane"), &sha(1), "commit", at);
        assert_eq!(link(&repo, &sha(1)).session_id.as_deref(), Some("S1"));
    }
    let (_d, repo) = repo_dir();
    command_row(&repo, Some("/x/lane/"), "S1", "git commit -m a", at - SEC);
    reflog_row(&repo, Some("/x/lane"), &sha(1), "commit", at);
    assert_eq!(link(&repo, &sha(1)).session_id.as_deref(), Some("S1"));
}

#[test]
fn adv_prefix_worktrees_do_not_cross() {
    let (_d, repo) = repo_dir();
    let at = t0();
    command_row(&repo, Some("/x/lane"), "S1", "git commit -m a", at - SEC);
    reflog_row(&repo, Some("/x/lane-2"), &sha(1), "commit", at);
    assert_eq!(link(&repo, &sha(1)).session_id, None);
}

// ---- the rules the adversary tests above exposed, pinned from each side ----

/// A command whose result was never read: recorded `ok` in the column, with no
/// `detail_json.result` (the column has no "unknown").
fn unobserved_command_row(repo: &str, session: &str, text: &str, at: u64) {
    append(Draft {
        repo_path: repo.into(),
        actor_kind: Some(ActorKind::Agent),
        session_id: Some(session.into()),
        action: "session.command".into(),
        object: Some(text.into()),
        outcome: Some(Outcome::Ok),
        event_stamp: Some(stamp(at, "transcript")),
        ..Draft::default()
    })
    .expect("append command");
}

#[test]
fn a_command_whose_result_was_never_read_is_not_credited() {
    let (_d, repo) = repo_dir();
    let at = t0();
    unobserved_command_row(&repo, "S1", "git commit -m a", at - SEC);
    reflog_row(&repo, None, &sha(1), "commit", at);

    let got = link(&repo, &sha(1));

    assert_eq!(got.session_id, None, "{got:?}");
    assert_eq!(got.basis, basis::COMMAND_OUTCOME_UNKNOWN);
    assert!(!got.ambiguous);
}

#[test]
fn an_unread_command_of_another_session_makes_a_credited_one_ambiguous() {
    let (_d, repo) = repo_dir();
    let at = t0();
    command_row(&repo, None, "S1", "git commit -m a", at - 5 * SEC);
    unobserved_command_row(&repo, "S2", "git commit -m b", at - SEC);
    reflog_row(&repo, None, &sha(1), "commit", at);

    let got = link(&repo, &sha(1));

    assert!(got.ambiguous, "S2 may have made it: {got:?}");
    assert_eq!(got.session_id, None);

    // The same session twice, one of the two unread, is still one session.
    let (_d2, repo2) = repo_dir();
    command_row(&repo2, None, "S1", "git commit -m a", at - 5 * SEC);
    unobserved_command_row(&repo2, "S1", "git commit -m b", at - SEC);
    reflog_row(&repo2, None, &sha(2), "commit", at);
    assert_eq!(link(&repo2, &sha(2)).session_id.as_deref(), Some("S1"));
}

#[test]
fn a_failed_command_beside_a_hand_commit_credits_nobody_and_hides_no_other_session() {
    let (_d, repo) = repo_dir();
    let at = t0();
    command_row_full(
        &repo,
        None,
        "S1",
        "git commit -m a",
        ids::iso8601_utc(at - 5 * SEC),
        Outcome::Failed,
    );
    command_row(&repo, None, "S2", "git commit -m b", at - SEC);
    reflog_row(&repo, None, &sha(1), "commit", at);

    let got = link(&repo, &sha(1));

    assert_eq!(
        got.session_id.as_deref(),
        Some("S2"),
        "the failed command made nothing, so it neither links nor makes S2 ambiguous: {got:?}"
    );
}

#[test]
fn a_git_run_aimed_at_this_worktree_still_links_and_one_aimed_elsewhere_does_not() {
    let (_d, repo) = repo_dir();
    let at = t0();
    command_row(
        &repo,
        None,
        "HERE",
        &format!("git -C {repo} commit -m a"),
        at - SEC,
    );
    reflog_row(&repo, None, &sha(1), "commit", at);
    assert_eq!(link(&repo, &sha(1)).session_id.as_deref(), Some("HERE"));

    for (n, text) in [
        "cd /somewhere/else && git commit -m a",
        "git --work-tree=/somewhere/else commit -m a",
        "git --git-dir=/somewhere/else/.git commit -m a",
        "git -C ../sibling commit -m a",
        "cd ~/other && git commit -m a",
        "git -C $OTHER commit -m a",
    ]
    .into_iter()
    .enumerate()
    {
        let (_d, repo) = repo_dir();
        command_row(&repo, None, "ELSEWHERE", text, at - SEC);
        reflog_row(&repo, None, &sha(10 + n), "commit", at);
        let got = link(&repo, &sha(10 + n));
        assert_eq!(got.session_id, None, "{text}: {got:?}");
    }
}

#[test]
fn a_run_that_cannot_create_a_commit_is_not_a_candidate_and_continue_is() {
    let wt = Path::new("/wt");
    for no in [
        "git commit --dry-run -m a",
        "git commit -h",
        "git rebase --abort",
        "git rebase --quit",
        "git merge --abort",
        "git cherry-pick --abort",
        "git pull --help",
        "git -C /wt rebase --show-current-patch",
    ] {
        assert!(!runs_git_mutation(no, wt), "{no}");
    }
    for yes in [
        "git rebase --continue",
        "git rebase --skip",
        "git merge --continue",
        "git cherry-pick --continue",
        "git commit --squash abc",
        "git -C /wt commit -m a",
        "cd /wt && git commit -m a",
        "cd /elsewhere && cd /wt && git commit -m a",
        "git commit -m 'it is not a --dry-run of anything' && git commit -m b",
    ] {
        assert!(runs_git_mutation(yes, wt), "{yes}");
    }
}

#[test]
fn only_an_entry_that_made_the_commit_is_its_origin() {
    let at = t0();
    // The forms git writes when it creates a commit.
    for (n, message) in [
        "commit: c",
        "commit (initial): c",
        "commit (amend): c",
        "commit (merge): c",
        "cherry-pick: c",
        "revert: c",
        "am: c",
        "rebase (pick): c",
        "rebase (continue): c",
        "rebase (fixup): c",
        "rebase -i (squash): c",
        "pull --rebase (pick): c",
        "merge g: Merge made by the 'ort' strategy.",
        "pull: Merge made by the 'ort' strategy.",
    ]
    .into_iter()
    .enumerate()
    {
        let (_d, repo) = repo_dir();
        command_row(&repo, None, "S1", "git commit -m a", at - SEC);
        reflog_row(&repo, None, &sha(n), message, at);
        let got = link(&repo, &sha(n));
        assert_eq!(got.session_id.as_deref(), Some("S1"), "{message}: {got:?}");
    }
    // The forms that move a ref onto a commit someone else made, or onto an
    // old one.
    for (n, message) in [
        "merge g: Fast-forward",
        "pull: Fast-forward",
        "pull -q --rebase: Fast-forward",
        "reset: moving to HEAD~2",
        "checkout: moving from main to g",
        "clone: from /somewhere",
        "rebase (start): checkout main",
        "rebase (finish): returning to refs/heads/g",
        "branch: Created from HEAD",
        "rebase: fast-forward",
        "an entry in a form nobody has heard of",
        "",
    ]
    .into_iter()
    .enumerate()
    {
        let (_d, repo) = repo_dir();
        command_row(&repo, None, "S1", "git pull", at - SEC);
        reflog_row(&repo, None, &sha(n), message, at);
        let got = link(&repo, &sha(n));
        assert_eq!(got.session_id, None, "{message}: {got:?}");
        assert_eq!(got.basis, basis::NO_ORIGIN_ENTRY, "{message}");
    }
}

#[test]
fn a_commit_made_in_one_worktree_and_fast_forwarded_in_another_credits_the_maker() {
    let (_d, repo) = repo_dir();
    let at = t0();
    command_row(&repo, Some("/wt/A"), "MAKER", "git commit -m a", at - SEC);
    reflog_row(&repo, Some("/wt/A"), &sha(1), "commit", at);
    command_row(
        &repo,
        Some("/wt/B"),
        "PULLER",
        "git merge lane-a",
        at + 60 * SEC,
    );
    reflog_row(
        &repo,
        Some("/wt/B"),
        &sha(1),
        "merge lane-a: Fast-forward",
        at + 61 * SEC,
    );

    assert_eq!(link(&repo, &sha(1)).session_id.as_deref(), Some("MAKER"));
}

fn old_noise(repo: &str) {
    noise(repo, LINK_SCAN_ROWS + 10);
}

#[test]
fn a_capped_read_still_links_when_what_it_did_not_read_is_older_than_the_window() {
    let (_d, repo) = repo_dir();
    let at = t0();
    // The unread rows were appended in January: no event in this window.
    old_noise(&repo);
    command_row(&repo, None, "S1", "git commit -m a", at - SEC);
    reflog_row(&repo, None, &sha(1), "commit", at);

    let got = link(&repo, &sha(1));

    assert_eq!(got.session_id.as_deref(), Some("S1"), "{got:?}");
}

#[test]
fn a_capped_read_with_no_origin_in_it_does_not_say_there_is_none() {
    let (_d, repo) = repo_dir();
    let at = t0();
    old_noise(&repo);
    reflog_row(&repo, None, &sha(1), "merge x: Fast-forward", at);

    assert_eq!(link(&repo, &sha(1)).basis, basis::SCAN_CAPPED);
}

#[test]
fn the_anchor_checkout_is_the_repository_however_it_is_spelled() {
    let (_d, repo) = repo_dir();
    let at = t0();
    // Stored as NULL by one writer and as the repository path by another.
    command_row(&repo, Some(&repo), "S1", "git commit -m a", at - SEC);
    reflog_row(&repo, None, &sha(1), "commit", at);
    assert_eq!(link(&repo, &sha(1)).session_id.as_deref(), Some("S1"));

    let (_d2, repo2) = repo_dir();
    command_row(&repo2, None, "S1", "git commit -m a", at - SEC);
    reflog_row(&repo2, Some(&format!("{repo2}/")), &sha(2), "commit", at);
    assert_eq!(link(&repo2, &sha(2)).session_id.as_deref(), Some("S1"));
}

#[test]
fn a_symlinked_spelling_of_a_worktree_is_the_same_worktree() {
    let (_d, repo) = repo_dir();
    let (_real, real_path) = repo_dir();
    let link_dir = tempfile::tempdir().unwrap();
    let alias = link_dir.path().join("alias");
    std::os::unix::fs::symlink(&real_path, &alias).unwrap();
    let at = t0();
    command_row(
        &repo,
        Some(alias.to_str().unwrap()),
        "S1",
        "git commit -m a",
        at - SEC,
    );
    reflog_row(&repo, Some(&real_path), &sha(1), "commit", at);

    assert_eq!(link(&repo, &sha(1)).session_id.as_deref(), Some("S1"));
}

#[test]
fn every_basis_the_matcher_can_give_is_listed() {
    // The schema test walks `basis::ALL`; a value missing from it would be a
    // value that test never checked.
    let (_d, repo) = repo_dir();
    let asked: Vec<String> = (0..LINK_MAX_COMMITS + 1).map(sha).collect();
    let got = link_commits(&repo, &asked);
    for link in got.values() {
        assert!(basis::ALL.contains(&link.basis), "{}", link.basis);
    }
}
