mod common;

use gitpulse_lib::engine::approve_worktree_hooks;
use gitpulse_lib::engine::git_cli::git_text;
use gitpulse_lib::engine::worktree::{
    add_worktree_extended, agent_layout, fork_worktree_extended, is_gitpulse_lane,
    read_base_record, write_base_record, DevmapMode,
};
use gitpulse_lib::workbench::agent_worktree::plan_fork_target;
use gitpulse_lib::workbench::conversation::{
    claude_project_dir_name, latest_claude_session_for_checkout,
};
use gitpulse_lib::workbench::terminal_command::{
    fork_resume, fork_resume_for_checkout, interactive_args, refuse_raw_session_args,
    LaunchOptions, CLAUDE_FORK_DOC_URL, CLAUDE_FORK_SESSION_FLAG, CLAUDE_FORK_VERIFIED_VERSION,
};
use std::path::Path;

fn setup_repo_with_source_worktree(hooks: Option<&str>) -> (tempfile::TempDir, tempfile::TempDir) {
    common::unmetered_spawns();
    let dir = tempfile::tempdir().expect("repo tempdir");
    let repo = dir.path();
    common::run_git(repo, &["init", "-b", "main", "."]);
    std::fs::write(repo.join("README.md"), "hello\n").unwrap();
    if let Some(hooks_json) = hooks {
        std::fs::create_dir_all(repo.join(".gitpulse")).unwrap();
        std::fs::write(repo.join(".gitpulse/hooks.json"), hooks_json).unwrap();
    }
    common::run_git(repo, &["add", "."]);
    common::run_git(repo, &["commit", "-m", "initial commit"]);
    if let Some(hooks_json) = hooks {
        approve_worktree_hooks(repo, hooks_json).expect("approve fixture hooks");
    }

    let src_wt = tempfile::tempdir().expect("source worktree tempdir");
    let created = add_worktree_extended(
        repo.to_str().unwrap(),
        src_wt.path().to_str().unwrap(),
        Some("feature-x"),
        None,
        false,
        false,
        false,
    )
    .expect("create source linked worktree");
    assert_eq!(created.path, src_wt.path().to_str().unwrap());
    common::trust_repo(src_wt.path());
    (dir, src_wt)
}

