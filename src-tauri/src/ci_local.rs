//! CI:local — run the repository's own CI pipeline on this machine.
//!
//! Mirrors `.github/workflows/ci.yml` step-for-step for the ecosystems a
//! checkout actually has (package.json → frontend checks; Cargo.toml → Rust
//! checks), so "did CI pass?" can be answered before pushing instead of
//! after. Steps run sequentially and stop at the first failure, matching
//! GitHub Actions' default step semantics; everything that did not run is
//! reported as skipped rather than laundered into a pass.
//!
//! Every step is judged by the harness command gate before it runs, and
//! reaches the ledger with its verdict. The gate fails closed on non-git
//! commands, so each step declares its own command line as the allowlist: the
//! harness can then answer with a clean allow instead of demoting
//! `command.not_allowed`, while the hard rungs still refuse a step that would
//! force-push or touch a credential path. On top of that every subprocess gets
//! hard per-step timeouts, capped output, no shell interpolation, and honest
//! status accounting.
//!
//! A completed run is recorded as a git-native verification note on HEAD — but
//! only when the working tree is clean. A dirty tree means the run exercised
//! HEAD *plus* uncommitted changes, and a note saying HEAD passed would be a
//! claim about a tree that was never tested.

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
    /// The commit this run was recorded against, empty when it was not
    /// recorded.
    ///
    /// A run that produced a verification note is a durable, git-native claim
    /// that survives a re-clone; a run that did not is a number on a screen.
    /// The two must be distinguishable, which is why this is a sha and not a
    /// boolean.
    #[serde(default)]
    pub recorded_commit: String,
    /// Empty when the run was recorded; otherwise why it was not.
    ///
    /// Separate from `recorded_commit` so "recorded nothing because the tree
    /// was dirty" never reads the same as "recorded nothing because writing
    /// the note failed".
    #[serde(default)]
    pub not_recorded_reason: String,
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

    let results = run_plan(plan, |step| {
        let step_started = Instant::now();
        let arg_refs: Vec<&str> = step.args.iter().map(String::as_str).collect();

        // Every step is judged before it runs.
        //
        // This runner used to bypass the gate entirely: `npm test`, `cargo
        // clippy` and everything else it spawned ran unjudged and unrecorded.
        // In a product whose claim is to be the trust boundary between agents
        // and the repository, a privileged unlogged execution path is the one
        // thing that cannot exist — and this was one, by design, because the
        // command gate fails closed on non-git commands.
        //
        // The answer is to declare the step rather than to skip the check. The
        // step's own command line is the allowlist, so the harness can answer
        // with a clean allow instead of demoting `command.not_allowed`; the
        // hard rungs still run, so a step that force-pushed or wrote a
        // credential path is still refused. Either way the step reaches the
        // ledger with its verdict.
        let mut argv = Vec::with_capacity(arg_refs.len() + 1);
        argv.push(step.program.as_str());
        argv.extend(arg_refs.iter().copied());
        let allowed = vec![step.rendered_command()];
        let repo_str = repo.to_string_lossy();
        if let Err(refusal) = crate::harness::guard_command_allowing(&repo_str, &argv, &allowed) {
            let duration_ms = u64::try_from(step_started.elapsed().as_millis()).unwrap_or(u64::MAX);
            // A refusal is not a step that failed: it is a step that never ran.
            // `COULD_NOT_RUN` is the same marker a spawn failure carries, so a
            // reader is never told a check passed when it did not happen.
            return (Err(refusal), duration_ms);
        }

        let outcome = capture_command(
            &step.program,
            &arg_refs,
            Some(&repo),
            STEP_TIMEOUT,
            NPM_CI_ENV,
        );
        let duration_ms = u64::try_from(step_started.elapsed().as_millis()).unwrap_or(u64::MAX);
        (outcome, duration_ms)
    });

    let mut report = summarize(
        results,
        u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
    );
    let (recorded_commit, not_recorded_reason) = record_verification(&repo, &report);
    report.recorded_commit = recorded_commit;
    report.not_recorded_reason = not_recorded_reason;
    Ok(report)
}

