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

use crate::engine::git_cli::{capture_command, validate_repo};
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
    let start = combined.len().saturating_sub(OUTPUT_TAIL_BYTES);
    let tail = &combined[start..];
    // Trim forward to a char boundary so a multibyte character cut in half
    // does not render as replacement garbage.
    let boundary = tail
        .iter()
        .position(|b| (*b & 0xC0) != 0x80)
        .unwrap_or(tail.len());
    String::from_utf8_lossy(&tail[boundary..])
        .lines()
        .collect::<Vec<_>>()
        .join("\n")
}

/// Runs the planned pipeline against `repo_path`. Every condition that stops
/// a step from running is recorded on that step — never folded into another
/// step's result and never reported as a pass.
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
            results.push(CiStepResult {
                name: step.name.to_string(),
                command: step.rendered_command(),
                status: "skipped".into(),
                detail: "skipped after an earlier step failed".into(),
                duration_ms: 0,
            });
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
        match outcome {
            Ok(output) if output.success => results.push(CiStepResult {
                name: step.name.to_string(),
                command: step.rendered_command(),
                status: "passed".into(),
                detail: String::new(),
                duration_ms,
            }),
            Ok(output) => {
                failed_early = true;
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
                results.push(CiStepResult {
                    name: step.name.to_string(),
                    command: step.rendered_command(),
                    status: "failed".into(),
                    detail: if tail.trim().is_empty() {
                        summary
                    } else {
                        format!("{summary}\n\n--- output (tail) ---\n{tail}")
                    },
                    duration_ms,
                });
            }
            Err(e) => {
                // Spawn failure or timeout: the check could not run to an
                // exit code, so it must not look like a normal failure of
                // the project under test.
                failed_early = true;
                results.push(CiStepResult {
                    name: step.name.to_string(),
                    command: step.rendered_command(),
                    status: "failed".into(),
                    detail: format!("could not run: {e}"),
                    duration_ms,
                });
            }
        }
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
