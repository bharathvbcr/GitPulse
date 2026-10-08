//! The agent hook's task-scope fence, driven end to end: the real
//! `gitpulse-hook` executable, a real `manvi serve`, and a repository bound to
//! a task that plans one file.
//!
//! The unit tests in `src/hooks/mod.rs` prove the hook sends the scope and
//! renders what comes back, against a scripted sidecar. What they cannot prove
//! is the half that lives in the harness: which files a command line is read
//! as writing. `sed -i`, `tee`, `cp` and `mv` write through their arguments,
//! and until DevCouncil `fix/argument-write-targets` the harness read only
//! redirections — so each came back a demoted allow while `echo x > f` into the
//! same file was refused. Only a real harness can show that gap closed.
//!
//! It is `#[ignore]` because it needs that harness: run it with
//! `GITPULSE_MANVI_BIN` naming the candidate binary, e.g.
//!
//! ```text
//! GITPULSE_MANVI_BIN=/path/to/manvi cargo test --test hook_scope_fence -- --ignored
//! ```
//!
//! One test per file, because it sets `HOME` for the whole process: trust
//! records live under it, and a fixture must not write the developer's own.

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
#[ignore = "requires GITPULSE_MANVI_BIN: a manvi built with DevCouncil fix/argument-write-targets"]
fn a_task_bound_session_cannot_write_outside_its_plan_through_any_tool() {
    let home = tempfile::tempdir().expect("home");
    let logs = tempfile::tempdir().expect("logs");
    // SAFETY: the only test in this binary, set before anything reads it.
    unsafe { std::env::set_var("HOME", home.path()) };

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

    let preview = gitpulse_lib::repository_trust::inspect(repo).expect("inspect");
    gitpulse_lib::repository_trust::grant(repo, &preview.identity, true).expect("trust");
    let db = gitpulse_lib::tasks::store_path(repo);
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    dc_store::Store::open(&db)
        .expect("task store")
        .connection()
        .execute(
            "INSERT INTO tasks (id, title, description, planned_files_json, status)
             VALUES ('TASK-FENCE', 'Fence', '',
                 '[{\"path\":\"src/planned.rs\",\"allowed_change\":\"modify\"}]', 'in_progress')",
            [],
        )
        .expect("task row");
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

    // Outside the plan: every way of writing docs/other.md is refused, the
    // redirect that always was and the four that were demoted allows.
    for command in [
        "echo x > docs/other.md",
        "sed -i.bak s/a/b/ docs/other.md",
        "printf x | tee docs/other.md",
        "cp seed.txt docs/other.md",
        "mv seed.txt docs/other.md",
    ] {
        let answer = ask("command-gate", &bash(command), home.path(), logs.path());
        assert!(denied(&answer), "{command} was not refused: {answer:?}");
        let reason = answer.unwrap()["hookSpecificOutput"]["permissionDecisionReason"]
            .as_str()
            .unwrap_or_default()
            .to_string();
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