/// Walk the plan, stopping at the first step that does not pass.
///
/// The runner is injected so the sequencing can be tested without spawning
/// anything: what a user sees after a failure — every later step marked
/// "skipped" rather than silently missing, or worse, still attempted — is
/// behaviour, and it had no test while it lived inside the loop that shells
/// out to npm and cargo.
fn run_plan(
    plan: Vec<CiStep>,
    mut run: impl FnMut(&CiStep) -> (Result<CapturedOutput, String>, u64),
) -> Vec<CiStepResult> {
    let mut results: Vec<CiStepResult> = Vec::with_capacity(plan.len());
    let mut failed_early = false;
    for step in plan {
        if failed_early {
            results.push(skipped_result(&step));
            continue;
        }
        let (outcome, duration_ms) = run(&step);
        let result = step_result(&step, outcome, duration_ms);
        failed_early = result.status != "passed";
        results.push(result);
    }
    results
}

/// Tally the run.
///
/// The frontend colours its header red on `failed > 0` alone, so miscounting
/// a skipped step as failed — or a failed one as skipped — is the difference
/// between a run that reads as broken and one that reads as fine.
fn summarize(results: Vec<CiStepResult>, total_duration_ms: u64) -> CiLocalReport {
    CiLocalReport {
        passed: results.iter().filter(|r| r.status == "passed").count(),
        failed: results.iter().filter(|r| r.status == "failed").count(),
        skipped: results.iter().filter(|r| r.status == "skipped").count(),
        steps: results,
        total_duration_ms,
        recorded_commit: String::new(),
        not_recorded_reason: String::new(),
    }
}

/// The verdict a completed run records.
///
/// Derived from the steps themselves, not from the counts. A future change to
/// `run_plan` that introduced a fourth status would silently make a count-based
/// verdict wrong in the direction that matters — everything not-failed reading
/// as a pass — so "passed" here means every single step passed, and nothing
/// else does.
fn run_verdict(report: &CiLocalReport) -> &'static str {
    if report.steps.iter().all(|s| s.status == "passed") {
        "passed"
    } else {
        "failed"
    }
}