#[test]
fn fork_lane_carries_staged_unstaged_untracked_preserves_source_and_stash_and_records_base() {
    let (dir, src_wt) = setup_repo_with_source_worktree(None);
    let repo = dir.path();
    let src = src_wt.path();

    let base_oid = git_text(repo, &["rev-parse", "refs/heads/main"])
        .unwrap()
        .trim()
        .to_string();
    write_base_record(src, "main", &base_oid).unwrap();

    // Seed a pre-existing stash entry on the repository stack.
    std::fs::write(repo.join("stash-seed.txt"), "keep on stack\n").unwrap();
    common::run_git(repo, &["add", "stash-seed.txt"]);
    common::run_git(repo, &["stash", "push", "-m", "preexisting"]);
    let stash_before = git_text(repo, &["stash", "list"]).unwrap();
    assert!(!stash_before.trim().is_empty());

    // Advance source lane HEAD ahead of main, then leave staged, unstaged, untracked, and ignored files.
    std::fs::write(src.join("extra.txt"), "base extra\n").unwrap();
    common::run_git(src, &["add", "extra.txt"]);
    common::run_git(src, &["commit", "-m", "source commit"]);
    let src_head = git_text(src, &["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    std::fs::write(src.join(".gitignore"), "*.ignored\n").unwrap();
    common::run_git(src, &["add", ".gitignore"]);
    std::fs::write(src.join("README.md"), "staged readme edit\n").unwrap();
    common::run_git(src, &["add", "README.md"]);
    std::fs::write(src.join("extra.txt"), "unstaged extra edit\n").unwrap();
    std::fs::create_dir_all(src.join("nested")).unwrap();
    std::fs::write(src.join("nested/untracked.txt"), "fresh untracked note\n").unwrap();
    std::fs::write(src.join("skip.ignored"), "do not copy ignored\n").unwrap();
    let src_porcelain = git_text(src, &["status", "--porcelain"]).unwrap();

    let (planned_path, planned_branch) =
        plan_fork_target(repo.to_str().unwrap(), src.to_str().unwrap(), None)
            .expect("plan_fork_target");
    let mut gated: Vec<Vec<String>> = Vec::new();
    let created = fork_worktree_extended(
        repo.to_str().unwrap(),
        src.to_str().unwrap(),
        &planned_path,
        Some(&planned_branch),
        false,
        false,
        DevmapMode::None,
        &mut |args| {
            gated.push(args.iter().map(|s| (*s).to_string()).collect());
            Ok(())
        },
    )
    .expect("fork_worktree_extended succeeds");
    let fork_path = Path::new(&created.path);
    common::trust_repo(fork_path);

    let layout = agent_layout(&created.path).expect("planned fork lives in .gitpulse/worktrees");
    assert!(is_gitpulse_lane(&layout.kind));
    assert_eq!(created.branch.as_deref(), Some(planned_branch.as_str()));
    assert_eq!(created.head_oid.as_deref(), Some(src_head.as_str()));
    assert!(created.carried_uncommitted);
    assert!(
        gated
            .iter()
            .any(|args| args.first().map(String::as_str) == Some("stash")),
        "stash create/apply must flow through gate: {gated:?}"
    );

    assert_eq!(
        git_text(fork_path, &["rev-parse", "HEAD"]).unwrap().trim(),
        src_head
    );
    assert_eq!(
        git_text(fork_path, &["status", "--porcelain"]).unwrap(),
        src_porcelain
    );
    assert_eq!(
        std::fs::read_to_string(fork_path.join("nested/untracked.txt")).unwrap(),
        "fresh untracked note\n"
    );
    assert!(!fork_path.join("skip.ignored").exists());
    assert_eq!(
        read_base_record(fork_path).unwrap(),
        Some(("main".to_string(), base_oid))
    );

    // Source working tree, main checkout exclusion, and stash stack remain untouched.
    assert_eq!(
        git_text(src, &["status", "--porcelain"]).unwrap(),
        src_porcelain
    );
    assert_eq!(git_text(repo, &["stash", "list"]).unwrap(), stash_before);
    let main_status = git_text(repo, &["status", "--porcelain"]).unwrap();
    assert!(
        !main_status.contains(".gitpulse"),
        "main checkout status must keep .gitpulse/ excluded: {main_status:?}"
    );
}

#[cfg(unix)]
#[test]
fn fork_lane_stash_conflict_reports_error_and_removes_fork_worktree_and_branch() {
    let (dir, src_wt) = setup_repo_with_source_worktree(Some(
        r#"{"worktree":{"post_create":["printf 'hook conflict\\n' > README.md && git add README.md && git commit -m hook"]}}"#,
    ));
    let repo = dir.path();
    let src = src_wt.path();

    std::fs::write(src.join("README.md"), "source conflicting edit\n").unwrap();
    let src_porcelain = git_text(src, &["status", "--porcelain"]).unwrap();

    let fork_dir = tempfile::tempdir().unwrap();
    let fork_target = fork_dir.path().join("fork-conflict");
    let err = fork_worktree_extended(
        repo.to_str().unwrap(),
        src.to_str().unwrap(),
        fork_target.to_str().unwrap(),
        Some("feature-x-conflict"),
        false,
        true,
        DevmapMode::None,
        &mut |_| Ok(()),
    )
    .expect_err("conflicting fork must fail closed");

    let msg = err.message();
    assert!(
        msg.contains("conflicted") || msg.contains("Applying uncommitted changes"),
        "unexpected conflict message: {msg}"
    );
    assert!(
        !fork_target.exists(),
        "conflicted fork worktree must be removed"
    );
    assert!(
        git_text(
            repo,
            &["rev-parse", "--verify", "refs/heads/feature-x-conflict"]
        )
        .is_err(),
        "conflicted fork branch must be deleted"
    );
    assert_eq!(
        git_text(src, &["status", "--porcelain"]).unwrap(),
        src_porcelain
    );
}

#[test]
fn fork_resume_owner_uses_documented_flag_and_spawn_refuses_raw_flags() {
    let serial = common::env::env_serial();
    let cfg = tempfile::tempdir().unwrap();
    let _env = common::env::bind_env(&serial).set("CLAUDE_CONFIG_DIR", cfg.path());

    let checkout = Path::new("/tmp/fork-lane-checkout-check");
    let source_id = "11111111-1111-4111-8111-111111111111";
    let fork_id = "22222222-2222-4222-8222-222222222222";
    let project_dir = cfg
        .path()
        .join("projects")
        .join(claude_project_dir_name(checkout));
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(project_dir.join(format!("{source_id}.jsonl")), "{}\n").unwrap();

    assert_eq!(
        latest_claude_session_for_checkout(checkout).as_deref(),
        Some(source_id)
    );
    assert_eq!(CLAUDE_FORK_SESSION_FLAG, "--fork-session");
    assert_eq!(CLAUDE_FORK_VERIFIED_VERSION, "2.1.325");
    assert_eq!(
        CLAUDE_FORK_DOC_URL,
        "https://code.claude.com/docs/en/cli-reference"
    );

    let argv = fork_resume("claude", source_id, fork_id).expect("fork_resume");
    assert_eq!(
        argv,
        vec![
            "--resume",
            source_id,
            "--fork-session",
            "--session-id",
            fork_id,
        ]
    );

    // Colliding fork UUID or missing source transcript fail closed.
    std::fs::write(project_dir.join(format!("{fork_id}.jsonl")), "{}\n").unwrap();
    assert!(fork_resume("claude", source_id, fork_id).is_err());
    assert!(fork_resume(
        "claude",
        "33333333-3333-4333-8333-333333333333",
        "44444444-4444-4444-8444-444444444444",
    )
    .is_err());

    // Raw --resume / --fork / --fork-session / --session-id remain refused in cmd_terminal_spawn args.
    for raw in [
        vec!["--resume".into(), source_id.into()],
        vec!["-r".into(), source_id.into()],
        vec!["--fork".into()],
        vec!["--fork-session".into()],
        vec![format!("--fork-session={fork_id}")],
        vec!["--session-id".into(), fork_id.into()],
    ] {
        assert!(
            refuse_raw_session_args(Some("claude"), Some(&raw)).is_err(),
            "expected refusal for {raw:?}"
        );
    }
    assert!(refuse_raw_session_args(
        Some("claude"),
        Some(&[
            "--".into(),
            "--resume".into(),
            "--fork".into(),
            "--fork-session".into(),
        ]),
    )
    .is_ok());
}

#[test]
fn cross_worktree_fork_resume_resolves_source_transcript_across_checkouts() {
    let serial = common::env::env_serial();
    let cfg = tempfile::tempdir().unwrap();
    let _env = common::env::bind_env(&serial).set("CLAUDE_CONFIG_DIR", cfg.path());

    let wt_src = tempfile::tempdir().unwrap();
    let wt_fork = tempfile::tempdir().unwrap();
    let wt_src_canon = wt_src.path().canonicalize().unwrap();

    let source_id = "11111111-1111-4111-8111-111111111111";
    let fork_id = "22222222-2222-4222-8222-222222222222";
    let proj_src = cfg
        .path()
        .join("projects")
        .join(claude_project_dir_name(&wt_src_canon));
    std::fs::create_dir_all(&proj_src).unwrap();
    let transcript_path = proj_src.join(format!("{source_id}.jsonl"));
    std::fs::write(&transcript_path, "{\"type\":\"user\"}\n").unwrap();

    // Launching inside a newly forked worktree whose project dir lacks `<source_id>.jsonl`
    // must pass the source transcript file path so Claude Code 2.1.325 finds it instead of
    // failing with `No conversation found with session ID`.
    let fork_argv = interactive_args(
        Some("claude"),
        Some("edit"),
        false,
        &LaunchOptions::default(),
        wt_fork.path(),
        Some(source_id),
        Some(fork_id),
        true,
        None,
    )
    .expect("cross-worktree fork interactive_args")
    .expect("claude argv");
    let fork_tail = fork_argv.iter().position(|a| a == "--resume").unwrap();
    assert_eq!(
        fork_argv[fork_tail..],
        [
            "--resume",
            transcript_path.to_str().unwrap(),
            "--fork-session",
            "--session-id",
            fork_id,
        ]
    );

    // Direct checkout-aware API matches the same cross-checkout path resolution.
    assert_eq!(
        fork_resume_for_checkout("claude", source_id, fork_id, wt_fork.path())
            .expect("fork_resume_for_checkout cross-worktree"),
        vec![
            "--resume".to_owned(),
            transcript_path.to_str().unwrap().to_owned(),
            "--fork-session".to_owned(),
            "--session-id".to_owned(),
            fork_id.to_owned(),
        ]
    );

    // Launching in the source checkout itself keeps the bare UUID form.
    let same_argv = interactive_args(
        Some("claude"),
        Some("edit"),
        false,
        &LaunchOptions::default(),
        wt_src.path(),
        Some(source_id),
        Some(fork_id),
        true,
        None,
    )
    .expect("same-checkout fork interactive_args")
    .expect("claude argv");
    let same_tail = same_argv.iter().position(|a| a == "--resume").unwrap();
    assert_eq!(
        same_argv[same_tail..],
        [
            "--resume",
            source_id,
            "--fork-session",
            "--session-id",
            fork_id,
        ]
    );
}
