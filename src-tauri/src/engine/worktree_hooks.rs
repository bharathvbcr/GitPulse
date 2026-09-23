//! Declarative lifecycle hooks for GitPulse worktrees.
//!
//! Allows teams and workflows to configure lifecycle steps in `.gitpulse/hooks.toml`:
//! - `post_create`: Run setup commands (e.g. `npm ci`, database seeding) in newly created worktrees.
//! - `pre_merge`: Run checks/tests prior to merging a worktree into main.
//! - `post_merge`: Run teardown/cleanup actions.
//!
//! # Security
//! Hook execution is strictly gated under GitPulse's Repository Trust model.
//! Untrusted checkouts refuse hook execution and fail closed.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

pub const HOOK_TIMEOUT_SECS: u64 = 90;

/// Configuration read from `.gitpulse/hooks.toml` or `.gitpulse/hooks.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorktreeHooksConfig {
    #[serde(default)]
    pub post_create: Vec<String>,
    #[serde(default)]
    pub pre_merge: Vec<String>,
    #[serde(default)]
    pub post_merge: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct HooksEnvelope {
    #[serde(default)]
    worktree: WorktreeHooksConfig,
}

/// Result of executing a lifecycle hook.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HookExecutionResult {
    pub hook_name: String,
    pub command: String,
    pub success: bool,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub duration_ms: u64,
}

/// Finds and parses `.gitpulse/hooks.json` or `.gitpulse/hooks.toml` in `repo_root`.
pub fn load_worktree_hooks(repo_root: &Path) -> WorktreeHooksConfig {
    let json_path = repo_root.join(".gitpulse").join("hooks.json");
    if json_path.exists() {
        if let Ok(content) = fs::read_to_string(&json_path) {
            if let Ok(env) = serde_json::from_str::<HooksEnvelope>(&content) {
                return env.worktree;
            }
            if let Ok(cfg) = serde_json::from_str::<WorktreeHooksConfig>(&content) {
                return cfg;
            }
        }
    }

    let toml_path = repo_root.join(".gitpulse").join("hooks.toml");
    if toml_path.exists() {
        if let Ok(content) = fs::read_to_string(&toml_path) {
            return parse_simple_toml_hooks(&content);
        }
    }

    WorktreeHooksConfig::default()
}

/// Lightweight parser for simple `[worktree]` TOML configs without needing external toml crate.
fn parse_simple_toml_hooks(content: &str) -> WorktreeHooksConfig {
    let mut cfg = WorktreeHooksConfig::default();
    let mut current_section = "";

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') || trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            current_section = &trimmed[1..trimmed.len() - 1];
            continue;
        }

        if current_section == "worktree" || current_section.is_empty() {
            if let Some((key, val)) = trimmed.split_once('=') {
                let key = key.trim();
                let val = val.trim();
                // Parse string array: ["cmd1", "cmd2"]
                let items: Vec<String> = if val.starts_with('[') && val.ends_with(']') {
                    val[1..val.len() - 1]
                        .split(',')
                        .map(|s| s.trim().trim_matches('"').trim_matches('\'').to_string())
                        .filter(|s| !s.is_empty())
                        .collect()
                } else {
                    vec![val.trim_matches('"').trim_matches('\'').to_string()]
                };

                match key {
                    "post_create" => cfg.post_create = items,
                    "pre_merge" => cfg.pre_merge = items,
                    "post_merge" => cfg.post_merge = items,
                    _ => {}
                }
            }
        }
    }

    cfg
}

/// Executes a list of hook commands within `worktree_dir`, enforcing repository trust.
pub fn execute_worktree_hooks(
    repo_root: &Path,
    worktree_dir: &Path,
    commands: &[String],
    hook_name: &str,
    extra_env: &[(&str, &str)],
) -> Result<Vec<HookExecutionResult>, String> {
    if commands.is_empty() {
        return Ok(Vec::new());
    }

    // Security Gate: Untrusted repositories must NEVER execute hooks.
    crate::repository_trust::require(repo_root)?;

    let mut results = Vec::new();

    for cmd_str in commands {
        let trimmed = cmd_str.trim();
        if trimmed.is_empty() {
            continue;
        }

        let start = std::time::Instant::now();

        #[cfg(target_os = "windows")]
        let mut child = Command::new("cmd");
        #[cfg(target_os = "windows")]
        child.args(["/C", trimmed]);

        #[cfg(not(target_os = "windows"))]
        let mut child = Command::new("sh");
        #[cfg(not(target_os = "windows"))]
        child.args(["-c", trimmed]);

        child.current_dir(worktree_dir);
        child.stdout(Stdio::piped());
        child.stderr(Stdio::piped());

        for (k, v) in extra_env {
            child.env(k, v);
        }

        let output = match child.output() {
            Ok(out) => out,
            Err(e) => {
                return Err(format!(
                    "Failed to spawn hook command '{trimmed}' for {hook_name}: {e}"
                ));
            }
        };

        let duration_ms = start.elapsed().as_millis() as u64;
        let success = output.status.success();
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        let res = HookExecutionResult {
            hook_name: hook_name.to_string(),
            command: trimmed.to_string(),
            success,
            exit_code: output.status.code(),
            stdout,
            stderr,
            duration_ms,
        };

        let failed = !success;
        results.push(res);

        if failed {
            return Err(format!(
                "Hook '{hook_name}' command '{trimmed}' failed with exit code {:?}: {}",
                output.status.code(),
                results.last().map(|r| &r.stderr).unwrap_or(&String::new())
            ));
        }
    }

    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_load_hooks_from_toml() {
        let temp = tempdir().unwrap();
        let dot_gitpulse = temp.path().join(".gitpulse");
        fs::create_dir_all(&dot_gitpulse).unwrap();

        let toml_content = r#"
[worktree]
post_create = ["echo post_create_done"]
pre_merge = ["echo pre_merge_done"]
post_merge = []
"#;
        fs::write(dot_gitpulse.join("hooks.toml"), toml_content).unwrap();

        let loaded = load_worktree_hooks(temp.path());
        assert_eq!(loaded.post_create, vec!["echo post_create_done"]);
        assert_eq!(loaded.pre_merge, vec!["echo pre_merge_done"]);
        assert!(loaded.post_merge.is_empty());
    }
}
