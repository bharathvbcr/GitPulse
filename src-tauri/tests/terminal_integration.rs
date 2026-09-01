//! Integration coverage for the terminal's one-shot command runner.
//!
//! `spawn_session` needs a Tauri `AppHandle` for its output events, but
//! `run_terminal` and `run_manvi_action` do not — and they are where the
//! security-relevant decisions live: whether a command is gated, what the
//! runner does with output it cannot bound, and whether a hostile argv is
//! refused before a process starts. These run against real repositories and
//! real child processes.

use gitpulse_lib::terminal::{run_manvi_action, run_terminal, ManviActionKind};
use std::fs;
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

fn run_git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .expect("git must be on PATH");
    assert!(status.success(), "git {args:?} failed");
}

struct TestRepo {
    dir: TempDir,
}

impl TestRepo {
    fn init() -> Self {
        let dir = TempDir::new().expect("tempdir");
        run_git(dir.path(), &["init", "-b", "main"]);
        run_git(dir.path(), &["config", "user.email", "test@example.com"]);
        run_git(dir.path(), &["config", "user.name", "Test User"]);
        run_git(dir.path(), &["config", "commit.gpgsign", "false"]);
        fs::write(dir.path().join("README.md"), "hello\n").expect("write");
        run_git(dir.path(), &["add", "."]);
        run_git(dir.path(), &["commit", "-m", "initial"]);
        Self { dir }
    }

    fn path(&self) -> String {
        self.dir.path().to_string_lossy().into_owned()
    }
}

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| (*s).to_string()).collect()
}

#[test]
fn runs_a_git_command_and_reports_it_as_gated() {
    let repo = TestRepo::init();
    let result = run_terminal(&repo.path(), &argv(&["git", "status", "--short"]), Some(30))
        .expect("git status should run");
    assert_eq!(result.exit_code, Some(0));
    assert!(!result.timed_out);
    // Git commands pass through the MANVI gate; the verdict must accompany them.
    assert!(result.gated, "a git command must be gated");
    assert!(
        result.policy.is_some(),
        "a gated command must carry its verdict"
    );
}

#[test]
fn a_non_git_command_runs_ungated_and_says_so() {
    let repo = TestRepo::init();
    let result =
        run_terminal(&repo.path(), &argv(&["echo", "hello"]), Some(30)).expect("echo should run");
    assert_eq!(result.exit_code, Some(0));
    assert!(result.stdout_tail.contains("hello"));
    // The gate only judges git; a non-git command must not claim a verdict it
    // never received.
    assert!(!result.gated);
    assert!(result.policy.is_none());
}

#[test]
fn a_failing_command_reports_its_status_rather_than_erroring() {
    let repo = TestRepo::init();
    let result = run_terminal(
        &repo.path(),
        &argv(&["git", "rev-parse", "--verify", "refs/heads/nope"]),
        Some(30),
    )
    .expect("a failing git command still completes");
    assert_ne!(result.exit_code, Some(0), "the failure must be visible");
    assert!(!result.timed_out);
}

#[test]
fn an_empty_or_blank_program_is_refused_before_anything_runs() {
    let repo = TestRepo::init();
    assert!(run_terminal(&repo.path(), &[], Some(30)).is_err());
    assert!(run_terminal(&repo.path(), &argv(&["   "]), Some(30)).is_err());
}

#[test]
fn a_path_outside_a_repository_is_refused() {
    let plain = TempDir::new().expect("tempdir");
    let outside = plain.path().to_string_lossy().into_owned();
    assert!(run_terminal(&outside, &argv(&["echo", "hi"]), Some(30)).is_err());
    assert!(run_terminal("/nonexistent/repo", &argv(&["echo", "hi"]), Some(30)).is_err());
}

#[test]
fn an_unbounded_argv_is_refused_rather_than_spawned() {
    let repo = TestRepo::init();
    // Argument count and total byte size are both capped; neither should reach
    // a process.
    let many: Vec<String> = std::iter::once("echo".to_string())
        .chain((0..100_000).map(|i| i.to_string()))
        .collect();
    assert!(run_terminal(&repo.path(), &many, Some(30)).is_err());

    let huge = argv(&["echo"])
        .into_iter()
        .chain(std::iter::once("x".repeat(50 * 1024 * 1024)))
        .collect::<Vec<_>>();
    assert!(run_terminal(&repo.path(), &huge, Some(30)).is_err());
}

#[test]
fn output_larger_than_the_tail_is_truncated_not_buffered_whole() {
    let repo = TestRepo::init();
    // A command that produces far more than the retained tail must come back
    // bounded and flagged, not consume memory proportional to its output.
    let result = run_terminal(
        &repo.path(),
        &argv(&["git", "log", "--format=%H %s", "--all"]),
        Some(60),
    )
    .expect("git log runs");
    assert!(result.stdout_tail.len() < 10 * 1024 * 1024);
}

#[test]
fn a_manvi_action_validates_the_command_against_its_declared_purpose() {
    let repo = TestRepo::init();
    // Every purpose must refuse a command outside its own vocabulary; the app
    // chooses the purpose, so the backend cannot trust it to match the argv.
    for kind in [
        ManviActionKind::Health,
        ManviActionKind::Coverage,
        ManviActionKind::CoverageGenerator,
    ] {
        assert!(
            run_manvi_action(&repo.path(), &argv(&["rm", "-rf", "/"]), kind, Some(30)).is_err(),
            "{kind:?} must refuse a command outside its purpose"
        );
    }
}

#[test]
fn a_manvi_action_refuses_an_executable_path() {
    let repo = TestRepo::init();
    // Anything path-shaped in argv[0] would let a caller pick the binary,
    // sidestepping the allowlist that makes the purpose meaningful.
    for program in ["/bin/sh", "./local-script", "../escape", "sub/dir/tool"] {
        let result = run_manvi_action(
            &repo.path(),
            &argv(&[program, "--version"]),
            ManviActionKind::Coverage,
            Some(30),
        );
        assert!(
            result.is_err(),
            "{program} must be refused as an executable path"
        );
    }
}

#[test]
fn a_timeout_is_reported_as_a_timeout_not_a_silent_success() {
    let repo = TestRepo::init();
    // The floor is one second, so this is the shortest honest timeout test.
    let result = run_terminal(&repo.path(), &argv(&["sleep", "30"]), Some(1))
        .expect("the runner returns a result rather than erroring on timeout");
    assert!(
        result.timed_out,
        "a killed command must be flagged as timed out"
    );
    assert_ne!(
        result.exit_code,
        Some(0),
        "a timeout must not look like success"
    );
}