/// Whether the working tree has changes git can see.
///
/// `Err` when the question could not be answered, which is deliberately not
/// the same as `Ok(false)`: a note must never be written on the strength of a
/// cleanliness check that did not run.
fn working_tree_is_dirty(repo: &Path) -> Result<bool, String> {
    let out = std::process::Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=no"])
        .current_dir(repo)
        .output()
        .map_err(|e| format!("could not run git status: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git status failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(!String::from_utf8_lossy(&out.stdout).trim().is_empty())
}

/// Records a completed run as a verification note on HEAD.
///
/// Returns the commit the note landed on, or the reason no note was written.
/// Exactly one of the two is non-empty.
///
/// # Why a dirty tree is refused
///
/// A note attaches to a commit and says that commit's tree passed. A run
/// against a dirty checkout exercised HEAD *plus* whatever is uncommitted, so
/// the note would be a claim about a tree nothing ever tested — and it would
/// survive into every clone of the repository, long after the uncommitted
/// changes that produced it were gone. Untracked files are excluded from the
/// check: they are not part of any tree, so they cannot make HEAD's tree a lie.
fn record_verification(repo: &Path, report: &CiLocalReport) -> (String, String) {
    let repo_str = repo.to_string_lossy().to_string();

    match working_tree_is_dirty(repo) {
        Err(e) => return (String::new(), e),
        Ok(true) => {
            return (
                String::new(),
                "not recorded: the working tree has uncommitted changes, so this run \
                 did not test HEAD's tree"
                    .to_string(),
            )
        }
        Ok(false) => {}
    }

    let head = match std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(repo)
        .output()
    {
        Err(e) => return (String::new(), format!("could not resolve HEAD: {e}")),
        Ok(out) if !out.status.success() => {
            return (
                String::new(),
                format!(
                    "could not resolve HEAD: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                ),
            )
        }
        Ok(out) => String::from_utf8_lossy(&out.stdout).trim().to_string(),
    };

    let note = crate::engine::provenance::VerificationNote {
        verdict: run_verdict(report).to_string(),
        verified_at: i64::try_from(crate::ledger::ids::now_millis() / 1000).unwrap_or(i64::MAX),
        checked_by: "ci.local".to_string(),
        task_id: None,
        // The step *names* and their statuses, never their output: a failure
        // detail is a captured log tail, and git notes are pushed to remotes.
        details: Some(
            report
                .steps
                .iter()
                .map(|s| format!("{}={}", s.name, s.status))
                .collect::<Vec<_>>()
                .join(", "),
        ),
    };

    match crate::engine::provenance::write_verification_note(&repo_str, &head, &note) {
        Ok(()) => (head, String::new()),
        Err(e) => (String::new(), format!("could not write the note: {e}")),
    }
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
    fn plan_of(names: &[&'static str]) -> Vec<CiStep> {
        names
            .iter()
            .map(|name| CiStep {
                name,
                program: "npm".into(),
                args: vec!["test".into()],
            })
            .collect()
    }

    /// After one step fails, every later step must be recorded as skipped —
    /// not attempted, and not quietly dropped from the report. The injected
    /// runner counts its own calls, so "not attempted" is asserted rather
    /// than assumed from the output.
    #[test]
    fn a_failure_skips_every_later_step_without_running_it() {
        let mut attempted: Vec<&str> = Vec::new();
        let results = run_plan(plan_of(&["one", "two", "three"]), |step| {
            attempted.push(step.name);
            let outcome = if step.name == "two" {
                Ok(captured(false, 1, "", "Error: boom"))
            } else {
                Ok(captured(true, 0, "", ""))
            };
            (outcome, 7)
        });

        assert_eq!(
            attempted,
            vec!["one", "two"],
            "the third step must never be spawned"
        );
        let statuses: Vec<&str> = results.iter().map(|r| r.status.as_str()).collect();
        assert_eq!(statuses, vec!["passed", "failed", "skipped"]);
        assert_eq!(results.len(), 3, "a skipped step still gets a row");
        assert_eq!(results[2].duration_ms, 0);
    }

    /// A step that could not be spawned stops the run for the same reason a
    /// failing one does: nothing after it can be trusted to mean anything.
    #[test]
    fn a_step_that_could_not_run_also_stops_the_run() {
        let results = run_plan(plan_of(&["one", "two"]), |step| {
            let outcome = if step.name == "one" {
                Err("no such file or directory".to_string())
            } else {
                Ok(captured(true, 0, "", ""))
            };
            (outcome, 3)
        });
        assert!(results[0].detail.starts_with(COULD_NOT_RUN));
        assert_eq!(results[1].status, "skipped");
    }

    #[test]
    fn an_all_passing_plan_runs_every_step() {
        let mut count = 0;
        let results = run_plan(plan_of(&["one", "two", "three"]), |_| {
            count += 1;
            (Ok(captured(true, 0, "", "")), 1)
        });
        assert_eq!(count, 3);
        assert!(results.iter().all(|r| r.status == "passed"));
    }

    /// The header goes red on `failed > 0` alone, so the tallies decide
    /// whether a run reads as broken or fine. A skipped step is not a failure
    /// and must not be counted as one.
    #[test]
    fn the_tally_counts_each_status_in_its_own_column() {
        let results = run_plan(plan_of(&["one", "two", "three", "four"]), |step| {
            let outcome = if step.name == "two" {
                Ok(captured(false, 1, "", "Error: boom"))
            } else {
                Ok(captured(true, 0, "", ""))
            };
            (outcome, 1)
        });
        let report = summarize(results, 4_200);

        assert_eq!(report.passed, 1);
        assert_eq!(report.failed, 1);
        assert_eq!(report.skipped, 2);
        assert_eq!(report.total_duration_ms, 4_200);
        assert_eq!(
            report.passed + report.failed + report.skipped,
            report.steps.len(),
            "every step lands in exactly one column"
        );
    }

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

#[cfg(test)]
mod gate_tests {
    /// Every CI step reaches the ledger with a verdict.
    ///
    /// The bypass this closes was deliberate and documented: the command gate
    /// fails closed on non-git commands, so routing `npm test` through it
    /// refused the run. Declaring the step as its own allowlist is what makes
    /// the check answerable instead of skippable.
    ///
    /// Asserted on the *ledger* rather than on the report, because the report
    /// would look identical either way — a step that ran unjudged and a step
    /// that ran with a clean verdict both read as "passed". The whole point is
    /// that those are no longer the same event.
    #[test]
    fn ci_steps_are_judged_and_recorded() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().to_str().expect("utf8");
        // A real git repository: the runner refuses anything else, and a test
        // that skipped that check would not be exercising the runner.
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .current_dir(dir.path())
                .args(args)
                .output()
                .expect("git");
            assert!(out.status.success(), "git {args:?} failed: {out:?}");
        };
        git(&["init", "-b", "main"]);

        // A manifest, so the planner produces at least one step.
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"name":"t","scripts":{"test":"echo ok"}}"#,
        )
        .expect("write manifest");

        let before = crate::ledger::latest_cursor(repo).expect("cursor");
        // The run itself may pass or fail — npm may not even be installed — and
        // that is not what is under test. What matters is that nothing was
        // spawned without a verdict landing first.
        // The run itself may pass or fail — npm may not even be installed —
        // and that is not what is under test.
        let _ = super::run_ci_local(repo);
        let events = crate::ledger::tail(repo, before, 100).expect("tail");

        assert!(
            !events.is_empty(),
            "the CI runner spawned steps without recording any of them"
        );
        for row in &events {
            assert!(
                row.verdict_json.is_some(),
                "a CI step reached the ledger with no verdict: {:?}",
                row.object
            );
            assert!(
                matches!(row.outcome.as_str(), "ok" | "blocked"),
                "unexpected outcome {:?}",
                row.outcome
            );
        }
    }
}

#[cfg(test)]
mod verification_note_tests {
    use super::*;

    fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(dir.path())
                .output()
                .expect("git");
            assert!(out.status.success(), "git {args:?}: {out:?}");
        };
        git(&["init", "-b", "main"]);
        git(&["config", "user.name", "T"]);
        git(&["config", "user.email", "t@e.com"]);
        std::fs::write(dir.path().join("a.txt"), "one\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-m", "c0"]);
        dir
    }

    fn report(statuses: &[(&str, &str)]) -> CiLocalReport {
        let steps: Vec<CiStepResult> = statuses
            .iter()
            .map(|(name, status)| CiStepResult {
                name: (*name).to_string(),
                command: format!("npm run {name}"),
                status: (*status).to_string(),
                detail: if *status == "passed" {
                    String::new()
                } else {
                    "assertion failed: expected 1 to be 2\nsecret-looking log tail".to_string()
                },
                duration_ms: 5,
            })
            .collect();
        summarize(steps, 10)
    }

    fn head(dir: &std::path::Path) -> String {
        let out = std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(dir)
            .output()
            .expect("git");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    #[test]
    fn a_clean_pass_is_recorded_on_head() {
        let dir = repo();
        let r = report(&[("Frontend unit tests", "passed"), ("Clippy", "passed")]);

        let (commit, reason) = record_verification(dir.path(), &r);
        assert_eq!(reason, "", "a clean tree has no reason to refuse");
        assert_eq!(commit, head(dir.path()));

        let note = crate::engine::provenance::read_verification_note(
            dir.path().to_str().unwrap(),
            &commit,
        )
        .expect("read")
        .expect("a note was written");
        assert_eq!(note.verdict, "passed");
        assert_eq!(note.checked_by, "ci.local");
    }

    /// The claim a note makes is about a *commit's tree*. A run against a
    /// dirty checkout tested HEAD plus uncommitted work, so a note saying HEAD
    /// passed would describe a tree that was never tested — and unlike a
    /// number on a screen, it would be pushed to every clone.
    #[test]
    fn a_dirty_tree_is_refused_rather_than_recorded() {
        let dir = repo();
        std::fs::write(dir.path().join("a.txt"), "changed\n").unwrap();

        let r = report(&[("Frontend unit tests", "passed")]);
        let (commit, reason) = record_verification(dir.path(), &r);

        assert_eq!(commit, "", "nothing may be recorded");
        assert!(
            reason.contains("uncommitted"),
            "the refusal must say why, got {reason:?}"
        );
        assert_eq!(
            crate::engine::provenance::read_verification_note(
                dir.path().to_str().unwrap(),
                &head(dir.path()),
            )
            .expect("read"),
            None,
            "no note may exist for a tree that was never tested"
        );
    }

    /// An untracked scratch file is not part of any tree, so it cannot make
    /// HEAD's tree a lie — and refusing on it would mean a repository with one
    /// stray file could never record a verification again.
    #[test]
    fn an_untracked_file_does_not_block_recording() {
        let dir = repo();
        std::fs::write(dir.path().join("scratch.log"), "noise\n").unwrap();

        let (commit, reason) = record_verification(dir.path(), &report(&[("Tests", "passed")]));
        assert_eq!(reason, "");
        assert_eq!(commit, head(dir.path()));
    }

    #[test]
    fn a_failed_run_is_recorded_as_failed_not_skipped() {
        let dir = repo();
        let r = report(&[("Tests", "failed"), ("Clippy", "skipped")]);

        let (commit, reason) = record_verification(dir.path(), &r);
        assert_eq!(reason, "");
        let note = crate::engine::provenance::read_verification_note(
            dir.path().to_str().unwrap(),
            &commit,
        )
        .expect("read")
        .expect("recorded");
        assert_eq!(
            note.verdict, "failed",
            "a recorded failure is worth as much as a recorded pass"
        );
    }

    /// Notes are pushed to remotes. A failure detail is a captured log tail,
    /// which is exactly the kind of thing that must not leave the machine
    /// inside a git object.
    #[test]
    fn the_note_carries_step_names_and_statuses_never_their_output() {
        let dir = repo();
        let r = report(&[("Tests", "failed")]);
        let (commit, _) = record_verification(dir.path(), &r);

        let note = crate::engine::provenance::read_verification_note(
            dir.path().to_str().unwrap(),
            &commit,
        )
        .expect("read")
        .expect("recorded");
        let details = note.details.expect("details");
        assert_eq!(details, "Tests=failed");
        assert!(
            !details.contains("secret-looking log tail"),
            "captured output must never reach a pushable git object"
        );
    }

    /// The verdict reads every step, so a status this function has never heard
    /// of can only ever make the run *not* a pass.
    #[test]
    fn only_an_all_passed_run_is_a_pass() {
        assert_eq!(run_verdict(&report(&[("a", "passed")])), "passed");
        assert_eq!(
            run_verdict(&report(&[("a", "passed"), ("b", "passed")])),
            "passed"
        );
        assert_eq!(
            run_verdict(&report(&[("a", "passed"), ("b", "skipped")])),
            "failed"
        );
        assert_eq!(run_verdict(&report(&[("a", "failed")])), "failed");
        assert_eq!(
            run_verdict(&report(&[("a", "passed"), ("b", "some-future-status")])),
            "failed",
            "a status this build does not recognise is not a pass"
        );
        assert_eq!(
            run_verdict(&summarize(Vec::new(), 0)),
            "passed",
            "an empty plan cannot reach here: run_ci_local refuses it earlier"
        );
    }

    /// A cleanliness check that could not run must not be read as "clean".
    #[test]
    fn an_unanswerable_cleanliness_check_refuses_to_record() {
        let dir = tempfile::tempdir().expect("tempdir");
        // A directory, but not a repository: `git status` fails here.
        let (commit, reason) = record_verification(dir.path(), &report(&[("Tests", "passed")]));
        assert_eq!(commit, "");
        assert!(!reason.is_empty(), "the failure must explain itself");
        assert!(working_tree_is_dirty(dir.path()).is_err());
    }
}
