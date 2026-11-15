//! Integration contract tests for the durable terminal session ledger,
//! cross-surface recovery offers, and single Rust resume-argv owner
//! (`S2-03`, `S2-05`, `S2-07`).

#[path = "../src/test_support/env.rs"]
mod hermetic_env;

use std::collections::HashSet;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use gitpulse_lib::alerts::bridge::{parse_report_stamped, Reason};
use gitpulse_lib::terminal::session_ledger::{
    NewSessionRow, SessionEndState, SessionLedger, MAX_LEDGER_ROWS,
};
use gitpulse_lib::workbench::conversation::{check_session_resumability, has_transcript_locator};
use gitpulse_lib::workbench::terminal_command::{
    interactive_args, refuse_raw_session_args, resume, LaunchOptions, AGY_CONTINUE_SENTINEL,
};
use tempfile::TempDir;

fn write_claude_transcript(session_id: &str) -> PathBuf {
    let home = std::env::var_os("HOME").expect("hermetic_env must set HOME");
    let projects = PathBuf::from(home)
        .join(".claude")
        .join("projects")
        .join("test-proj");
    fs::create_dir_all(&projects).expect("create claude projects dir");
    let file = projects.join(format!("{session_id}.jsonl"));
    fs::write(&file, "{\"type\":\"user\",\"message\":\"hi\"}\n").expect("write transcript");
    file
}

fn spawn_spec(
    terminal_id: &str,
    launcher: &str,
    native_id: Option<&str>,
    repo: &Path,
    cwd: &Path,
    mode: Option<&str>,
) -> NewSessionRow {
    NewSessionRow {
        terminal_id: terminal_id.to_owned(),
        launcher: launcher.to_owned(),
        native_id: native_id.map(str::to_owned),
        repo_path: repo.to_string_lossy().into_owned(),
        start_dir: cwd
            .strip_prefix(repo)
            .ok()
            .filter(|rel| !rel.as_os_str().is_empty())
            .map(|rel| rel.to_string_lossy().into_owned()),
        cwd: cwd.to_string_lossy().into_owned(),
        mode: mode.map(str::to_owned),
        reopen_row_id: None,
    }
}

#[test]
fn s2_03_sigkill_relaunch_marks_running_row_lost() {
    let tmp = TempDir::new().unwrap();
    let ledger_path = tmp.path().join("terminal-sessions.v1.jsonl");
    let repo = tmp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    let sid = "6f0a6d1e-1111-4222-8333-444455550001";
    write_claude_transcript(sid);

    let ledger = SessionLedger::open(&ledger_path);
    ledger.record_spawn(spawn_spec(
        "term-sigkill",
        "claude",
        Some(sid),
        &repo,
        &repo,
        Some("edit"),
    ));
    drop(ledger);

    let relaunched = SessionLedger::open(&ledger_path);
    let lost_count = relaunched.reconcile_startup(&HashSet::new());
    assert_eq!(lost_count, 1);

    let view = relaunched.view();
    assert_eq!(view.reopenable_count, 1);
    let offer = view
        .offers
        .iter()
        .find(|o| o.terminal_id.as_deref() == Some("term-sigkill"))
        .expect("lost offer present after relaunch");
    assert_eq!(offer.end_state, SessionEndState::Lost);
    assert!(offer.resumable, "expected resumable offer: {offer:?}");
    assert_eq!(offer.native_id.as_deref(), Some(sid));
}

