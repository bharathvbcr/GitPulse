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
//! ref CI can fetch. Run it by hand when the harness changes.
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
