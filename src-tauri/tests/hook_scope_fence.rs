//! The agent hook's task-scope fence, driven end to end: the real
//! `gitpulse-hook` executable, a real `manvi serve`, and a repository bound to
//! a task that plans one file.
//!
//! The unit tests in `src/hooks/mod.rs` prove the hook sends the scope and
//! renders what comes back, against a scripted sidecar. What they cannot prove
//! is the half that lives in the harness: which files a command line is read
//! as writing. Only a real harness can show that gap closed, and the harness
//! owns it: DevCouncil `policy` reads the files a command writes through its
//! arguments and gives each to the same file gate `policy.check.file` uses.
//!
//! It is `#[ignore]` because it needs that harness: a Manvi built from
//! `hx/wave2` or later, whose DevCouncil pin is at or after `ec26e906`
//! (`fix(policy): command gate judges every file rm/truncate/touch/install/ln/
//! rsync/patch/dd/perl -i write against the file gate`). Run it with
//! `GITPULSE_MANVI_BIN` naming that binary:
//!
//! ```text
//! GITPULSE_MANVI_BIN=/path/to/manvi cargo test --test hook_scope_fence -- --ignored
//! ```
//!
//! # What the fence covers
//!
//! Against a plan that excludes `docs/other.md`, every command that writes it
//! through an argument is refused with `scope.unplanned`: `sed -i`, `tee`, `cp`
//! and `mv`, and `rm`, `truncate`, `touch`, `dd of=`, `perl -i`, `install`,
//! `ln`, `rsync` and `patch FILE`. A write whose target is not in the command
//! text cannot be judged against a scope, so with a scope bound it is refused
//! as `command.substitution`, with a reason naming the declared scope: `git
//! apply`, anything run through `xargs`, and `find -exec`. The hook forwards
//! every one of them verbatim with the bound scope
//! (`every_argument_writer_reaches_the_harness_verbatim_with_the_scope` in
//! `src/hooks/mod.rs`, which CI runs), so fencing one more writer is a harness
//! change with no change in this repository. When one stops being refused, the
//! assertion below fails and says so.
//!
//! Two edges stay open, and no hook can close them: a descendant that escapes
//! the agent's process group acts outside any hook (the hook judges calls, not
//! processes; teardown reaping owns processes), and an agent running under a
//! second Claude profile has no hook registered at all (Settings -> MCP copies
//! a `hooks.json` for that profile). `wiki/MCP and Agents.md` states both.
//!
//! CI does not run this file: it needs that Manvi build, and none exists at a
//! ref CI can fetch. Run it by hand when the harness changes. The fence
//! decisions that do not depend on the real harness's parsing (out-of-plan
//! Deny, in-plan allow, and a write outside the cwd's checkout) are covered in
//! CI by `the_scope_fence_decides_for_in_plan_out_of_plan_and_outside_cwd_root_files`
//! and its neighbours in `src/hooks/mod.rs`, against a plan-aware stand-in.
//!
//! Its `HOME` is scoped to the test through `bind_env`: trust records live under
//! it, and a fixture must not write the developer's own.

// The hermetic test environment: a per-process home, git config and trust
// store, rewritten before `main`. See `src/test_support/env.rs`.
#[path = "../src/test_support/env.rs"]
mod hermetic_env;
// dc-store schema 10 retired the `tasks` table; a task is a workbench item.
#[path = "../src/test_support/plant_task.rs"]
mod plant_task;

use gitpulse_lib::procguard::LockedSpawn;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

const HOOK: &str = env!("CARGO_BIN_EXE_gitpulse-hook");

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args([
            "-c",
            "user.name=Fence Fixture",
            "-c",
            "user.email=fence@fixture.invalid",
        ])
        .args(args)
        .current_dir(dir)
        .status_locked()
        .expect("run git");
    assert!(status.success(), "git {args:?} failed");
}

/// One hook answer, parsed. `None` is the documented "no decision".
fn ask(
    subcommand: &str,
    payload: &serde_json::Value,
    home: &Path,
    logs: &Path,
) -> Option<serde_json::Value> {
    let manvi = std::env::var("GITPULSE_MANVI_BIN")
        .expect("GITPULSE_MANVI_BIN names the harness under test");
    let mut child = Command::new(HOOK)
        .arg(subcommand)
        .env("HOME", home)
        .env("GITPULSE_MANVI_BIN", manvi)
        .env(gitpulse_lib::logging::LOG_DIR_ENV, logs)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn_locked()
        .expect("spawn gitpulse-hook");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(payload.to_string().as_bytes())
        .expect("write payload");
    let output = child.wait_with_output().expect("hook output");
    assert_eq!(output.status.code(), Some(0), "a hook must always exit 0");
    let text = String::from_utf8(output.stdout).expect("utf8 stdout");
    (!text.trim().is_empty()).then(|| serde_json::from_str(text.trim()).expect("one JSON document"))
}