#[test]
fn s2_03_cmd_q_three_claude_tabs_app_quit_and_reopen_argv() {
    let tmp = TempDir::new().unwrap();
    let ledger_path = tmp.path().join("terminal-sessions.v1.jsonl");
    let repo = tmp.path().join("repo");
    let sub_a = repo.join("packages/a");
    let sub_b = repo.join("packages/b");
    fs::create_dir_all(&sub_a).unwrap();
    fs::create_dir_all(&sub_b).unwrap();

    let s1 = "6f0a6d1e-1111-4222-8333-444455550011";
    let s2 = "6f0a6d1e-1111-4222-8333-444455550012";
    let s3 = "6f0a6d1e-1111-4222-8333-444455550013";
    for sid in [s1, s2, s3] {
        write_claude_transcript(sid);
    }

    let ledger = SessionLedger::open(&ledger_path);
    ledger.record_spawn(spawn_spec(
        "t1",
        "claude",
        Some(s1),
        &repo,
        &repo,
        Some("edit"),
    ));
    ledger.record_spawn(spawn_spec(
        "t2",
        "claude",
        Some(s2),
        &repo,
        &sub_a,
        Some("bypass"),
    ));
    ledger.record_spawn(spawn_spec("t3", "claude", Some(s3), &repo, &sub_b, None));
    ledger.record_spawn(spawn_spec("t-shell", "shell", None, &repo, &repo, None));

    // Teardown marks app_quit synchronously before sending SIGHUP.
    let marked = ledger.mark_app_quit();
    assert_eq!(marked, 4);

    // Late SIGHUP exit callbacks must NEVER overwrite AppQuit.
    for tid in ["t1", "t2", "t3", "t-shell"] {
        assert!(!ledger.record_exit(tid, true, None, Some("Hangup: 1")));
    }
    drop(ledger);

    let relaunched = SessionLedger::open(&ledger_path);
    let view = relaunched.view();
    assert_eq!(view.reopenable_count, 3);

    let shell = view
        .offers
        .iter()
        .find(|o| o.terminal_id.as_deref() == Some("t-shell"))
        .expect("shell tab listed as non-restorable");
    assert_eq!(shell.end_state, SessionEndState::AppQuit);
    assert!(!shell.resumable);
    assert_eq!(shell.reason.as_deref(), Some("not restorable"));

    for (tid, expected_sid, expected_cwd, expected_mode, expected_flag) in [
        ("t1", s1, repo.as_path(), Some("edit"), Some("acceptEdits")),
        (
            "t2",
            s2,
            sub_a.as_path(),
            Some("bypass"),
            Some("bypassPermissions"),
        ),
        ("t3", s3, sub_b.as_path(), None, None),
    ] {
        let offer = view
            .offers
            .iter()
            .find(|o| o.terminal_id.as_deref() == Some(tid))
            .expect("claude offer present");
        assert_eq!(offer.end_state, SessionEndState::AppQuit);
        assert!(offer.resumable);
        assert_eq!(offer.cwd, expected_cwd.to_string_lossy());
        assert_eq!(offer.mode.as_deref(), expected_mode);

        let argv = interactive_args(
            Some("claude"),
            offer.mode.as_deref(),
            offer.mode.as_deref() == Some("bypass"),
            &LaunchOptions::default(),
            Path::new(&offer.cwd),
            offer.native_id.as_deref(),
            None,
            false,
            None,
        )
        .expect("build reopen argv")
        .expect("non-empty argv");
        assert!(
            argv.windows(2).any(|w| w == ["--resume", expected_sid]),
            "expected [--resume, {expected_sid}] in {argv:?}"
        );
        if let Some(perm_val) = expected_flag {
            assert!(
                argv.windows(2)
                    .any(|w| w == ["--permission-mode", perm_val]),
                "expected [--permission-mode, {perm_val}] in {argv:?}"
            );
        } else {
            assert!(
                !argv.iter().any(|a| a == "--permission-mode"),
                "did not expect --permission-mode in {argv:?}"
            );
        }
    }
}

