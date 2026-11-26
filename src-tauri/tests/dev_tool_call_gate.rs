//! The agent hook's Bash gate, asked about the shell fallbacks agents reach for
//! when MCP is unavailable: `dev tool call …` (DevCouncil) and
//! `gitpulse-mcp call …` (GitPulse's own shell route). ft-a4bb9f73, criterion 1.
//!
//! "Tested and recorded", not a policy change: every row pins what the real
//! `gitpulse-hook command-gate` executable answers today, driven with the same
//! PreToolUse JSON Claude Code sends.
//!
//! # What the gate does with these lines
//!
//! GitPulse does not tokenise the command line. `hooks::judge_command` hands the
//! raw string to the Manvi harness (`check_command_in_scope`), and quoting, pipes and
//! `--project-root` are Manvi's to read. So the decision pinned here is the
//! decision of the *hook with no harness*: `GITPULSE_MANVI_BIN` names a file that
//! does not exist, so the result does not depend on whether this machine has
//! Manvi. With no harness every line gets the same answer: no `permissionDecision`
//! (the host's own permission rules run) and a `systemMessage` saying the command
//! ran UNGATED. That is "not judged", never an approval.
//!
//! The Manvi case (allow with no note for the read-only calls; a refusal with
//! `path.outside_root` for a write whose `--project-root` is outside the repository)
//! is what the card observed on 2026-11-18 in an interactive session. It is
//! NOT pinned by the default run. The `#[ignore]`d test at the bottom asks it of a
//! real harness (run it with `GITPULSE_MANVI_BIN=<manvi>`); on the machine this
//! was written on it did not reproduce the refusal: see that test.

mod common;

use common::env as hermetic_env;
use gitpulse_lib::procguard::LockedSpawn;
use serde_json::Value;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const HOOK: &str = env!("CARGO_BIN_EXE_gitpulse-hook");

/// A throwaway home, a real git repository, and a directory outside it.
struct World {
    _root: tempfile::TempDir,
    home: PathBuf,
    logs: PathBuf,
    repo: PathBuf,
    outside: PathBuf,
}

impl World {
    fn new() -> Self {
        // `/tmp` keeps the path short on Unix; Windows has no such directory.
        let parent = if cfg!(windows) {
            std::env::temp_dir()
        } else {
            PathBuf::from("/tmp")
        };
        let root = tempfile::Builder::new()
            .prefix("dtc")
            .tempdir_in(parent)
            .expect("root");
        let base = root.path().canonicalize().expect("canonical root");
        let (home, logs, repo, outside) = (
            base.join("home"),
            base.join("logs"),
            base.join("repo"),
            base.join("outside"),
        );
        for dir in [&home, &home.join("tmp"), &logs, &repo, &outside] {
            std::fs::create_dir_all(dir).expect("mkdir");
        }
        std::fs::create_dir_all(repo.join("sub")).expect("mkdir sub");
        let status = Command::new("git")
            .args(["init", "-q", "-b", "main"])
            .current_dir(&repo)
            .status_locked()
            .expect("git init");
        assert!(status.success(), "git init failed");
        Self {
            _root: root,
            home,
            logs,
            repo,
            outside,
        }
    }

    /// One PreToolUse Bash call from the repository root. Returns the hook's
    /// exit code and its parsed stdout (None when it printed nothing).
    fn ask(&self, command: &str) -> (Option<i32>, Option<Value>) {
        self.ask_with(command, &self.home.join("no-such-manvi"))
    }

    /// Each call is its own session and its own tool call, with a private
    /// TMPDIR. The hook says a given advisory once per (session, checkout) and
    /// drops an exact repeat for the same `tool_use_id` (`hooks::advisory_once`,
    /// 15 minutes, in the shared temp directory), so reused ids or a shared
    /// TMPDIR would make a later run silent for a reason that has nothing to do
    /// with the command: the first version of this test did exactly that.
    fn ask_with(&self, command: &str, manvi: &Path) -> (Option<i32>, Option<Value>) {
        use std::sync::atomic::{AtomicU64, Ordering};
        static CALLS: AtomicU64 = AtomicU64::new(0);
        let n = CALLS.fetch_add(1, Ordering::Relaxed);
        let payload = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "session_id": format!("dev-tool-call-gate-{}-{n}", std::process::id()),
            "tool_use_id": format!("toolu_dtc_{}_{n}", std::process::id()),
            "cwd": self.repo,
            "tool_name": "Bash",
            "tool_input": { "command": command },
            "permission_mode": "default",
        })
        .to_string();
        let mut child = Command::new(HOOK)
            .arg("command-gate")
            .env(gitpulse_lib::logging::LOG_DIR_ENV, &self.logs)
            .env("HOME", &self.home)
            .env("TMPDIR", self.home.join("tmp"))
            .env("XDG_CONFIG_HOME", self.home.join(".config"))
            .env("GITPULSE_TOOL_CONFIG", self.home.join("tools.json"))
            .env("GITPULSE_MANVI_BIN", manvi)
            .env_remove("GITPULSE_LEASE_BIN")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn_locked()
            .expect("spawn gitpulse-hook");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(payload.as_bytes())
            .expect("write payload");
        let output = child.wait_with_output().expect("hook output");
        let text = String::from_utf8(output.stdout).expect("utf8 stdout");
        let parsed = (!text.trim().is_empty())
            .then(|| serde_json::from_str(text.trim()).unwrap_or_else(|e| panic!("{e}: {text:?}")));
        (output.status.code(), parsed)
    }
}