fn denied(answer: &Option<serde_json::Value>) -> bool {
    answer
        .as_ref()
        .and_then(|value| value["hookSpecificOutput"]["permissionDecision"].as_str())
        == Some("deny")
}

#[test]
#[ignore = "requires GITPULSE_MANVI_BIN: a manvi built from Manvi hx/wave2 (DevCouncil >= ec26e906)"]
fn a_task_bound_session_cannot_write_outside_its_plan_through_any_tool() {
    let home = tempfile::tempdir().expect("home");
    let logs = tempfile::tempdir().expect("logs");
    let serial = hermetic_env::env_serial();
    let _home = hermetic_env::bind_env(&serial).set("HOME", home.path());

    let dir = tempfile::tempdir().expect("repository");
    let repo_buf = dir.path().canonicalize().expect("canonical repository");
    let repo = repo_buf.to_str().expect("utf8");
    git(&repo_buf, &["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(repo_buf.join("src")).unwrap();
    std::fs::create_dir_all(repo_buf.join("docs")).unwrap();
    std::fs::write(repo_buf.join("src/planned.rs"), "a").unwrap();
    std::fs::write(repo_buf.join("docs/other.md"), "a").unwrap();
    std::fs::write(repo_buf.join("seed.txt"), "a").unwrap();
    git(&repo_buf, &["add", "."]);
    git(&repo_buf, &["commit", "-q", "-m", "seed"]);

    // `bind` below reaches the library's git gate in this process
    // (`resolve_worktree_family` -> `git worktree list`; traced through the
    // same call in `hook_protocol_stress`, since this test is ignored without a
    // manvi binary). The trust read does not: it reads the filesystem. The hook
    // processes this test drives have gates of their own.
    gitpulse_lib::engine::process::run_process_with_unlimited_spawn_rate();
    let preview = gitpulse_lib::repository_trust::inspect(repo).expect("inspect");
    gitpulse_lib::repository_trust::grant(repo, &preview.identity, true).expect("trust");
    let db = gitpulse_lib::tasks::store_path(repo);
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    plant_task::plant(
        &dc_store::Store::open(&db).expect("task store"),
        "TASK-FENCE",
        "in_progress",
        r#"{"planned_files":[{"path":"src/planned.rs","allowed_change":"modify"}]}"#,
    );
    gitpulse_lib::ledger::bindings::bind(repo, repo, "TASK-FENCE").expect("bind");

    let bash = |command: &str| {
        serde_json::json!({
            "hook_event_name": "PreToolUse", "session_id": "fence", "cwd": repo,
            "tool_name": "Bash", "tool_input": { "command": command },
        })
    };
    let edit = |file: &str| {
        serde_json::json!({
            "hook_event_name": "PreToolUse", "session_id": "fence", "cwd": repo,
            "tool_name": "Edit",
            "tool_input": { "file_path": format!("{repo}/{file}"), "old_string": "a", "new_string": "b" },
        })
    };

    let reason_of = |answer: &Option<serde_json::Value>| {
        answer.as_ref().unwrap()["hookSpecificOutput"]["permissionDecisionReason"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    };

    // Outside the plan: every way of writing docs/other.md is refused as an
    // unplanned write, whether the harness reads it from a redirection or from
    // the arguments of the program that writes it.
    for command in [
        "echo x > docs/other.md",
        "sed -i.bak s/a/b/ docs/other.md",
        "printf x | tee docs/other.md",
        "cp seed.txt docs/other.md",
        "mv seed.txt docs/other.md",
        "rm docs/other.md",
        "truncate -s 0 docs/other.md",
        "touch docs/other.md",
        "dd if=seed.txt of=docs/other.md",
        "perl -pi -e 's/a/b/' docs/other.md",
        "install seed.txt docs/other.md",
        "ln -sf seed.txt docs/other.md",
        "rsync seed.txt docs/other.md",
        "patch docs/other.md < x.diff",
    ] {
        let answer = ask("command-gate", &bash(command), home.path(), logs.path());
        assert!(denied(&answer), "{command} was not refused: {answer:?}");
        let reason = reason_of(&answer);
        assert!(reason.contains("scope.unplanned"), "{command}: {reason}");
    }
    let answer = ask(
        "collision-guard",
        &edit("docs/other.md"),
        home.path(),
        logs.path(),
    );
    assert!(
        denied(&answer),
        "an Edit outside the plan was not refused: {answer:?}"
    );

    // A writer whose target is not in the command text cannot be judged against
    // a scope, so a bound scope refuses it rather than guessing.
    for command in [
        "git apply x.diff",
        "echo docs/other.md | xargs rm",
        "find docs -name other.md -exec rm {} +",
    ] {
        let answer = ask("command-gate", &bash(command), home.path(), logs.path());
        assert!(denied(&answer), "{command} was not refused: {answer:?}");
        let reason = reason_of(&answer);
        assert!(
            reason.contains("command.substitution"),
            "{command}: {reason}"
        );
        assert!(reason.contains("declared scope"), "{command}: {reason}");
    }

    // Inside the plan, nothing is refused.
    for command in [
        "echo x > src/planned.rs",
        "sed --in-place s/a/b/ src/planned.rs",
    ] {
        let answer = ask("command-gate", &bash(command), home.path(), logs.path());
        assert!(
            !denied(&answer),
            "{command} into the planned file was refused: {answer:?}"
        );
    }
    let answer = ask(
        "collision-guard",
        &edit("src/planned.rs"),
        home.path(),
        logs.path(),
    );
    assert!(
        !denied(&answer),
        "an Edit of the planned file was refused: {answer:?}"
    );
}

#[test]
#[ignore = "live contract against an installed manvi binary; run in CI with --ignored"]
fn sibling_worktree_and_outside_reads_are_judged_end_to_end() {
    let home = tempfile::tempdir().expect("home");
    let logs = tempfile::tempdir().expect("logs");
    let serial = hermetic_env::env_serial();
    let _home = hermetic_env::bind_env(&serial).set("HOME", home.path());

    let base_raw = tempfile::tempdir().expect("base");
    let base = base_raw.path().canonicalize().expect("canonical base");
    let main_dir = base.join("main");
    let wt_b_dir = base.join("wt-b");
    let plain_dir = base.join("plain");
    let outside_dir = base.join("outside");
    std::fs::create_dir_all(&main_dir).unwrap();
    std::fs::create_dir_all(&plain_dir).unwrap();
    std::fs::create_dir_all(&outside_dir).unwrap();

    git(&main_dir, &["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(main_dir.join("src")).unwrap();
    std::fs::create_dir_all(main_dir.join("sub")).unwrap();
    std::fs::write(main_dir.join("src/planned.rs"), "main\n").unwrap();
    std::fs::write(main_dir.join("src/sibling.rs"), "sib\n").unwrap();
    std::fs::write(main_dir.join("unplanned.txt"), "unplanned\n").unwrap();
    git(&main_dir, &["add", "."]);
    git(&main_dir, &["commit", "-q", "-m", "seed"]);
    git(
        &main_dir,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "feat",
            wt_b_dir.to_str().unwrap(),
        ],
    );

    let scratch = outside_dir.join("scratch.json");
    let outside_env = outside_dir.join(".env");
    std::fs::write(&scratch, "{\"ok\":true}\n").unwrap();
    std::fs::write(&outside_env, "SECRET=1\n").unwrap();

    let main_repo = main_dir.to_str().expect("utf8 main");
    let wt_b_repo = wt_b_dir.to_str().expect("utf8 wt_b");

    gitpulse_lib::engine::process::run_process_with_unlimited_spawn_rate();
    let main_preview = gitpulse_lib::repository_trust::inspect(main_repo).expect("inspect main");
    gitpulse_lib::repository_trust::grant(main_repo, &main_preview.identity, true)
        .expect("trust main");
    let wt_b_preview = gitpulse_lib::repository_trust::inspect(wt_b_repo).expect("inspect wt_b");
    gitpulse_lib::repository_trust::grant(wt_b_repo, &wt_b_preview.identity, true)
        .expect("trust wt_b");

    let db = gitpulse_lib::tasks::store_path(main_repo);
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    let store = dc_store::Store::open(&db).expect("task store");
    plant_task::plant(
        &store,
        "TASK-MAIN",
        "in_progress",
        r#"{"planned_files":[{"path":"src/planned.rs","allowed_change":"modify"}],"allowed_commands":["git *","jq *","cat *","head *","sed *","mytool *","ls *"]}"#,
    );
    plant_task::plant(
        &store,
        "TASK-WTB",
        "in_progress",
        r#"{"planned_files":[{"path":"src/sibling.rs","allowed_change":"modify"}],"allowed_commands":["git *"]}"#,
    );
    gitpulse_lib::ledger::bindings::bind(main_repo, main_repo, "TASK-MAIN").expect("bind main");
    gitpulse_lib::ledger::bindings::bind(main_repo, wt_b_repo, "TASK-WTB").expect("bind wt_b");

    let bash = |command: &str| {
        serde_json::json!({
            "hook_event_name": "PreToolUse", "session_id": "fence-wt", "cwd": main_repo,
            "tool_name": "Bash", "tool_input": { "command": command },
        })
    };
    let reason_of = |answer: &Option<serde_json::Value>| {
        answer.as_ref().unwrap()["hookSpecificOutput"]["permissionDecisionReason"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    };

    // AC1: git -C <registered sibling worktree> is judged against sibling scope.
    for allowed_cmd in [
        "git -C ../wt-b status",
        "git -C ../wt-b checkout -- src/sibling.rs",
    ] {
        let answer = ask("command-gate", &bash(allowed_cmd), home.path(), logs.path());
        assert!(
            !denied(&answer),
            "{allowed_cmd} was unexpectedly refused: {answer:?}"
        );
    }

    for (cmd, want_substrings) in [
        (
            "git -C ../wt-b checkout -- unplanned.txt",
            vec!["scope.unplanned", "TASK-WTB"],
        ),
        ("git -C ../wt-b checkout -- .env", vec!["path.secret"]),
        (
            "git -C ../wt-b status > ../wt-b/out.txt",
            vec!["path.outside_root"],
        ),
    ] {
        let answer = ask("command-gate", &bash(cmd), home.path(), logs.path());
        assert!(denied(&answer), "{cmd} was not refused: {answer:?}");
        let reason = reason_of(&answer);
        for sub in want_substrings {
            assert!(
                reason.contains(sub),
                "{cmd}: missing {sub:?} in reason: {reason}"
            );
        }
    }

    // AC2: read-only programs on outside paths are allowed, while outside secret reads refuse.
    for cmd in [
        format!("jq . {}", scratch.display()),
        format!("cat {}", scratch.display()),
        format!("head -n 1 {}", scratch.display()),
        format!("sed -n '1p' {}", scratch.display()),
    ] {
        let answer = ask("command-gate", &bash(&cmd), home.path(), logs.path());
        assert!(!denied(&answer), "read-only {cmd} was refused: {answer:?}");
    }
    let outside_env_cmd = format!("jq . {}", outside_env.display());
    let answer = ask(
        "command-gate",
        &bash(&outside_env_cmd),
        home.path(),
        logs.path(),
    );
    assert!(denied(&answer), "outside .env read was not refused");
    assert!(
        reason_of(&answer).contains("command.secret_read"),
        "missing command.secret_read in refusal reason"
    );

    // AC3: under blocking posture, refusal texts name the judged alternative with no "..".
    for (cmd, rule, hints) in [
        (
            "cd sub && ls".to_string(),
            "command.directory_change",
            vec!["git -c", "repository root"],
        ),
        (
            format!("git -C {} status", plain_dir.display()),
            "command.directory_change",
            vec!["registered", "repository root"],
        ),
        (
            "git -C ../plain status".to_string(),
            "command.directory_change",
            vec!["registered", "repository root"],
        ),
        (
            "cat <<EOF\nhi\nEOF".to_string(),
            "command.heredoc",
            vec!["file", "edit"],
        ),
        (
            format!("mytool {}", scratch.display()),
            "command.operand.unverifiable",
            vec!["cat", "jq", "sed -n"],
        ),
    ] {
        let answer = ask("command-gate", &bash(&cmd), home.path(), logs.path());
        assert!(denied(&answer), "{cmd} was not refused: {answer:?}");
        let reason = reason_of(&answer);
        assert!(reason.contains(rule), "{cmd}: missing {rule} in {reason}");
        let msg = reason.split("\n  target:").next().unwrap_or(&reason);
        assert!(
            !msg.contains(".."),
            "{cmd}: reason message must not contain '..': {reason}"
        );
        let lower = reason.to_lowercase();
        for hint in hints {
            assert!(
                lower.contains(&hint.to_lowercase()),
                "{cmd}: missing alternative {hint:?} in {reason}"
            );
        }
    }
}