#[test]
fn s2_03_gone_cwd_clean_exit_bounds_and_corrupt_row() {
    let tmp = TempDir::new().unwrap();
    let ledger_path = tmp.path().join("terminal-sessions.v1.jsonl");
    let repo = tmp.path().join("repo");
    let gone_cwd = tmp.path().join("gone-worktree");
    fs::create_dir_all(&repo).unwrap();
    fs::create_dir_all(&gone_cwd).unwrap();

    let sid_gone = "6f0a6d1e-1111-4222-8333-444455550021";
    let sid_clean = "6f0a6d1e-1111-4222-8333-444455550022";
    let sid_closed = "6f0a6d1e-1111-4222-8333-444455550023";
    for sid in [sid_gone, sid_clean, sid_closed] {
        write_claude_transcript(sid);
    }

    let ledger = SessionLedger::open(&ledger_path);
    let gone_row_id = ledger.record_spawn(spawn_spec(
        "t-gone",
        "claude",
        Some(sid_gone),
        &repo,
        &gone_cwd,
        Some("edit"),
    ));
    ledger.record_spawn(spawn_spec(
        "t-clean",
        "claude",
        Some(sid_clean),
        &repo,
        &repo,
        Some("edit"),
    ));
    ledger.record_spawn(spawn_spec(
        "t-closed",
        "claude",
        Some(sid_closed),
        &repo,
        &repo,
        Some("edit"),
    ));

    // Clean /exit and explicit user Close never appear in Reopen offers.
    assert!(ledger.record_exit("t-clean", false, Some(0), None));
    assert!(ledger.mark_user_closed("t-closed"));

    // Quit marks t-gone AppQuit, then remove its cwd before Reopen check.
    assert_eq!(ledger.mark_app_quit(), 1);
    fs::remove_dir_all(&gone_cwd).unwrap();

    let check = ledger.check_reopen(&gone_row_id).expect("check gone row");
    assert!(!check.offer.resumable);
    let err = check
        .offer
        .last_error
        .as_deref()
        .expect("gone cwd sets per-row last_error");
    assert!(err.contains("Working directory no longer exists"), "{err}");

    let failed = ledger
        .record_reopen_failure(&gone_row_id, "custom reopen failure".into())
        .expect("row stays listed");
    assert_eq!(failed.last_error.as_deref(), Some("custom reopen failure"));

    let view = ledger.view();
    assert!(view
        .offers
        .iter()
        .all(|o| o.terminal_id.as_deref() != Some("t-clean")));
    assert!(view
        .offers
        .iter()
        .all(|o| o.terminal_id.as_deref() != Some("t-closed")));
    let still_listed = view
        .offers
        .iter()
        .find(|o| o.row_id == gone_row_id)
        .expect("gone cwd row stays listed with error");
    assert_eq!(
        still_listed.last_error.as_deref(),
        Some("custom reopen failure")
    );

    // Bounded row store never grows past MAX_LEDGER_ROWS on disk.
    for idx in 0..(MAX_LEDGER_ROWS + 12) {
        let tid = format!("t-bound-{idx}");
        ledger.record_spawn(spawn_spec(&tid, "claude", None, &repo, &repo, None));
        ledger.record_exit(&tid, false, Some(0), None);
    }
    let persisted_lines = fs::read_to_string(&ledger_path)
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .count();
    assert!(
        persisted_lines <= MAX_LEDGER_ROWS,
        "expected <= {MAX_LEDGER_ROWS} lines, got {persisted_lines}"
    );
    drop(ledger);

    // Corrupt line in JSONL surfaces as Unreadable ("unreadable") and can be dismissed.
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(&ledger_path)
        .unwrap();
    writeln!(file, "{{not-valid-json").unwrap();
    drop(file);

    let reopened = SessionLedger::open(&ledger_path);
    let view_with_corrupt = reopened.view();
    assert_eq!(view_with_corrupt.unreadable_count, 1);
    let corrupt = view_with_corrupt
        .offers
        .iter()
        .find(|o| o.end_state == SessionEndState::Unreadable)
        .expect("corrupt row projected as Unreadable");
    assert!(!corrupt.resumable);
    assert_eq!(corrupt.reason.as_deref(), Some("unreadable"));
    assert!(reopened.dismiss(&corrupt.row_id));
    assert_eq!(reopened.view().unreadable_count, 0);
}