/// The commands under test, as an agent types them. JSON sits inside single
/// quotes, exactly as in the card.
fn commands(world: &World) -> Vec<(&'static str, String)> {
    let inside = world.repo.join("sub");
    vec![
        (
            "dev_next_task",
            "dev tool call devcouncil_next_task".to_string(),
        ),
        (
            "dev_get_gaps_quoted_json",
            r#"dev tool call devcouncil_get_gaps --args '{"task_id":"T-1"}'"#.to_string(),
        ),
        (
            "dev_checkout_project_root_inside",
            format!(
                r#"dev tool call devcouncil_checkout_task --args '{{"task_id":"T-1","client_id":"probe"}}' --project-root {}"#,
                inside.display()
            ),
        ),
        (
            "dev_checkout_project_root_outside",
            format!(
                r#"dev tool call devcouncil_checkout_task --args '{{"task_id":"T-1","client_id":"probe"}}' --project-root {}"#,
                world.outside.display()
            ),
        ),
        (
            "dev_args_from_stdin_pipe",
            "echo '{}' | dev tool call x --args -".to_string(),
        ),
        (
            "gitpulse_mcp_insights_read",
            format!(
                "gitpulse-mcp call gitpulse_insights repo_path={}",
                world.repo.display()
            ),
        ),
        (
            "gitpulse_mcp_add_task_write",
            format!(
                "gitpulse-mcp call gitpulse_add_task --write repo_path={} title=probe",
                world.repo.display()
            ),
        ),
    ]
}
fn decision(out: &Option<Value>) -> Option<String> {
    out.as_ref()?["hookSpecificOutput"]["permissionDecision"]
        .as_str()
        .map(str::to_string)
}

fn message(out: &Option<Value>) -> String {
    out.as_ref()
        .and_then(|v| v["systemMessage"].as_str())
        .unwrap_or_default()
        .to_string()
}

/// With no harness on the machine, every one of these lines is answered the
/// same way: exit 0, no permission decision (so the host's own rules run), and
/// a note that the command ran UNGATED. Nothing here is allowed, asked or
/// denied by GitPulse, and the note is never absent: a line that was not
/// judged does not look like one that passed.
#[test]
fn with_no_harness_every_dev_tool_call_and_gitpulse_mcp_line_is_unjudged_and_says_so() {
    let world = World::new();
    let serial = hermetic_env::env_serial();
    let _home = hermetic_env::bind_env(&serial).set("HOME", &world.home);
    let rows = commands(&world);
    assert_eq!(rows.len(), 7, "the command table changed");
    for (name, command) in rows {
        let (code, out) = world.ask(&command);
        assert_eq!(code, Some(0), "{name}: a hook always exits 0");
        assert_eq!(decision(&out), None, "{name}: {command}: {out:?}");
        let note = message(&out);
        assert!(
            note.contains("No MANVI harness is installed") && note.contains("It ran UNGATED."),
            "{name}: {command}: {out:?}"
        );
    }
}

/// The card's observation, asked of a real harness. Ignored: it needs a
/// `manvi` binary (`GITPULSE_MANVI_BIN`), and its answers are the harness's,
/// not GitPulse's. Pinned against the Manvi installed on the machine this was
/// written on (version stamp unknown), so a different Manvi can move it.
///
/// Findings it records:
/// - In a repository GitPulse trusts, every line is admitted silently: no
///   decision, no note. That includes the `--write` `gitpulse-mcp` call, and the
///   `--project-root` *inside* the repository.
/// - In an untrusted repository the same lines carry only the "judged unscoped"
///   note: no decision.
/// - `--project-root` *outside* the repository is NOT refused with
///   `path.outside_root` as the card says. Manvi answers
///   `command.operand.unverifiable` ("handed a path outside the repository … cannot
///   tell a read from a write"), which the hook turns into a note and no
///   decision, because an unbound checkout's unreadable construct is left to the
///   host's permission rules (`hooks::gate_decision`). The card's refusal was
///   seen in an interactive session, so a bound checkout or another Manvi build
///   may produce it; neither is reproduced here.
#[test]
#[ignore = "needs GITPULSE_MANVI_BIN naming a manvi build; answers are the harness's"]
fn with_manvi_dev_tool_call_is_admitted_silently_except_an_outside_project_root() {
    let world = World::new();
    let manvi = PathBuf::from(
        std::env::var("GITPULSE_MANVI_BIN").expect("GITPULSE_MANVI_BIN names the harness"),
    );
    let serial = hermetic_env::env_serial();
    let _home = hermetic_env::bind_env(&serial).set("HOME", &world.home);
    for trusted in [false, true] {
        if trusted {
            gitpulse_lib::engine::process::run_process_with_unlimited_spawn_rate();
            let repo = world.repo.to_str().expect("utf8 repo");
            let preview = gitpulse_lib::repository_trust::inspect(repo).expect("inspect");
            gitpulse_lib::repository_trust::grant(repo, &preview.identity, true).expect("trust");
        }
        for (name, command) in commands(&world) {
            let (code, out) = world.ask_with(&command, &manvi);
            assert_eq!(code, Some(0), "{name}");
            assert_eq!(decision(&out), None, "{name} (trusted={trusted}): {out:?}");
            let note = message(&out);
            if name == "dev_checkout_project_root_outside" {
                assert!(
                    note.contains("command.operand.unverifiable")
                        && note.contains("outside the repository"),
                    "{name} (trusted={trusted}): {note}"
                );
            } else if trusted {
                assert!(out.is_none(), "{name}: a trusted repo is silent: {out:?}");
            } else {
                assert!(
                    note.contains("repository not trusted") && !note.contains("UNGATED"),
                    "{name}: {note}"
                );
            }
        }
    }
}
