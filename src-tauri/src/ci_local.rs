//! CI:local — run the repository's own CI pipeline on this machine.
//!
//! Mirrors `.github/workflows/ci.yml` step-for-step for the ecosystems a
//! checkout actually has (package.json → frontend checks; Cargo.toml → Rust
//! checks), so "did CI pass?" can be answered before pushing instead of
//! after. Steps run sequentially and stop at the first failure, matching
//! GitHub Actions' default step semantics; everything that did not run is
//! reported as skipped rather than laundered into a pass.
//!
//! These are build/test commands, not git mutations, so they follow the deps
//! scanner's precedent (`analyzer::deps`) and are not routed through the
//! harness command gate — which fails closed on non-git commands and would
//! make the feature unusable with a harness installed. The protections that
//! matter here are the same ones every subprocess gets: hard per-step
//! timeouts, capped output, no shell interpolation, and honest status
//! accounting.

use crate::engine::git_cli::{capture_command, validate_repo, CapturedOutput};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Per-step hard cap. Generous by design (a cold `cargo clippy` on a large
/// tree can take minutes) but bounded so a wedged toolchain cannot pin the
/// blocking pool forever.
const STEP_TIMEOUT: Duration = Duration::from_secs(30 * 60);
/// Tail of combined output kept per step. Full logs belong to the terminal;
/// the report needs enough to name the failure.
const OUTPUT_TAIL_BYTES: usize = 8 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CiStepResult {
    pub name: String,
    /// The exact command line, rendered for display.
    pub command: String,
    /// `passed` | `failed` | `skipped`.
    pub status: String,
    /// Skip reason or failure detail; empty on a pass.
    pub detail: String,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CiLocalReport {
    pub steps: Vec<CiStepResult>,
    pub passed: usize,
    pub failed: usize,
    pub skipped: usize,
    pub total_duration_ms: u64,
}

#[derive(Debug, Clone)]
pub struct CiStep {
    pub name: &'static str,
    pub program: String,
    pub args: Vec<String>,
}

impl CiStep {
    fn rendered_command(&self) -> String {
        let mut line = self.program.clone();
        for arg in &self.args {
            if arg.contains(' ') || arg.is_empty() {
                line.push_str(&format!(" {:?}", arg));
            } else {
                line.push(' ');
                line.push_str(arg);
            }
        }
        line
    }
}

fn npm_program() -> &'static str {
    if cfg!(windows) {
        "npm.cmd"
    } else {
        "npm"
    }
}

/// Finds the workspace's Cargo manifest: repo root first, then the Tauri
/// backend directory — the same layout ci.yml assumes (`src-tauri/Cargo.toml`).
fn find_cargo_manifest(repo_root: &Path) -> Option<PathBuf> {
    [
        repo_root.join("Cargo.toml"),
        repo_root.join("src-tauri").join("Cargo.toml"),
    ]
    .into_iter()
    .find(|candidate| candidate.is_file())
}

/// Plans the steps this checkout would run, purely from its manifests.
/// No toolchain probing happens here: a missing binary surfaces as that
/// step's failure when it runs, exactly like CI on a broken runner image.
pub fn plan_ci_steps(repo_root: &Path) -> Vec<CiStep> {
    let mut steps = Vec::new();
    if repo_root.join("package.json").is_file() {
        steps.push(CiStep {
            name: "Frontend type-check",
            program: npm_program().to_string(),
            args: vec!["run".into(), "check".into()],
        });
        steps.push(CiStep {
            name: "Frontend unit tests",
            program: npm_program().to_string(),
            args: vec!["test".into()],
        });
        steps.push(CiStep {
            name: "Frontend build",
            program: npm_program().to_string(),
            args: vec!["run".into(), "build".into()],
        });
    }
    if let Some(manifest) = find_cargo_manifest(repo_root) {
        let manifest = manifest.to_string_lossy().to_string();
        steps.push(CiStep {
            name: "Rust format check",
            program: "cargo".into(),
            args: vec![
                "fmt".into(),
                "--manifest-path".into(),
                manifest.clone(),
                "--all".into(),
                "--".into(),
                "--check".into(),
            ],
        });
        steps.push(CiStep {
            name: "Rust lint (clippy)",
            program: "cargo".into(),
            args: vec![
                "clippy".into(),
                "--manifest-path".into(),
                manifest.clone(),
                "--all-targets".into(),
                "--".into(),
                "-D".into(),
                "warnings".into(),
            ],
        });
        steps.push(CiStep {
            name: "Rust tests",
            program: "cargo".into(),
            args: vec!["test".into(), "--manifest-path".into(), manifest],
        });
    }
    steps
}

/// Quieter, non-interactive npm output without disabling lifecycle scripts
/// (unlike the audit path, these runs may legitimately use them).
const NPM_CI_ENV: &[(&str, &str)] = &[
    ("npm_config_fund", "false"),
    ("npm_config_update_notifier", "false"),
    ("npm_config_progress", "false"),
    ("CI", "1"),
];

fn output_tail(stdout: &[u8], stderr: &[u8]) -> String {
    let mut combined = stdout.to_vec();
    combined.extend_from_slice(stderr);
    crate::engine::git_cli::byte_tail(&combined, OUTPUT_TAIL_BYTES)
        .lines()
        .collect::<Vec<_>>()
        .join("\n")
}

/// Runs the planned pipeline against `repo_path`. Every condition that stops
/// a step from running is recorded on that step — never folded into another
/// step's result and never reported as a pass.
/// What one step's command outcome means.
///
/// Split out of [`run_ci_local`], whose loop does the IO: every branch below
/// is a decision about what that IO meant, and none of them were reachable by
/// a test while they lived inside a function that shells out to npm and cargo.
///
/// Three outcomes, and the third is the one that matters. A step that could
/// not be spawned or that timed out never reached an exit code, so nothing is
/// known about the project under test. It is still reported failed — a run
/// that could not complete is not a pass, and the header must not go green —
/// but its detail says so in words, because "could not run" and "ran and
/// failed" are different facts that happen to share a colour.
fn step_result(
    step: &CiStep,
    outcome: Result<CapturedOutput, String>,
    duration_ms: u64,
) -> CiStepResult {
    let (status, detail) = match outcome {
        Ok(output) if output.success => ("passed", String::new()),
        Ok(output) => {
            // The detail is the capped output tail: enough to name the
            // failure without a terminal, never the whole log. When the
            // tool printed nothing, the exit status is the only fact.
            let tail = output_tail(&output.stdout, &output.stderr);
            let stderr = output.stderr_text();
            let summary = if stderr.is_empty() {
                format!("{} exited {}", step.program, output.status_code)
            } else {
                stderr.lines().next_back().unwrap_or(&stderr).to_string()
            };
            let detail = if tail.trim().is_empty() {
                summary
            } else {
                format!("{summary}\n\n--- output (tail) ---\n{tail}")
            };
            ("failed", detail)
        }
        Err(e) => ("failed", format!("{COULD_NOT_RUN}{e}")),
    };
    CiStepResult {
        name: step.name.to_string(),
        command: step.rendered_command(),
        status: status.into(),
        detail,
        duration_ms,
    }
}

/// Prefix marking a step that never reached an exit code, so the distinction
/// survives in one place rather than being spelled out at each site that
/// needs to recognise it.
const COULD_NOT_RUN: &str = "could not run: ";

/// The row recorded for every step after one has already failed.
fn skipped_result(step: &CiStep) -> CiStepResult {
    CiStepResult {
        name: step.name.to_string(),
        command: step.rendered_command(),
        status: "skipped".into(),
        detail: "skipped after an earlier step failed".into(),
        duration_ms: 0,
    }
}

pub fn run_ci_local(repo_path: &str) -> Result<CiLocalReport, String> {
    let repo = validate_repo(repo_path)?;
    let started = Instant::now();
    let plan = plan_ci_steps(&repo);
    if plan.is_empty() {
        return Err(
            "No supported CI manifests found (expected package.json and/or Cargo.toml)".into(),
        );
    }

    let mut results: Vec<CiStepResult> = Vec::with_capacity(plan.len());
    let mut failed_early = false;
    for step in plan {
        if failed_early {
            results.push(skipped_result(&step));
            continue;
        }
        let step_started = Instant::now();
        let arg_refs: Vec<&str> = step.args.iter().map(String::as_str).collect();
        let outcome = capture_command(
            &step.program,
            &arg_refs,
            Some(&repo),
            STEP_TIMEOUT,
            NPM_CI_ENV,
        );
        let duration_ms = u64::try_from(step_started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let result = step_result(&step, outcome, duration_ms);
        failed_early = result.status != "passed";
        results.push(result);
    }

    let passed = results.iter().filter(|r| r.status == "passed").count();
    let failed = results.iter().filter(|r| r.status == "failed").count();
    let skipped = results.iter().filter(|r| r.status == "skipped").count();
    Ok(CiLocalReport {
        passed,
        failed,
        skipped,
        steps: results,
        total_duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
    })
}

#[cfg(test)]
mod tests {
    /// A step that passed, failed, or never ran at all — the three outcomes
    /// [`step_result`] has to tell apart. None of these were reachable by a
    /// test until the decision was split out of the loop that shells out to
    /// npm and cargo, so `run_ci_local` was 0% covered and every branch here
    /// was taken on trust.
    fn step() -> CiStep {
        CiStep {
            name: "Frontend unit tests",
            program: "npm".into(),
            args: vec!["test".into()],
        }
    }

    fn captured(success: bool, code: i32, stdout: &str, stderr: &str) -> CapturedOutput {
        CapturedOutput {
            stdout: stdout.as_bytes().to_vec(),
            stderr: stderr.as_bytes().to_vec(),
            success,
            status_code: code,
        }
    }

    #[test]
    fn a_successful_step_carries_no_detail() {
        let r = step_result(&step(), Ok(captured(true, 0, "all good", "")), 120);
        assert_eq!(r.status, "passed");
        assert_eq!(r.detail, "");
        assert_eq!(r.duration_ms, 120);
        assert_eq!(r.command, step().rendered_command());
    }

    #[test]
    fn a_failed_step_reports_the_last_stderr_line_and_the_output_tail() {
        let r = step_result(
            &step(),
            Ok(captured(
                false,
                1,
                "test output here",
                "warning: noise\nError: 3 tests failed",
            )),
            50,
        );
        assert_eq!(r.status, "failed");
        // The last stderr line is the summary, not the first: tools print the
        // verdict last.
        assert!(
            r.detail.starts_with("Error: 3 tests failed"),
            "{}",
            r.detail
        );
        assert!(r.detail.contains("--- output (tail) ---"), "{}", r.detail);
        assert!(r.detail.contains("test output here"), "{}", r.detail);
    }

    #[test]
    fn a_silent_failure_falls_back_to_the_exit_status() {
        // Nothing on either stream: the status code is the only fact there is,
        // and an empty detail would leave the row unexplained.
        let r = step_result(&step(), Ok(captured(false, 137, "", "")), 10);
        assert_eq!(r.status, "failed");
        assert_eq!(r.detail, "npm exited 137");
    }

    /// The distinction the surrounding comment promises: a step that could not
    /// be spawned, or that timed out, never reached an exit code. Nothing is
    /// known about the project under test. It still counts as failed, because
    /// a run that could not complete is not a pass and the header must not go
    /// green — but it must not be indistinguishable from a real failure.
    #[test]
    fn a_step_that_never_ran_says_so_rather_than_blaming_the_project() {
        let r = step_result(&step(), Err("timed out after 600s".into()), 600_000);
        assert_eq!(r.status, "failed", "fail closed: this is not a pass");
        assert!(r.detail.starts_with(COULD_NOT_RUN), "{}", r.detail);
        assert!(r.detail.contains("timed out after 600s"), "{}", r.detail);

        // And it is distinguishable from a step that ran and failed, which is
        // the whole point: same colour, different fact.
        let ran_and_failed = step_result(&step(), Ok(captured(false, 1, "", "Error: boom")), 5);
        assert!(!ran_and_failed.detail.starts_with(COULD_NOT_RUN));
    }

    #[test]
    fn skipped_steps_name_why_they_were_skipped() {
        let r = skipped_result(&step());
        assert_eq!(r.status, "skipped");
        assert_eq!(r.duration_ms, 0, "a step that never started took no time");
        assert!(r.detail.contains("earlier step failed"), "{}", r.detail);
    }

    /// The complete set of statuses this module can produce.
    ///
    /// `run_ci_local` continues only while a step's status is "passed", so
    /// this is what that decision is made against. Pinning the set means a
    /// fourth status cannot be introduced without this failing and forcing
    /// the question of whether the run should carry on past it — and the
    /// frontend, which colours by these three and sums `failed` to decide
    /// whether the header goes red, cannot be handed a fourth silently.
    #[test]
    fn the_status_vocabulary_is_exactly_these_three() {
        let produced: Vec<String> = vec![
            step_result(&step(), Ok(captured(true, 0, "", "")), 1).status,
            step_result(&step(), Ok(captured(false, 1, "", "boom")), 1).status,
            step_result(&step(), Err("spawn failed".into()), 1).status,
            skipped_result(&step()).status,
        ];
        assert_eq!(produced, vec!["passed", "failed", "failed", "skipped"]);

        let distinct: std::collections::BTreeSet<&str> =
            produced.iter().map(String::as_str).collect();
        assert_eq!(
            distinct,
            ["failed", "passed", "skipped"].into_iter().collect(),
            "a new status needs a counter in CiLocalReport and a colour in the panel"
        );
    }

    use super::*;

    #[test]
    fn planner_matches_manifests_to_ecosystem_steps() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        // Nothing present → nothing planned.
        assert!(plan_ci_steps(root).is_empty());

        std::fs::write(root.join("package.json"), "{}").unwrap();
        let steps = plan_ci_steps(root);
        assert_eq!(steps.len(), 3);
        assert_eq!(steps[0].name, "Frontend type-check");
        assert_eq!(steps[2].program, npm_program());

        std::fs::create_dir_all(root.join("src-tauri")).unwrap();
        std::fs::write(root.join("src-tauri").join("Cargo.toml"), "[package]").unwrap();
        let steps = plan_ci_steps(root);
        assert_eq!(steps.len(), 6);
        assert_eq!(steps[3].name, "Rust format check");
        // The manifest path is threaded through every cargo step.
        assert!(
            steps[5]
                .args
                .windows(2)
                .any(|w| w[0] == "--manifest-path" && w[1].ends_with("src-tauri/Cargo.toml")),
            "cargo test step must carry --manifest-path, got {:?}",
            steps[5].args
        );

        // A root-level Cargo.toml wins over src-tauri's.
        std::fs::write(root.join("Cargo.toml"), "[package]").unwrap();
        let steps = plan_ci_steps(root);
        assert!(steps[4].args.windows(2).any(|w| w[0] == "--manifest-path"
            && w[1].ends_with("/Cargo.toml")
            && !w[1].contains("src-tauri")));
    }

    #[test]
    fn rendered_command_quotes_arguments_with_spaces() {
        let step = CiStep {
            name: "x",
            program: "cargo".into(),
            args: vec![
                "fmt".into(),
                "--manifest-path".into(),
                "/repo with space/Cargo.toml".into(),
            ],
        };
        assert_eq!(
            step.rendered_command(),
            "cargo fmt --manifest-path \"/repo with space/Cargo.toml\""
        );
    }

    #[test]
    fn output_tail_keeps_the_end_and_stays_char_safe() {
        let long = "a".repeat(10_000);
        let tail = output_tail(long.as_bytes(), b"ERROR: boom");
        assert!(tail.ends_with("ERROR: boom"));
        assert!(tail.len() <= OUTPUT_TAIL_BYTES + 16);

        // A multibyte sequence split at the cap must not leak replacement
        // characters at the head of the tail.
        let prefix = "é".repeat(5000); // 2 bytes each, 10000 bytes total
        let split_tail = output_tail(prefix.as_bytes(), b"");
        assert!(!split_tail.starts_with('\u{FFFD}'));
    }

    #[test]
    fn empty_plan_is_an_error_not_an_empty_pass() {
        let dir = tempfile::TempDir::new().unwrap();
        // A real repository with neither manifest plans zero steps; that must
        // refuse rather than return a vacuous all-green report.
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(dir.path())
            .status()
            .expect("git available in test environment");
        let err = run_ci_local(dir.path().to_str().unwrap()).unwrap_err();
        assert!(
            err.contains("No supported CI manifests"),
            "empty plan should name the reason, got: {err}"
        );
    }
}
