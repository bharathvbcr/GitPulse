//! Commit-to-session attribution: a transcript's `git commit` command and the
//! reflog entry that followed it. Rows go in through the ledger's own writer
//! (`append`), stamped the way ingest stamps them, so what is matched is what a
//! real catch-up writes.

use super::attribution::{
    basis, command_target, link_commits, read_command, runs_git_mutation, CommitLink, Target,
    HIDDEN_PROBE_ROWS, LINK_MAX_COMMITS, LINK_SCAN_ROWS, LINK_WINDOW_MS,
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
fn a_hand_commit_with_no_ingested_session_command_stays_unattributed() {
    let (_dir, repo) = repo_dir();
    let at = t0();
    // Other commands in the window are not git commits.
    command_row(&repo, None, "S1", "cargo test", at - 3 * SEC);
    command_row(&repo, None, "S1", "git status", at - 2 * SEC);
    reflog_row(&repo, None, &sha(1), "commit", at);

    let got = link(&repo, &sha(1));

    assert_eq!(got.session_id, None, "{got:?}");
    assert_eq!(got.basis, basis::NO_INGESTED_SESSION_COMMAND);
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
    assert_eq!(got.basis, basis::NO_INGESTED_SESSION_COMMAND);
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
    assert_eq!(
        link(&repo, &sha(1)).basis,
        basis::NO_INGESTED_SESSION_COMMAND
    );
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
    command_row_timed(
        &repo2,
        None,
        "S9",
        &format!("cd {repo2} && git -C {repo2} rebase main"),
        at,
        10_000,
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
    assert_eq!(got.basis, basis::NO_INGESTED_SESSION_COMMAND);
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
    let tree = tree();
    let wt = tree.w.as_path();
    for yes in [
        "git commit -m x",
        "git add -A && git commit -m x",
        "cd . ; git -C . commit --amend",
        "git -c user.name=x merge main",
        "/usr/bin/git rebase main",
        "git cherry-pick abc",
        "git pull --rebase",
    ] {
        assert!(runs_git_mutation(yes, wt), "{yes}");
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
        assert!(!runs_git_mutation(no, wt), "{no}");
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
    let wt = Path::new("/wt");
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
        assert!(!runs_git_mutation(no, wt), "falsely matches: {no}");
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
fn adv_cap_hides_command_but_reports_no_ingested_session_command() {
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
    // An existing checkout under a symlinked parent (macOS: /var is
    // /private/var) is one worktree however it is spelled. A vanished one is
    // not resolved through its ancestors; see `a_vanished_worktree_matches_...`.
    let (_lane, lane_path) = repo_dir();
    let canonical = std::fs::canonicalize(&lane_path).unwrap();
    command_row(&repo, Some(&lane_path), "S1", "git commit -m a", at - SEC);
    reflog_row(
        &repo,
        Some(canonical.to_str().unwrap()),
        &sha(1),
        "commit",
        at,
    );
    assert_eq!(link(&repo, &sha(1)).session_id.as_deref(), Some("S1"));
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
    let tree = tree();
    let wt = tree.w.as_path();
    let (w, l) = (tree.w.display(), tree.lane.display());
    for no in [
        "git commit --dry-run -m a",
        "git commit -h",
        "git rebase --abort",
        "git rebase --quit",
        "git merge --abort",
        "git cherry-pick --abort",
        "git pull --help",
        "git -C . rebase --show-current-patch",
    ] {
        assert!(!runs_git_mutation(no, wt), "{no}");
    }
    for yes in [
        "git rebase --continue",
        "git rebase --skip",
        "git merge --continue",
        "git cherry-pick --continue",
        "git commit --squash abc",
        "git -C . commit -m a",
        "git commit -m 'it is not a --dry-run of anything' && git commit -m b",
    ] {
        assert!(runs_git_mutation(yes, wt), "{yes}");
    }
    for yes in [
        format!("git -C {w} commit -m a"),
        format!("cd {w} && git commit -m a"),
        format!("cd {l} && cd {w} && git commit -m a"),
    ] {
        assert!(runs_git_mutation(&yes, wt), "{yes}");
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
        command_row_for(&repo, "S1", message, at);
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

// ---- round-2 adversarial additions (reviewer, uncommitted) ----

static R2_SEQ: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

#[allow(clippy::too_many_arguments)]
fn r2_raw(
    repo: &str,
    ts: &str,
    action: &str,
    session: Option<&str>,
    object: Option<&str>,
    outcome: &str,
    detail: Option<&str>,
    wt: Option<&str>,
    after_ref: Option<&str>,
) {
    let n = R2_SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    with_conn(repo, |conn| {
        conn.execute(
            "INSERT INTO events (ulid, ts_utc, repo_path, worktree_path, actor_kind, session_id, action, object, outcome, after_ref, detail_json)
             VALUES (?1, ?2, 'r', ?3, 'system', ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![format!("{:026}", 9_000_000 + n), ts, wt, session, action, object, outcome, after_ref, detail],
        )
        .map_err(|e| LedgerError::new("t", e.to_string()))?;
        Ok(())
    })
    .unwrap();
}

fn r2_cmd_detail(at: u64) -> String {
    format!(
        r#"{{"result":"ok","occurred_at":"{}","occurred_at_basis":"transcript"}}"#,
        ids::iso8601_utc(at)
    )
}

#[test]
fn r2_cherry_pick_fast_forward_is_not_an_origin() {
    let (_d, repo) = repo_dir();
    let at = t0();
    // Session B cherry-picks (fast-forwards onto) a commit session A made.
    command_row(&repo, None, "B", "git cherry-pick abc123", at - SEC);
    reflog_row(&repo, None, &sha(1), "cherry-pick: fast-forward", at);
    let got = link(&repo, &sha(1));
    assert_eq!(got.session_id, None, "BROKEN: mover credited: {got:?}");
}

#[test]
fn r2_reflog_message_table() {
    // (message, is_origin) -- real forms from git 2.54 first, then variants.
    let table: &[(&str, bool)] = &[
        ("commit: m", true),
        ("commit (initial): m", true),
        ("commit (amend): m", true),
        ("commit (merge): m", true),
        ("commit (cherry-pick): m", true),
        ("cherry-pick: m", true),
        ("cherry-pick: fast-forward", false),
        ("revert: Revert \"x\"", true),
        ("am: m", true),
        ("rebase (pick): m", true),
        ("rebase (continue): m", true),
        ("rebase (start): checkout main", false),
        ("rebase (finish): returning to refs/heads/x", false),
        (
            "rebase (continue) (finish): returning to refs/heads/x",
            false,
        ),
        ("pull --rebase (pick): m", true),
        ("pull --rebase (start): checkout abc", false),
        (
            "pull --rebase (finish): returning to refs/heads/main",
            false,
        ),
        ("pull --rebase: Fast-forward", false),
        ("pull --ff-only: Fast-forward", false),
        ("pull: Fast-forward", false),
        ("pull: Merge made by the 'ort' strategy.", true),
        ("merge feat: Merge made by the 'ort' strategy.", true),
        ("merge feat: Fast-forward", false),
        ("merge feat: Already up to date", false),
        ("reset: moving to HEAD~1", false),
        ("checkout: moving from a to b", false),
        ("branch: Created from HEAD", false),
        ("clone: from /x", false),
        ("update by push", false),
        ("fetch origin: fast-forward", false),
        ("stash: pop", false),
        ("commit: Fast-forward", true),
        ("merge: Merge made by the 'ort' strategy.", true),
        ("reset: moving to commit: x", false),
        ("checkout: moving from commit: x to y", false),
        ("revert (continue): m", true),
        ("cherry-pick (continue): m", true),
        ("rebase: fast-forward", false),
        ("rebase (fast-forward): m", false),
        ("pull (fast-forward): m", false),
    ];
    let mut wrong = vec![];
    for (i, (msg, origin)) in table.iter().enumerate() {
        let (_d, repo) = repo_dir();
        let at = t0();
        command_row_for(&repo, "S1", msg, at);
        reflog_row(&repo, None, &sha(i + 1), msg, at);
        let got = link(&repo, &sha(i + 1));
        let credited = got.session_id.is_some();
        if credited != *origin {
            wrong.push(format!("{msg:?}: expected origin={origin}, got {got:?}"));
        }
    }
    assert!(wrong.is_empty(), "misclassified:\n{}", wrong.join("\n"));
}

struct Tree {
    _base: tempfile::TempDir,
    w: PathBuf,
    lane: PathBuf,
    deep: PathBuf,
}
use std::path::PathBuf;
fn tree() -> Tree {
    let base = tempfile::tempdir().unwrap();
    let p = base.path().canonicalize().unwrap();
    let w = p.join("w");
    let lane = p.join("lane");
    let deep = p.join("deep");
    for d in [&w, &lane, &deep, &w.join("sub"), &w.join("lane one")] {
        std::fs::create_dir_all(d).unwrap();
    }
    for d in [&w, &lane, &deep, &w.join("lane one")] {
        std::fs::create_dir_all(d.join(".git")).unwrap();
    }
    std::os::unix::fs::symlink(&deep, w.join("link")).unwrap();
    std::os::unix::fs::symlink(w.join("loop"), w.join("loop")).unwrap();
    Tree {
        _base: base,
        w,
        lane,
        deep,
    }
}

fn r2_aimed_elsewhere(cmds: Vec<String>, t: &Tree) -> Vec<String> {
    cmds.into_iter()
        .filter(|c| runs_git_mutation(c, &t.w))
        .collect()
}

#[test]
fn r2_commands_aimed_at_another_worktree_are_not_credited() {
    let t = tree();
    let (w, l, d) = (t.w.display(), t.lane.display(), t.deep.display());
    let cmds: Vec<String> = vec![
        // cd chains
        "cd .. && cd lane && git commit -m x".into(),
        format!("pushd {l} && git commit -m x"),
        format!("git -C . -C {l} commit -m x"),
        format!("git -C {l} -C . commit -m x"),
        // quoted path with spaces (nested checkout)
        format!("git -C \"{w}/lane one\" commit -m x"),
        format!("cd \"{w}/lane one\" && git commit -m x"),
        // env prefixes
        format!("GIT_DIR={l}/.git GIT_WORK_TREE={l} git commit -m x"),
        format!("GIT_DIR={l}/.git git commit -m x"),
        format!("GIT_WORK_TREE={l} git commit -m x"),
        format!("env GIT_DIR={l}/.git git commit -m x"),
        // config worktree
        format!("git -c core.worktree={l} commit -m x"),
        "git --config-env=core.worktree=X commit -m x".to_string(),
        // nested shells
        format!("sh -c 'cd {l} && git commit -m x'"),
        format!("bash -lc \"cd {l} && git commit -m x\""),
        // `..` after a symlink
        format!("git -C {w}/link/.. commit -m x"),
        format!("cd {w}/link/.. && git commit -m x"),
        // wrappers
        format!("env -C {l} git commit -m x"),
        format!("xargs -I{{}} git -C {l} commit"),
        format!("git --git-dir {l}/.git commit -m x"),
        format!("git --git-dir={d}/.git commit -m x"),
    ];
    let wrong = r2_aimed_elsewhere(cmds, &t);
    assert!(
        wrong.is_empty(),
        "credited to w but aimed elsewhere:\n{}",
        wrong.join("\n")
    );
}

#[test]
fn r2_commands_aimed_here_still_count_controls() {
    let t = tree();
    let w = t.w.display();
    for c in [
        "git commit -m x".to_string(),
        "git -C . -C sub commit -m x".to_string(),
        format!("git -C {w}/sub commit -m x"),
        "cd sub && git commit -m x".to_string(),
        format!("git --git-dir={w}/.git --work-tree={w} commit -m x"),
    ] {
        assert!(runs_git_mutation(&c, &t.w), "control missed: {c}");
    }
}

#[test]
fn r2_symlink_loop_does_not_hang() {
    let t = tree();
    let c = format!("git -C {}/loop/x commit -m x", t.w.display());
    let _ = runs_git_mutation(&c, &t.w);
}

#[test]
fn r2_removed_lane_replaced_by_symlink_must_not_match_another_worktree() {
    let (_d, repo) = repo_dir();
    let base = tempfile::tempdir().unwrap();
    let p = base.path().canonicalize().unwrap();
    let other = p.join("other");
    std::fs::create_dir_all(&other).unwrap();
    // `gone` was a lane directory, now removed and replaced by a link to `other`.
    std::os::unix::fs::symlink(&other, p.join("gone")).unwrap();
    let at = t0();
    // The command ran in `other/lane` (a different worktree); the commit is
    // reflogged for the old lane `gone/lane`. Neither exists on disk now.
    command_row(
        &repo,
        Some(other.join("lane").to_str().unwrap()),
        "S1",
        "git commit -m a",
        at - SEC,
    );
    reflog_row(
        &repo,
        Some(p.join("gone/lane").to_str().unwrap()),
        &sha(1),
        "commit",
        at,
    );
    let got = link(&repo, &sha(1));
    assert_eq!(
        got.session_id, None,
        "resolved into another worktree: {got:?}"
    );
}

#[test]
fn r2_case_insensitive_spelling_probe() {
    let base = tempfile::tempdir().unwrap();
    let p = base.path().canonicalize().unwrap();
    std::fs::create_dir_all(p.join("Repo")).unwrap();
    let ci = std::fs::canonicalize(p.join("repo")).is_ok();
    eprintln!(
        "fs case-insensitive: {ci}; canonical(repo)={:?}",
        std::fs::canonicalize(p.join("repo"))
    );
    let (_d, repo) = repo_dir();
    let at = t0();
    command_row(
        &repo,
        Some(p.join("repo").to_str().unwrap()),
        "S1",
        "git commit -m a",
        at - SEC,
    );
    reflog_row(
        &repo,
        Some(p.join("Repo").to_str().unwrap()),
        &sha(1),
        "commit",
        at,
    );
    let got = link(&repo, &sha(1));
    eprintln!("case-variant spellings link: {got:?}");
    // Both are the same directory on a case-insensitive volume.
    if ci {
        assert_eq!(got.session_id.as_deref(), Some("S1"), "false miss: {got:?}");
    }
    // And a vanished one, spelled two ways, on the same volume.
    let (_d2, repo2) = repo_dir();
    command_row(
        &repo2,
        Some(p.join("Gone/x").to_str().unwrap()),
        "S1",
        "git commit -m a",
        at - SEC,
    );
    reflog_row(
        &repo2,
        Some(p.join("gone/x").to_str().unwrap()),
        &sha(2),
        "commit",
        at,
    );
    eprintln!("vanished case-variants: {:?}", link(&repo2, &sha(2)));
}

#[test]
fn r2_outcome_only_ok_links() {
    let at = t0();
    let details: Vec<(&str, String)> = vec![
        ("error", r#"{"result":"error"}"#.into()),
        ("cancelled", r#"{"result":"cancelled"}"#.into()),
        ("interrupted", r#"{"result":"interrupted"}"#.into()),
        ("null", r#"{"result":null}"#.into()),
        ("OK upper", r#"{"result":"OK"}"#.into()),
        ("padded", r#"{"result":" ok"}"#.into()),
        ("badjson", r#"{"result":"ok""#.into()),
        ("empty", "".into()),
        ("missing", "{}".into()),
    ];
    let mut wrong = vec![];
    for (name, detail) in details {
        let (_d, repo) = repo_dir();
        let det = if detail.starts_with('{') && detail.ends_with('}') {
            detail.trim_end_matches('}').to_string()
                + &format!(
                    r#","occurred_at":"{}","occurred_at_basis":"transcript"}}"#,
                    ids::iso8601_utc(at - SEC)
                )
                .replacen("{,", "{", 1)
        } else {
            detail.clone()
        };
        let det = det.replace("{,", "{");
        r2_raw(
            &repo,
            &ids::iso8601_utc(at),
            "session.command",
            Some("S1"),
            Some("git commit -m a"),
            "ok",
            Some(&det),
            None,
            None,
        );
        reflog_row(&repo, None, &sha(1), "commit", at);
        let got = link(&repo, &sha(1));
        if got.basis == basis::INFERRED {
            wrong.push(format!("{name} ({det}) -> {got:?}"));
        }
    }
    assert!(wrong.is_empty(), "linked without ok:\n{}", wrong.join("\n"));
}

#[test]
fn r2_result_ok_but_column_failed_does_not_link() {
    let (_d, repo) = repo_dir();
    let at = t0();
    r2_raw(
        &repo,
        &ids::iso8601_utc(at),
        "session.command",
        Some("S1"),
        Some("git commit -m a"),
        "failed",
        Some(&r2_cmd_detail(at - SEC)),
        None,
        None,
    );
    reflog_row(&repo, None, &sha(1), "commit", at);
    assert_ne!(link(&repo, &sha(1)).basis, basis::INFERRED);
}

#[test]
fn r2_one_malformed_typed_row_does_not_blind_every_commit() {
    let (_d, repo) = repo_dir();
    let at = t0();
    // A non-string result (transcripts are arbitrary JSON).
    let bad = format!(
        r#"{{"result":1,"occurred_at":"{}","occurred_at_basis":"transcript"}}"#,
        ids::iso8601_utc(at)
    );
    r2_raw(
        &repo,
        &ids::iso8601_utc(at),
        "session.command",
        Some("X"),
        Some("git status"),
        "ok",
        Some(&bad),
        None,
        None,
    );
    command_row(&repo, None, "S1", "git commit -m a", at - SEC);
    reflog_row(&repo, None, &sha(1), "commit", at);
    let got = link(&repo, &sha(1));
    assert_eq!(
        got.session_id.as_deref(),
        Some("S1"),
        "poisoned read: {got:?}"
    );
}

#[test]
fn r2_numeric_occurred_at_does_not_blind_every_commit() {
    let (_d, repo) = repo_dir();
    let at = t0();
    r2_raw(
        &repo,
        &ids::iso8601_utc(at),
        "session.command",
        Some("X"),
        Some("git status"),
        "ok",
        Some(r#"{"result":"ok","occurred_at":1762500000,"occurred_at_basis":"transcript"}"#),
        None,
        None,
    );
    command_row(&repo, None, "S1", "git commit -m a", at - SEC);
    reflog_row(&repo, None, &sha(1), "commit", at);
    let got = link(&repo, &sha(1));
    assert_eq!(
        got.session_id.as_deref(),
        Some("S1"),
        "poisoned read: {got:?}"
    );
}

#[test]
fn r2_ambiguity_shapes() {
    let at = t0();
    // two unread from the SAME session only
    let (_d, repo) = repo_dir();
    unobserved_command_row(&repo, "B", "git commit -m a", at - 3 * SEC);
    unobserved_command_row(&repo, "B", "git commit -m b", at - 2 * SEC);
    reflog_row(&repo, None, &sha(1), "commit", at);
    assert_eq!(link(&repo, &sha(1)).basis, basis::COMMAND_OUTCOME_UNKNOWN);
    // unread B beside ok A
    let (_d, repo) = repo_dir();
    command_row(&repo, None, "A", "git commit -m a", at - 3 * SEC);
    unobserved_command_row(&repo, "B", "git commit -m b", at - 2 * SEC);
    reflog_row(&repo, None, &sha(1), "commit", at);
    assert!(link(&repo, &sha(1)).ambiguous);
    // unread A beside ok A: same session
    let (_d, repo) = repo_dir();
    command_row(&repo, None, "A", "git commit -m a", at - 3 * SEC);
    unobserved_command_row(&repo, "A", "git commit -m b", at - 2 * SEC);
    reflog_row(&repo, None, &sha(1), "commit", at);
    assert_eq!(link(&repo, &sha(1)).session_id.as_deref(), Some("A"));
}

// ---- cap probe ----

fn r2_capped_world(hidden: impl FnOnce(&str, u64)) -> (tempfile::TempDir, String, u64) {
    let (d, repo) = repo_dir();
    let at = t0();
    hidden(&repo, at);
    noise(&repo, LINK_SCAN_ROWS + 10);
    command_row(&repo, None, "A", "git commit -m a", at - SEC);
    reflog_row(&repo, None, &sha(1), "commit", at);
    (d, repo, at)
}

#[test]
fn r2_cap_hidden_future_ts_is_scan_capped() {
    let (_d, repo, _) = r2_capped_world(|repo, _| {
        r2_raw(
            repo,
            "2099-01-01T00:00:00.000Z",
            "noise",
            None,
            None,
            "ok",
            None,
            None,
            None,
        );
    });
    assert_eq!(link(&repo, &sha(1)).basis, basis::SCAN_CAPPED);
}

#[test]
fn r2_cap_boundary_inclusive() {
    let at0 = t0();
    let since = at0.saturating_sub(LINK_WINDOW_MS).saturating_sub(600_000);
    let (_d, repo, _) = r2_capped_world(|repo, at| {
        let since = at - LINK_WINDOW_MS - 600_000;
        r2_raw(
            repo,
            &ids::iso8601_utc(since),
            "noise",
            None,
            None,
            "ok",
            None,
            None,
            None,
        );
    });
    assert_eq!(
        link(&repo, &sha(1)).basis,
        basis::SCAN_CAPPED,
        "since={since}"
    );
    let (_d, repo, _) = r2_capped_world(|repo, at| {
        let since = at - LINK_WINDOW_MS - 600_000 - 1;
        r2_raw(
            repo,
            &ids::iso8601_utc(since),
            "noise",
            None,
            None,
            "ok",
            None,
            None,
            None,
        );
    });
    assert_eq!(link(&repo, &sha(1)).session_id.as_deref(), Some("A"));
}

#[test]
fn r2_cap_hidden_row_with_empty_ts_but_in_window_event() {
    // The hidden row was appended with a bad append stamp but its event time
    // is inside the window: session B ran a commit, then A's is credited alone.
    for ts in ["", "not-a-time", "2026-01-01T00:00:00.000Z"] {
        let (_d, repo, _) = r2_capped_world(|repo, at| {
            r2_raw(
                repo,
                ts,
                "session.command",
                Some("B"),
                Some("git commit -m b"),
                "ok",
                Some(&r2_cmd_detail(at - 2 * SEC)),
                None,
                None,
            );
        });
        let got = link(&repo, &sha(1));
        eprintln!("hidden-B ts={ts:?}: {:?} {:?}", got.basis, got.session_id);
    }
}

#[test]
fn r2_cap_non_monotonic_ids_newest_by_ts_hidden() {
    // The newest-by-ts rows have the LOWEST ids (a replayed batch).
    let (_d, repo, _) = r2_capped_world(|repo, at| {
        r2_raw(
            repo,
            &ids::iso8601_utc(at),
            "session.command",
            Some("B"),
            Some("git commit -m b"),
            "ok",
            Some(&r2_cmd_detail(at - 2 * SEC)),
            None,
            None,
        );
    });
    assert_eq!(link(&repo, &sha(1)).basis, basis::SCAN_CAPPED);
}

#[test]
fn r2_cap_hidden_earlier_origin_of_same_commit() {
    // The commit's first origin entry (session B's) is hidden and old; a later
    // origin entry for the same sha (cherry-pick of identical tree) is visible.
    let (_d, repo) = repo_dir();
    let at = t0();
    r2_raw(
        &repo,
        "2026-01-01T00:00:00.000Z",
        "reflog.commit",
        None,
        Some(&format!("{} commit: m", sha(1))),
        "ok",
        None,
        None,
        Some(&sha(1)),
    );
    noise(&repo, LINK_SCAN_ROWS + 10);
    command_row(&repo, None, "A", "git cherry-pick x", at - SEC);
    reflog_row(&repo, None, &sha(1), "cherry-pick: m", at);
    let got = link(&repo, &sha(1));
    eprintln!("hidden-earlier-origin: {got:?}");
}

// ---- fail closed: what a command line does not prove, it does not credit ----

#[test]
fn a_command_whose_target_is_not_known_never_links_and_is_named() {
    let at = t0();
    for (n, text) in [
        "GIT_DIR=/x/.git git commit -m a",
        "env GIT_WORK_TREE=/x git commit -m a",
        "sh -c 'git commit -m a'",
        "bash -lc \"git commit -m a\"",
        "eval git commit -m a",
        "git -c core.worktree=/x commit -m a",
        "git --config-env=core.worktree=X commit -m a",
        "time git commit -m a",
        "git -C \"/some where\" commit -m a",
        "git -C ../nowhere commit -m a",
        "cd /does/not/exist && git commit -m a",
        "(cd /x && git commit -m a)",
    ]
    .into_iter()
    .enumerate()
    {
        let (_d, repo) = repo_dir();
        command_row(&repo, None, "S1", text, at - SEC);
        reflog_row(&repo, None, &sha(n), "commit", at);
        let got = link(&repo, &sha(n));
        assert_eq!(got.session_id, None, "{text}: {got:?}");
        assert_eq!(got.basis, basis::COMMAND_TARGET_UNKNOWN, "{text}");
    }
}

#[test]
fn a_command_of_unknown_target_from_another_session_makes_a_proved_one_ambiguous() {
    let (_d, repo) = repo_dir();
    let at = t0();
    command_row(&repo, None, "A", "git commit -m a", at - 5 * SEC);
    command_row(&repo, None, "B", "env GIT_DIR=/x git commit -m b", at - SEC);
    reflog_row(&repo, None, &sha(1), "commit", at);

    let got = link(&repo, &sha(1));

    assert!(got.ambiguous, "B may have made it: {got:?}");
    assert_eq!(got.session_id, None);
}

#[test]
fn a_command_proved_to_aim_elsewhere_is_not_a_candidate_and_does_not_make_anyone_ambiguous() {
    let t = tree();
    let (_d, repo) = repo_dir();
    let at = t0();
    command_row(&repo, None, "A", "git commit -m a", at - 5 * SEC);
    command_row(
        &repo,
        None,
        "B",
        &format!("git -C {} commit -m b", t.lane.display()),
        at - SEC,
    );
    reflog_row(&repo, None, &sha(1), "commit", at);

    assert_eq!(link(&repo, &sha(1)).session_id.as_deref(), Some("A"));
}

#[test]
fn a_hidden_row_that_cannot_vouch_for_its_own_time_defeats_the_cap_proof() {
    for (ts, event) in [
        ("", true),
        ("not-a-time", true),
        ("2026-01-01T00:00:00.000Z", true),
    ] {
        let (_d, repo, _) = r2_capped_world(|repo, at| {
            let detail = r2_cmd_detail(at - 2 * SEC);
            r2_raw(
                repo,
                ts,
                "session.command",
                Some("B"),
                Some("git commit -m b"),
                "ok",
                event.then_some(detail.as_str()),
                None,
                None,
            );
        });
        let got = link(&repo, &sha(1));
        assert_eq!(got.session_id, None, "ts={ts:?}: {got:?}");
        assert_eq!(got.basis, basis::SCAN_CAPPED, "ts={ts:?}");
    }
}

#[test]
fn a_hidden_origin_entry_of_the_same_commit_defeats_the_cap_proof() {
    let (_d, repo) = repo_dir();
    let at = t0();
    r2_raw(
        &repo,
        "2026-01-01T00:00:00.000Z",
        "reflog.commit",
        None,
        Some(&format!("{} commit: m", sha(1))),
        "ok",
        None,
        None,
        Some(&sha(1)),
    );
    noise(&repo, LINK_SCAN_ROWS + 10);
    command_row(&repo, None, "A", "git cherry-pick x", at - SEC);
    reflog_row(&repo, None, &sha(1), "cherry-pick: m", at);

    assert_eq!(link(&repo, &sha(1)).basis, basis::SCAN_CAPPED);
}

#[test]
fn a_ledger_larger_than_the_probe_bound_is_not_provable() {
    let (_d, repo) = repo_dir();
    let at = t0();
    noise(&repo, LINK_SCAN_ROWS + HIDDEN_PROBE_ROWS + 10);
    command_row(&repo, None, "A", "git commit -m a", at - SEC);
    reflog_row(&repo, None, &sha(1), "commit", at);

    assert_eq!(link(&repo, &sha(1)).basis, basis::SCAN_CAPPED);
}

#[test]
fn a_row_of_the_wrong_type_is_that_rows_own_unknown_and_the_batch_still_answers() {
    let (_d, repo) = repo_dir();
    let at = t0();
    for detail in [
        r#"{"result":1,"occurred_at":1762500000,"occurred_at_basis":"transcript"}"#,
        r#"{"result":{"x":1},"occurred_at":["a"],"occurred_at_basis":7}"#,
        r#"{"result":true,"occurred_at":null}"#,
    ] {
        r2_raw(
            &repo,
            &ids::iso8601_utc(at),
            "session.command",
            Some("X"),
            Some("git status"),
            "ok",
            Some(detail),
            None,
            None,
        );
    }
    // A column of the wrong type altogether.
    with_conn(&repo, |conn| {
        conn.execute(
            "INSERT INTO events (ulid, ts_utc, repo_path, actor_kind, session_id, action, object, outcome)
             VALUES ('9999999999999999999999ZZZZ', ?1, 'r', 'system', 12345, 'session.command', X'67697420636f6d6d6974', 'ok')",
            [ids::iso8601_utc(at)],
        )
        .map_err(|e| LedgerError::new("t", e.to_string()))?;
        Ok(())
    })
    .unwrap();
    command_row(&repo, None, "S1", "git commit -m a", at - SEC);
    reflog_row(&repo, None, &sha(1), "commit", at);

    assert_eq!(link(&repo, &sha(1)).session_id.as_deref(), Some("S1"));
}

#[test]
fn a_vanished_worktree_matches_only_its_own_spelling() {
    let (_d, repo) = repo_dir();
    let at = t0();
    command_row(
        &repo,
        Some("/gone/lane/"),
        "S1",
        "git commit -m a",
        at - SEC,
    );
    reflog_row(&repo, Some("/gone/lane"), &sha(1), "commit", at);
    assert_eq!(link(&repo, &sha(1)).session_id.as_deref(), Some("S1"));

    let (_d2, repo2) = repo_dir();
    command_row(
        &repo2,
        Some("/gone/lane-b"),
        "S1",
        "git commit -m a",
        at - SEC,
    );
    reflog_row(&repo2, Some("/gone/lane"), &sha(2), "commit", at);
    assert_eq!(link(&repo2, &sha(2)).session_id, None);
}

// ---- round 3: a command is credited with the next entry, and an unknown verb
// is not a known read ----

#[test]
fn an_agent_commit_is_credited_with_its_own_commit_and_not_with_a_hand_commit_after_it() {
    let (_d, repo) = repo_dir();
    let at = t0();
    command_row(&repo, None, "AGENT", "git commit -m a", at - SEC);
    reflog_row(&repo, None, &sha(1), "commit", at);
    // A human commits in the same worktree 30 s later, inside the window.
    reflog_row(&repo, None, &sha(2), "commit", at + 30 * SEC);

    let agent = link(&repo, &sha(1));
    let human = link(&repo, &sha(2));

    assert_eq!(agent.session_id.as_deref(), Some("AGENT"), "{agent:?}");
    assert_eq!(
        human.session_id, None,
        "the command made X, not Y: {human:?}"
    );
    assert_eq!(human.basis, basis::COMMAND_SPENT);
}

#[test]
fn a_rebase_is_credited_with_its_picks_and_nothing_after_them() {
    let at = t0();
    for with_finish in [true, false] {
        let (_d, repo) = repo_dir();
        command_row_timed(&repo, None, "AGENT", "git rebase main", at, 10_000);
        for n in 1..=3 {
            reflog_row(
                &repo,
                None,
                &sha(n),
                "rebase (pick): work",
                at + n as u64 * SEC,
            );
        }
        if with_finish {
            reflog_row(
                &repo,
                None,
                &sha(3),
                "rebase (finish): returning to refs/heads/x",
                at + 4 * SEC,
            );
        }
        reflog_row(&repo, None, &sha(9), "commit", at + 30 * SEC);

        for n in 1..=3 {
            let got = link(&repo, &sha(n));
            assert_eq!(
                got.session_id.as_deref(),
                Some("AGENT"),
                "pick {n}: {got:?}"
            );
        }
        let hand = link(&repo, &sha(9));
        assert_eq!(hand.session_id, None, "finish={with_finish}: {hand:?}");
        assert_eq!(hand.basis, basis::COMMAND_SPENT);
    }
}

#[test]
fn a_rebase_run_ends_at_its_finish_and_at_an_unrelated_commit() {
    let at = t0();
    // A human's own rebase after the agent's finished.
    let (_d, repo) = repo_dir();
    command_row_timed(&repo, None, "AGENT", "git rebase main", at, 10_000);
    reflog_row(&repo, None, &sha(1), "rebase (pick): work", at + SEC);
    reflog_row(
        &repo,
        None,
        &sha(1),
        "rebase (finish): returning to refs/heads/x",
        at + 2 * SEC,
    );
    reflog_row(&repo, None, &sha(2), "rebase (pick): work", at + 20 * SEC);
    assert_eq!(link(&repo, &sha(1)).session_id.as_deref(), Some("AGENT"));
    assert_eq!(link(&repo, &sha(2)).session_id, None);

    // A hand commit in the middle of the run ends it.
    let (_d2, repo2) = repo_dir();
    command_row_timed(&repo2, None, "AGENT", "git rebase main", at, 10_000);
    reflog_row(&repo2, None, &sha(1), "rebase (pick): work", at + SEC);
    reflog_row(&repo2, None, &sha(2), "commit", at + 2 * SEC);
    reflog_row(&repo2, None, &sha(3), "rebase (pick): work", at + 3 * SEC);
    assert_eq!(link(&repo2, &sha(1)).session_id.as_deref(), Some("AGENT"));
    assert_eq!(link(&repo2, &sha(3)).session_id, None);
}

#[test]
fn two_commits_by_one_session_ten_seconds_apart_each_link_to_their_own_command() {
    let (_d, repo) = repo_dir();
    let at = t0();
    command_row(&repo, None, "S1", "git commit -m first", at - SEC);
    reflog_row(&repo, None, &sha(1), "commit", at);
    command_row(&repo, None, "S1", "git commit -m second", at + 9 * SEC);
    reflog_row(&repo, None, &sha(2), "commit", at + 10 * SEC);

    let first = link(&repo, &sha(1));
    let second = link(&repo, &sha(2));

    assert_eq!(
        first.command.as_deref(),
        Some("git commit -m first"),
        "{first:?}"
    );
    assert_eq!(
        second.command.as_deref(),
        Some("git commit -m second"),
        "{second:?}"
    );
    assert_eq!(first.session_id.as_deref(), Some("S1"));
    assert_eq!(second.session_id.as_deref(), Some("S1"));
}

#[test]
fn two_different_commits_in_one_second_have_no_order_and_neither_is_credited() {
    let (_d, repo) = repo_dir();
    let at = t0();
    command_row(&repo, None, "S1", "git commit -m a", at - SEC);
    reflog_row(&repo, None, &sha(1), "commit", at);
    reflog_row(&repo, None, &sha(2), "commit", at);

    for n in [1, 2] {
        let got = link(&repo, &sha(n));
        assert_eq!(got.session_id, None, "{got:?}");
        assert_eq!(got.basis, basis::COMMAND_SPENT);
    }
}

#[test]
fn an_entry_with_no_event_time_between_them_leaves_the_order_unknown() {
    let (_d, repo) = repo_dir();
    let at = t0();
    command_row(&repo, None, "S1", "git commit -m a", at - 5 * SEC);
    // Another commit's origin entry that git could not date: it may lie
    // between the command and this commit.
    append(Draft {
        repo_path: repo.clone(),
        actor_kind: Some(ActorKind::System),
        action: "reflog.commit".into(),
        object: Some(format!("{} commit: other", sha(8))),
        outcome: Some(Outcome::Ok),
        after_ref: Some(sha(8)),
        event_stamp: Some(stamp(at - 2 * SEC, "ingest")),
        ..Draft::default()
    })
    .unwrap();
    reflog_row(&repo, None, &sha(1), "commit", at);

    let got = link(&repo, &sha(1));

    assert_eq!(got.session_id, None, "{got:?}");
    assert_eq!(got.basis, basis::COMMAND_ORDER_UNKNOWN);
}

#[test]
fn an_unknown_git_verb_never_links_and_makes_another_sessions_link_ambiguous() {
    let (_d, repo) = repo_dir();
    let at = t0();
    // A alias nobody here can read: `git ci` may be `git commit`.
    command_row(&repo, None, "A", "git ci -m x", at - 3 * SEC);
    command_row(&repo, None, "B", "git commit -m y", at - SEC);
    reflog_row(&repo, None, &sha(1), "commit", at);

    let got = link(&repo, &sha(1));

    assert_ne!(got.session_id.as_deref(), Some("B"), "{got:?}");
    assert!(got.ambiguous, "A may have made it: {got:?}");

    // Alone, it is named, and nothing is credited.
    let (_d2, repo2) = repo_dir();
    command_row(&repo2, None, "A", "git ci -m x", at - SEC);
    reflog_row(&repo2, None, &sha(2), "commit", at);
    let alone = link(&repo2, &sha(2));
    assert_eq!(alone.session_id, None);
    assert_eq!(alone.basis, basis::COMMAND_TARGET_UNKNOWN);
}

#[test]
fn only_verbs_known_to_make_no_commit_are_read_only() {
    let t = tree();
    let w = t.w.as_path();
    for read_only in [
        "git status",
        "git log --oneline -5",
        "git diff HEAD",
        "git show abc",
        "git rev-parse HEAD",
        "git branch --list",
        "git ls-files",
        "git fetch origin",
        "git push origin main",
        "git add -A",
        "git checkout main",
        "git reset --hard",
        "git stash",
        "git -C . status",
        "git --no-pager log",
        "git status && git diff && git log",
    ] {
        assert_eq!(
            command_target(read_only, w),
            Target::Elsewhere,
            "{read_only}"
        );
    }
    for unknown in [
        "git ci -m x",
        "git co main",
        "git c",
        "git foo",
        "git bisect run make",
        "git amend",
    ] {
        assert_eq!(command_target(unknown, w), Target::Unknown, "{unknown}");
    }
    // The commit-making verbs git itself has, `revert` and `am` among them.
    for here in [
        "git commit -m x",
        "git merge x",
        "git rebase x",
        "git cherry-pick x",
        "git pull",
        "git revert x",
        "git am p.patch",
    ] {
        assert_eq!(command_target(here, w), Target::Here, "{here}");
    }
}

#[test]
fn how_many_commits_a_command_makes_is_a_single_commit_unless_the_words_say_a_run() {
    let t = tree();
    let w = t.w.as_path();
    let shape = |command: &str| read_command(command, w).1;
    for single in [
        "git commit -m x",
        "git merge x",
        "git revert x",
        "git am p.patch",
        "git pull",
        "git pull --ff-only",
        "git cherry-pick abc",
        "git cherry-pick --continue",
        "git commit -m a && git rebase main",
    ] {
        assert!(!shape(single).is_run(), "{single}");
    }
    for run in [
        "git rebase main",
        "git rebase -i HEAD~3",
        "git pull --rebase",
        "git pull -r",
        "git cherry-pick a..b",
        "git cherry-pick a b",
        "git rebase main && git rebase main",
    ] {
        assert!(shape(run).is_run(), "{run}");
    }
}

// ---- round 4: an entry is credited only to a command that writes its verb,
// and a command's end bounds the entries it can have made ----

/// A transcript command row that ran for `duration_ms`: the call's result was
/// read and timed, so its completion is known.
fn command_row_timed(
    repo: &str,
    worktree: Option<&str>,
    session: &str,
    text: &str,
    at: u64,
    duration_ms: i64,
) {
    append(Draft {
        repo_path: repo.into(),
        worktree_path: worktree.map(str::to_string),
        actor_kind: Some(ActorKind::Agent),
        actor_id: Some("claude".into()),
        session_id: Some(session.into()),
        action: "session.command".into(),
        object: Some(text.into()),
        outcome: Some(Outcome::Ok),
        duration_ms: Some(duration_ms),
        detail_json: Some(r#"{"result":"ok"}"#.into()),
        event_stamp: Some(stamp(at, "transcript")),
        ..Draft::default()
    })
    .expect("append timed command");
}

/// The command that writes a reflog `message`'s verb, run so that it covers an
/// entry written at `at`: what a table of entry forms is read against.
fn command_row_for(repo: &str, session: &str, message: &str, at: u64) {
    let verb = message.split([' ', ':', '(']).next().unwrap_or("");
    let text = match verb {
        "cherry-pick" => "git cherry-pick abc",
        "revert" => "git revert abc",
        "am" => "git am p.patch",
        "rebase" => "git rebase main",
        "pull" => "git pull",
        "merge" => "git merge feat",
        _ => "git commit -m x",
    };
    command_row_timed(repo, None, session, text, at - SEC, 2_000);
}

#[test]
fn a_pull_that_found_nothing_does_not_make_a_hand_commit_after_it() {
    let (_d, repo) = repo_dir();
    let at = t0();
    // `git pull` exited 0 with "Already up to date": no reflog row. A human
    // commits 40 s later, inside the window.
    command_row(&repo, None, "PULLER", "git pull", at - 40 * SEC);
    reflog_row(&repo, None, &sha(1), "commit", at);

    let got = link(&repo, &sha(1));

    assert_eq!(got.session_id, None, "{got:?}");
    assert_eq!(got.basis, basis::COMMAND_SPENT);

    // The same, whatever the pull's completion was timed as.
    let (_d2, repo2) = repo_dir();
    command_row_timed(&repo2, None, "PULLER", "git pull", at - 40 * SEC, 2_000);
    reflog_row(&repo2, None, &sha(2), "commit", at);
    assert_eq!(link(&repo2, &sha(2)).session_id, None);
}

#[test]
fn a_merge_that_found_nothing_does_not_make_someone_elses_merge_after_it() {
    let at = t0();
    // Without the command's end, a merge that made nothing and one that made
    // this entry look the same: no link, and a name for why.
    let (_d, repo) = repo_dir();
    command_row(&repo, None, "MERGER", "git merge feat", at - 40 * SEC);
    reflog_row(
        &repo,
        None,
        &sha(1),
        "merge other: Merge made by the 'ort' strategy.",
        at,
    );
    let untimed = link(&repo, &sha(1));
    assert_eq!(untimed.session_id, None, "{untimed:?}");
    assert_eq!(untimed.basis, basis::COMMAND_OUTCOME_UNKNOWN);

    // With it, the entry is after the command finished: not its entry.
    let (_d2, repo2) = repo_dir();
    command_row_timed(
        &repo2,
        None,
        "MERGER",
        "git merge feat",
        at - 40 * SEC,
        2_000,
    );
    reflog_row(
        &repo2,
        None,
        &sha(2),
        "merge other: Merge made by the 'ort' strategy.",
        at,
    );
    let timed = link(&repo2, &sha(2));
    assert_eq!(timed.session_id, None, "{timed:?}");
    assert_eq!(timed.basis, basis::COMMAND_SPENT);

    // And a merge that did make its entry, timed, links.
    let (_d3, repo3) = repo_dir();
    command_row_timed(
        &repo3,
        None,
        "MERGER",
        "git merge feat",
        at - 2 * SEC,
        4_000,
    );
    reflog_row(
        &repo3,
        None,
        &sha(3),
        "merge feat: Merge made by the 'ort' strategy.",
        at,
    );
    assert_eq!(link(&repo3, &sha(3)).session_id.as_deref(), Some("MERGER"));
}

#[test]
fn a_hand_commit_during_a_slow_commit_is_ambiguous_when_the_commits_end_is_known() {
    let (_d, repo) = repo_dir();
    let at = t0();
    // The agent's `git commit` starts at `at`; its pre-commit hooks take 60 s;
    // a human commits at +10; the agent's commit lands at +61.
    command_row_timed(&repo, None, "AGENT", "git commit -m a", at, 62_000);
    reflog_row(&repo, None, &sha(1), "commit", at + 10 * SEC);
    reflog_row(&repo, None, &sha(2), "commit", at + 61 * SEC);

    for n in [1, 2] {
        let got = link(&repo, &sha(n));
        assert_eq!(got.session_id, None, "commit {n}: {got:?}");
        assert!(
            got.ambiguous,
            "two actors committed inside one command: {got:?}"
        );
    }

    // With only its own entry inside, the end makes the link certain.
    let (_d2, repo2) = repo_dir();
    command_row_timed(&repo2, None, "AGENT", "git commit -m a", at, 62_000);
    reflog_row(&repo2, None, &sha(3), "commit", at + 61 * SEC);
    reflog_row(&repo2, None, &sha(4), "commit", at + 90 * SEC);
    assert_eq!(link(&repo2, &sha(3)).session_id.as_deref(), Some("AGENT"));
    let after = link(&repo2, &sha(4));
    assert_eq!(
        after.session_id, None,
        "after the command finished: {after:?}"
    );
}

#[test]
fn a_command_with_no_timed_end_is_credited_with_its_verbs_first_entry_and_that_is_the_residual() {
    // Documented, not endorsed: without the command's end, a human's commit
    // that lands during a slow hook, first, is the first entry of the verb after
    // the command and is credited to it. Only the command's end can tell them
    // apart, and a row whose result was never timed does not have one.
    let (_d, repo) = repo_dir();
    let at = t0();
    command_row(&repo, None, "AGENT", "git commit -m a", at);
    reflog_row(&repo, None, &sha(1), "commit", at + 10 * SEC);
    reflog_row(&repo, None, &sha(2), "commit", at + 61 * SEC);

    let human_first = link(&repo, &sha(1));
    let agents_own = link(&repo, &sha(2));

    assert_eq!(human_first.session_id.as_deref(), Some("AGENT"));
    assert_eq!(agents_own.session_id, None);
}

#[test]
fn a_verb_the_command_does_not_write_is_not_credited() {
    let at = t0();
    for (command, message) in [
        (
            "git commit -m a",
            "merge x: Merge made by the 'ort' strategy.",
        ),
        ("git commit -m a", "cherry-pick: picked"),
        ("git merge feat", "commit: hand"),
        ("git pull", "commit: hand"),
        ("git revert abc", "commit: hand"),
        ("git cherry-pick abc", "revert: hand"),
    ] {
        let (_d, repo) = repo_dir();
        command_row_timed(&repo, None, "S1", command, at - SEC, 4_000);
        reflog_row(&repo, None, &sha(1), message, at);
        let got = link(&repo, &sha(1));
        assert_eq!(got.session_id, None, "{command} / {message}: {got:?}");
        assert_eq!(got.basis, basis::COMMAND_SPENT, "{command} / {message}");
    }
}

#[test]
fn an_intervening_entry_of_another_verb_does_not_spend_a_commit() {
    let (_d, repo) = repo_dir();
    let at = t0();
    command_row(&repo, None, "S1", "git commit -m a", at - 5 * SEC);
    // A human's merge lands between the command and its commit: not a commit
    // of the verb the command writes.
    reflog_row(
        &repo,
        None,
        &sha(8),
        "merge x: Merge made by the 'ort' strategy.",
        at - 2 * SEC,
    );
    reflog_row(&repo, None, &sha(1), "commit", at);

    assert_eq!(link(&repo, &sha(1)).session_id.as_deref(), Some("S1"));
}

#[test]
fn the_no_command_basis_says_ingested() {
    let (_d, repo) = repo_dir();
    reflog_row(&repo, None, &sha(1), "commit", t0());

    let got = link(&repo, &sha(1));

    assert_eq!(got.basis, "no_ingested_session_command");
    assert!(
        !basis::ALL.contains(&"no_session_command"),
        "a basis that reads as 'no agent was involved' is gone"
    );
}