#[test]
fn s2_05_persists_across_reloads_until_reopened_or_dismissed() {
    let tmp = TempDir::new().unwrap();
    let ledger_path = tmp.path().join("terminal-sessions.v1.jsonl");
    let repo = tmp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();

    let s1 = "6f0a6d1e-1111-4222-8333-444455550031";
    let s2 = "6f0a6d1e-1111-4222-8333-444455550032";
    write_claude_transcript(s1);
    write_claude_transcript(s2);

    let ledger = SessionLedger::open(&ledger_path);
    let r1 = ledger.record_spawn(spawn_spec(
        "t1",
        "claude",
        Some(s1),
        &repo,
        &repo,
        Some("edit"),
    ));
    let r2 = ledger.record_spawn(spawn_spec(
        "t2",
        "claude",
        Some(s2),
        &repo,
        &repo,
        Some("edit"),
    ));
    assert_eq!(ledger.mark_app_quit(), 2);
    drop(ledger);

    // Survives repeated reloads without being withdrawn.
    for _ in 0..2 {
        let reload = SessionLedger::open(&ledger_path);
        let view = reload.view();
        assert_eq!(view.reopenable_count, 2);
        assert!(view.offers.iter().any(|o| o.row_id == r1));
        assert!(view.offers.iter().any(|o| o.row_id == r2));
    }

    // Reopening row 1 via record_spawn withdraws row 1 while row 2 remains offered.
    let ledger = SessionLedger::open(&ledger_path);
    ledger.record_spawn(NewSessionRow {
        reopen_row_id: Some(r1.clone()),
        ..spawn_spec(
            "t1-reopened",
            "claude",
            Some(s1),
            &repo,
            &repo,
            Some("edit"),
        )
    });
    let after_reopen = ledger.view();
    assert!(!after_reopen.offers.iter().any(|o| o.row_id == r1));
    assert!(after_reopen.offers.iter().any(|o| o.row_id == r2));

    // Dismissing row 2 removes the remaining offer from disk.
    assert!(ledger.dismiss(&r2));
    drop(ledger);
    let final_reload = SessionLedger::open(&ledger_path);
    assert!(final_reload.view().offers.is_empty());
}

#[test]
fn s2_07_refuses_raw_flags_checks_locators_and_blocks_hook_argv() {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path();
    let sid = "6f0a6d1e-1111-4222-8333-444455550041";

    for raw in [
        vec!["--resume".into(), "x".into()],
        vec!["-r".into(), "x".into()],
        vec!["--session-id".into(), sid.into()],
        vec!["--continue".into()],
        vec!["-c".into()],
        vec!["--conversation".into(), sid.into()],
        vec!["--fork-session".into()],
    ] {
        let err = interactive_args(
            Some("claude"),
            None,
            false,
            &LaunchOptions::default(),
            cwd,
            None,
            None,
            false,
            Some(raw.clone()),
        )
        .expect_err("raw session flags must be refused");
        assert!(!err.is_empty(), "empty error for {raw:?}");
    }

    // Literal prompt text after `--` is still permitted.
    assert!(refuse_raw_session_args(
        Some("claude"),
        Some(&["--".into(), "--resume".into(), "x".into()]),
    )
    .is_ok());

    assert_eq!(
        resume("claude", sid, cwd).unwrap(),
        vec!["--resume".to_owned(), sid.to_owned()]
    );
    let agy_cont = resume("agy", AGY_CONTINUE_SENTINEL, cwd).unwrap();
    assert_eq!(agy_cont[0], "--continue");
    assert!(agy_cont.windows(2).any(|w| w[0] == "--add-dir"));

    for provider in ["codex", "grok", "manvi"] {
        assert!(!has_transcript_locator(provider));
        let check = check_session_resumability(provider, Some(sid), cwd);
        assert!(!check.resumable);
        let reason = check.reason.expect("reason required when not resumable");
        assert!(!reason.is_empty());
    }

    for payload in [
        br#"{"v":1,"event":"error","resume_argv":["claude","--resume","x"]}"#.as_slice(),
        br#"{"v":1,"event":"error","args":["--resume","x"]}"#.as_slice(),
        br#"{"v":1,"event":"error","command":"claude --resume x"}"#.as_slice(),
    ] {
        assert_eq!(
            parse_report_stamped(payload),
            Err(Reason::ForbiddenArgvField)
        );
    }
}
