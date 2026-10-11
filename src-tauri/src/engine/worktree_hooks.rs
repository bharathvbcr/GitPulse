//! Declarative lifecycle hooks for GitPulse worktrees.
//!
//! Allows teams and workflows to configure lifecycle steps in `.gitpulse/hooks.toml`
//! (or `.gitpulse/hooks.json`):
//! - `post_create`: Run setup commands (e.g. `npm ci`, database seeding) in newly created worktrees.
//! - `pre_merge`: Run checks/tests prior to merging a worktree into main.
//! - `post_merge`: Run teardown/cleanup actions.
//!
//! # Security
//! The file lives in the working tree, so whoever can write the tree (a pulled
//! branch, a compromised agent) can write it. Three things stand between that
//! and a shell:
//! 1. **Consent.** Nothing runs until a person has approved the exact file
//!    content for this repository ([`approve_worktree_hooks`]); any change to
//!    the file is a different file and asks again. The approval stores the
//!    content itself and compares it byte for byte, so there is no digest to
//!    collide.
//! 2. **The command gate.** Every command is judged by the harness and
//!    recorded in the ledger before it runs, like a git line.
//! 3. **Repository trust and a deadline.** Untrusted checkouts refuse, and a
//!    hook that hangs is killed with its process tree.
//!
//! The only way to run a hook is [`run_repo_hooks`]; the loader and runner
//! below it are private so no caller can skip the three steps.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Largest hooks file read. The whole file is shown to the approver and kept
/// as the approval, so it is bounded.
const MAX_HOOKS_FILE_BYTES: u64 = 64 * 1024;

/// Most commands one hook may list, and the longest one. A bound on what an
/// approver is asked to read in one sitting, not on legitimate setup scripts.
const MAX_HOOK_COMMANDS: usize = 32;
const MAX_HOOK_COMMAND_CHARS: usize = 4096;

/// Where approvals live, under the GitPulse configuration directory.
const APPROVAL_DIR: &str = "worktree-hook-approvals-v1";

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

impl WorktreeHooksConfig {
    fn commands(&self, hook_name: &str) -> Result<&[String], String> {
        match hook_name {
            "post_create" => Ok(&self.post_create),
            "pre_merge" => Ok(&self.pre_merge),
            "post_merge" => Ok(&self.post_merge),
            other => Err(format!("Unknown worktree hook '{other}'")),
        }
    }
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

/// What [`run_repo_hooks`] did. `skipped` says why nothing ran when the file
/// lists commands for this hook that were not approved; it is never set when
/// there was nothing to run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HookOutcome {
    pub results: Vec<HookExecutionResult>,
    pub skipped: Option<String>,
}

/// The hooks file as read once: its exact text and what it says. Everything
/// downstream (the approval check, the commands run) comes from this one read,
/// so a file swapped in between cannot be approved as one thing and run as
/// another.
#[derive(Debug, Clone, PartialEq, Eq)]
struct LoadedHooks {
    /// Repository-relative name of the file that was read.
    file: &'static str,
    content: String,
    config: WorktreeHooksConfig,
}

/// A hooks file for the approval dialog: the text the person is approving and
/// whether it already is.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorktreeHooksPreview {
    pub file: String,
    pub content: String,
    pub config: WorktreeHooksConfig,
    pub approved: bool,
}

/// Reads `.gitpulse/hooks.json`, else `.gitpulse/hooks.toml`, from `repo_root`.
///
/// `Ok(None)` is "no hooks file". A file that cannot be read, is too large or
/// does not parse is an error: it used to fall through to the other format or
/// to no hooks, so a typo turned a team's pre-merge checks off without a word.
fn read_hooks_file(repo_root: &Path) -> Result<Option<LoadedHooks>, String> {
    let dir = repo_root.join(".gitpulse");
    for (file, is_json) in [("hooks.json", true), ("hooks.toml", false)] {
        let path = dir.join(file);
        let meta = match fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(format!("Cannot read .gitpulse/{file}: {e}")),
        };
        if !meta.is_file() {
            return Err(format!(
                "Refusing .gitpulse/{file}: it is not a regular file"
            ));
        }
        if meta.len() > MAX_HOOKS_FILE_BYTES {
            return Err(format!(
                "Refusing .gitpulse/{file}: {} bytes is over the {MAX_HOOKS_FILE_BYTES} byte limit",
                meta.len()
            ));
        }
        let content =
            fs::read_to_string(&path).map_err(|e| format!("Cannot read .gitpulse/{file}: {e}"))?;
        let config = if is_json {
            parse_json_hooks(&content)
        } else {
            parse_simple_toml_hooks(&content)
        }
        .map_err(|e| format!("Invalid .gitpulse/{file}: {e}"))?;
        for list in [&config.post_create, &config.pre_merge, &config.post_merge] {
            if list.len() > MAX_HOOK_COMMANDS
                || list
                    .iter()
                    .any(|c| c.chars().count() > MAX_HOOK_COMMAND_CHARS)
            {
                return Err(format!(
                    "Invalid .gitpulse/{file}: a hook lists more than {MAX_HOOK_COMMANDS} commands or a command over {MAX_HOOK_COMMAND_CHARS} characters"
                ));
            }
        }
        return Ok(Some(LoadedHooks {
            file: if is_json {
                ".gitpulse/hooks.json"
            } else {
                ".gitpulse/hooks.toml"
            },
            content,
            config,
        }));
    }
    Ok(None)
}

/// Parses the JSON form: `{"worktree": {...}}` or the lists at the top level.
fn parse_json_hooks(content: &str) -> Result<WorktreeHooksConfig, String> {
    let value: serde_json::Value = serde_json::from_str(content).map_err(|e| e.to_string())?;
    let object = value.as_object().ok_or("expected a JSON object")?;
    let body = match object.get("worktree") {
        Some(inner) => inner.clone(),
        None => value.clone(),
    };
    serde_json::from_value(body).map_err(|e| e.to_string())
}

/// Lightweight parser for simple `[worktree]` TOML configs without needing external toml crate.
///
/// Accepts `key = "command"` and `key = ["one", "two"]` on one line, with
/// basic (`"..."`, `\"` and `\\` escapes) and literal (`'...'`) strings. A commas
/// inside a string belongs to the string. Anything else on a key it reads is
/// an error rather than a guess.
fn parse_simple_toml_hooks(content: &str) -> Result<WorktreeHooksConfig, String> {
    let mut cfg = WorktreeHooksConfig::default();
    let mut current_section = "";

    for (index, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') || trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with('[') && trimmed.ends_with(']') && !trimmed.contains('=') {
            current_section = trimmed[1..trimmed.len() - 1].trim();
            continue;
        }
        if current_section != "worktree" && !current_section.is_empty() {
            continue;
        }
        let Some((key, val)) = trimmed.split_once('=') else {
            return Err(format!("line {}: expected key = value", index + 1));
        };
        let key = key.trim();
        if !matches!(key, "post_create" | "pre_merge" | "post_merge") {
            continue;
        }
        let items = parse_toml_strings(val.trim())
            .map_err(|e| format!("line {}: {key}: {e}", index + 1))?;
        match key {
            "post_create" => cfg.post_create = items,
            "pre_merge" => cfg.pre_merge = items,
            _ => cfg.post_merge = items,
        }
    }

    Ok(cfg)
}

/// One quoted string or a one-line array of them.
fn parse_toml_strings(value: &str) -> Result<Vec<String>, String> {
    let mut chars = value.chars().peekable();
    let array = chars.peek() == Some(&'[');
    if array {
        chars.next();
    }
    let mut items = Vec::new();
    loop {
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        match chars.peek().copied() {
            Some(']') if array => {
                chars.next();
                break;
            }
            Some(quote @ ('"' | '\'')) => {
                chars.next();
                let mut text = String::new();
                loop {
                    match chars.next() {
                        None => return Err("unterminated string".to_string()),
                        Some(c) if c == quote => break,
                        Some('\\') if quote == '"' => match chars.next() {
                            Some('"') => text.push('"'),
                            Some('\\') => text.push('\\'),
                            Some('n') => text.push('\n'),
                            Some('t') => text.push('\t'),
                            Some(other) => return Err(format!("unsupported escape \\{other}")),
                            None => return Err("unterminated string".to_string()),
                        },
                        Some(c) => text.push(c),
                    }
                }
                items.push(text);
            }
            Some(other) => return Err(format!("expected a quoted string, found '{other}'")),
            None => {
                if array {
                    return Err("unterminated array (multi-line arrays are not supported)".into());
                }
                break;
            }
        }
        if !array {
            break;
        }
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        match chars.next() {
            Some(',') => {}
            Some(']') => break,
            None => return Err("unterminated array (multi-line arrays are not supported)".into()),
            Some(other) => return Err(format!("expected ',' or ']', found '{other}'")),
        }
    }
    let rest: String = chars.collect();
    let rest = rest.trim();
    if !rest.is_empty() && !rest.starts_with('#') {
        return Err(format!("unexpected text after the value: {rest}"));
    }
    Ok(items.into_iter().filter(|s| !s.trim().is_empty()).collect())
}

// ---- consent ---------------------------------------------------------------

/// One approval: the repository (its common git directory, so every worktree
/// of it shares the answer) and the exact file approved.
#[derive(Debug, Serialize, Deserialize)]
struct Approval {
    repository: String,
    file: String,
    content: String,
}

fn approval_store() -> Result<PathBuf, String> {
    crate::tool_config::default_config_dir()
        .map(|dir| dir.join(APPROVAL_DIR))
        .ok_or_else(|| "Cannot resolve GitPulse hook approval storage".to_string())
}

/// The repository the approval is keyed to, and the record's file name in
/// `store`. Keyed by the common git directory so a linked worktree and the
/// main checkout agree.
fn approval_slot(store: &Path, repo_root: &Path) -> Result<(String, PathBuf), String> {
    let (_, common) = crate::repository_trust::git_directories(repo_root)?;
    let repository = common.to_string_lossy().into_owned();
    let name = format!(
        "{:016x}.json",
        crate::ledger::ids::fnv1a64(repository.as_bytes())
    );
    Ok((repository, store.join(name)))
}

/// Whether `loaded` is exactly what was approved for this repository. Any
/// failure to read the record is "not approved": an unreadable approval never
/// reads as consent.
fn is_approved(store: &Path, repo_root: &Path, loaded: &LoadedHooks) -> Result<bool, String> {
    let (repository, path) = approval_slot(store, repo_root)?;
    let Ok(text) = fs::read_to_string(&path) else {
        return Ok(false);
    };
    let Ok(approval) = serde_json::from_str::<Approval>(&text) else {
        return Ok(false);
    };
    Ok(approval.repository == repository
        && approval.file == loaded.file
        && approval.content == loaded.content)
}

/// Shows the hooks file for the approval dialog. `None`: there is none.
pub fn preview_worktree_hooks(repo_root: &Path) -> Result<Option<WorktreeHooksPreview>, String> {
    preview_in(&approval_store()?, repo_root)
}

fn preview_in(store: &Path, repo_root: &Path) -> Result<Option<WorktreeHooksPreview>, String> {
    let Some(loaded) = read_hooks_file(repo_root)? else {
        return Ok(None);
    };
    let approved = is_approved(store, repo_root, &loaded)?;
    Ok(Some(WorktreeHooksPreview {
        file: loaded.file.to_string(),
        content: loaded.content,
        config: loaded.config,
        approved,
    }))
}

/// Records consent to the hooks file whose text is `shown`, the text the
/// person was looking at. The file is read again and must still be exactly
/// that, or nothing is recorded: approving what was on screen, not whatever is
/// on disk a moment later.
pub fn approve_worktree_hooks(repo_root: &Path, shown: &str) -> Result<(), String> {
    approve_in(&approval_store()?, repo_root, shown)
}

fn approve_in(store: &Path, repo_root: &Path, shown: &str) -> Result<(), String> {
    crate::repository_trust::require(repo_root)?;
    let loaded = read_hooks_file(repo_root)?
        .ok_or("This repository has no .gitpulse/hooks file to approve")?;
    if loaded.content != shown {
        return Err(
            "The hooks file changed after it was shown; review the new content and approve again"
                .to_string(),
        );
    }
    let (repository, path) = approval_slot(store, repo_root)?;
    fs::create_dir_all(store).map_err(|e| format!("Cannot create hook approval storage: {e}"))?;
    let record = serde_json::to_string(&Approval {
        repository,
        file: loaded.file.to_string(),
        content: loaded.content,
    })
    .map_err(|e| e.to_string())?;
    let staged = path.with_extension("json.tmp");
    fs::write(&staged, record).map_err(|e| format!("Cannot record hook approval: {e}"))?;
    fs::rename(&staged, &path).map_err(|e| format!("Cannot record hook approval: {e}"))
}

/// The command gate as hooks are handed it: the repository and the argv
/// (program first), like [`crate::engine::worktree`]'s teardown gate.
type HookGate<'a> = &'a dyn Fn(&str, &[&str]) -> Result<(), String>;

/// Runs this repository's `hook_name` commands in `worktree_dir`, if — and
/// only if — the exact hooks file was approved. The one entry point.
///
/// * No file, or nothing listed for the hook: nothing to do, no outcome.
/// * A file that does not parse: an error. Never a fallback.
/// * Not approved (never, or the file changed since): nothing runs and
///   `skipped` says so.
/// * Approved: each command is judged by the harness gate and recorded in the
///   ledger as `sh -c <command>`, then run under a deadline. A refusal stops
///   the list.
pub fn run_repo_hooks(
    repo_root: &Path,
    worktree_dir: &Path,
    hook_name: &str,
    extra_env: &[(&str, &str)],
) -> Result<HookOutcome, String> {
    run_repo_hooks_with(
        &approval_store()?,
        repo_root,
        worktree_dir,
        hook_name,
        extra_env,
        &|repo, argv| crate::harness::guard_command(repo, argv).map(|_| ()),
        HOOK_TIMEOUT,
    )
}

fn run_repo_hooks_with(
    store: &Path,
    repo_root: &Path,
    worktree_dir: &Path,
    hook_name: &str,
    extra_env: &[(&str, &str)],
    gate: HookGate<'_>,
    timeout: std::time::Duration,
) -> Result<HookOutcome, String> {
    let Some(loaded) = read_hooks_file(repo_root)? else {
        return Ok(HookOutcome::default());
    };
    let commands = loaded.config.commands(hook_name)?;
    if commands.iter().all(|c| c.trim().is_empty()) {
        return Ok(HookOutcome::default());
    }
    // Security Gate: Untrusted repositories must NEVER execute hooks.
    crate::repository_trust::require(repo_root)?;
    if !is_approved(store, repo_root, &loaded)? {
        return Ok(HookOutcome {
            results: Vec::new(),
            skipped: Some(format!(
                "{} lists {hook_name} commands that have not been approved, so none ran. \
                 Review and approve the file in the worktree options.",
                loaded.file
            )),
        });
    }
    let repo_text = repo_root
        .to_str()
        .ok_or("Repository path is not UTF-8")?
        .to_string();
    let program: &[&str] = if cfg!(target_os = "windows") {
        &["cmd", "/C"]
    } else {
        &["sh", "-c"]
    };
    // Judge every command before any runs, so a refused third command does
    // not leave the first two already executed.
    for command in commands.iter().map(|c| c.trim()).filter(|c| !c.is_empty()) {
        let argv = [program[0], program[1], command];
        gate(&repo_text, &argv)
            .map_err(|e| format!("Hook '{hook_name}' command '{command}' was refused: {e}"))?;
    }
    let results = execute_worktree_hooks_within(
        repo_root,
        worktree_dir,
        commands,
        hook_name,
        extra_env,
        timeout,
    )?;
    Ok(HookOutcome {
        results,
        skipped: None,
    })
}

/// Wall clock allowed one hook command. Generous — a post_create hook may
/// install dependencies — but finite.
const HOOK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15 * 60);

/// Bytes of stdout kept per hook command; the rest is drained and dropped.
const HOOK_OUTPUT_CAP: usize = 1024 * 1024;

fn execute_worktree_hooks_within(
    repo_root: &Path,
    worktree_dir: &Path,
    commands: &[String],
    hook_name: &str,
    extra_env: &[(&str, &str)],
    timeout: std::time::Duration,
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

        for (k, v) in extra_env {
            child.env(k, v);
        }

        // Through the bounded runner, like every other spawn outside the git
        // seam: a hook that hangs (a dev server started by mistake, a prompt
        // waiting on a terminal that is not there) is killed with its process
        // tree at the deadline instead of hanging worktree creation, and its
        // output is capped. It was `child.output()`: no deadline, no cap.
        let output = match crate::engine::git_cli::run_bounded_capped(
            child,
            "worktree hook",
            timeout,
            None,
            HOOK_OUTPUT_CAP,
        ) {
            Ok(out) => out,
            Err(e) => {
                return Err(format!(
                    "Hook '{hook_name}' command '{trimmed}' did not complete: {e}"
                ));
            }
        };

        let duration_ms = start.elapsed().as_millis() as u64;
        let success = output.success;
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        let res = HookExecutionResult {
            hook_name: hook_name.to_string(),
            command: trimmed.to_string(),
            success,
            exit_code: Some(output.status_code),
            stdout,
            stderr,
            duration_ms,
        };

        let failed = !success;
        results.push(res);

        if failed {
            return Err(format!(
                "Hook '{hook_name}' command '{trimmed}' failed with exit code {:?}: {}",
                Some(output.status_code),
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

    /// A hook that never finishes is killed at the deadline and reported,
    /// instead of hanging worktree creation: it ran through `output()` with
    /// no deadline at all.
    #[cfg(unix)]
    #[test]
    fn a_hanging_hook_is_killed_at_the_deadline() {
        let repo = crate::test_support::git_repo();
        let started = std::time::Instant::now();
        let result = execute_worktree_hooks_within(
            repo.path(),
            repo.path(),
            &["sleep 30".to_string()],
            "post_create",
            &[],
            std::time::Duration::from_secs(1),
        );
        let elapsed = started.elapsed();
        let err = result.expect_err("a hook past its deadline must fail");
        assert!(err.contains("did not complete"), "{err}");
        assert!(
            elapsed < crate::test_support::coverage_relaxed(std::time::Duration::from_secs(10)),
            "the hook ran {elapsed:?} against a 1s deadline"
        );
    }

    /// A finishing hook still reports its output and status.
    #[cfg(unix)]
    #[test]
    fn a_finishing_hook_reports_output_and_status() {
        let repo = crate::test_support::git_repo();
        let results = execute_worktree_hooks_within(
            repo.path(),
            repo.path(),
            &["echo hello".to_string()],
            "post_create",
            &[("GP_HOOK_TEST", "1")],
            HOOK_TIMEOUT,
        )
        .expect("hook runs");
        assert_eq!(results.len(), 1);
        assert!(results[0].success);
        assert_eq!(results[0].exit_code, Some(0));
        assert_eq!(results[0].stdout.trim(), "hello");
        let failed = execute_worktree_hooks_within(
            repo.path(),
            repo.path(),
            &["exit 3".to_string()],
            "pre_merge",
            &[],
            HOOK_TIMEOUT,
        )
        .expect_err("a failing hook fails");
        assert!(failed.contains("Some(3)"), "{failed}");
    }

    fn allow(_: &str, _: &[&str]) -> Result<(), String> {
        Ok(())
    }

    fn write_hooks(repo: &Path, name: &str, content: &str) {
        fs::create_dir_all(repo.join(".gitpulse")).unwrap();
        fs::write(repo.join(".gitpulse").join(name), content).unwrap();
    }

    fn run(store: &Path, repo: &Path, gate: HookGate<'_>) -> Result<HookOutcome, String> {
        run_repo_hooks_with(
            store,
            repo,
            repo,
            "post_create",
            &[],
            gate,
            std::time::Duration::from_secs(30),
        )
    }

    const TOUCH: &str = r#"{"worktree":{"post_create":["touch ran.marker"]}}"#;

    /// Repository content alone must not run commands: a hooks file nobody
    /// approved is reported and nothing executes. It ran everything it found.
    #[cfg(unix)]
    #[test]
    fn an_unapproved_hooks_file_runs_nothing() {
        let repo = crate::test_support::git_repo();
        let store = tempdir().unwrap();
        write_hooks(repo.path(), "hooks.json", TOUCH);
        let outcome = run(store.path(), repo.path(), &allow).unwrap();
        assert!(outcome.results.is_empty());
        let why = outcome.skipped.expect("the skip is reported");
        assert!(why.contains("not been approved"), "{why}");
        assert!(!repo.path().join("ran.marker").exists());
        let preview = preview_in(store.path(), repo.path()).unwrap().unwrap();
        assert!(!preview.approved);
        assert_eq!(preview.config.post_create, vec!["touch ran.marker"]);
    }

    /// Approval is for the exact file: once approved the hooks run, and any
    /// edit — one added command, a whitespace change — asks again.
    #[cfg(unix)]
    #[test]
    fn an_approval_covers_only_the_exact_file_and_a_change_asks_again() {
        let repo = crate::test_support::git_repo();
        let store = tempdir().unwrap();
        write_hooks(repo.path(), "hooks.json", TOUCH);
        approve_in(store.path(), repo.path(), TOUCH).unwrap();
        let outcome = run(store.path(), repo.path(), &allow).unwrap();
        assert_eq!(outcome.results.len(), 1);
        assert!(outcome.skipped.is_none());
        assert!(repo.path().join("ran.marker").exists());
        fs::remove_file(repo.path().join("ran.marker")).unwrap();

        let edited = format!("{TOUCH} ");
        write_hooks(repo.path(), "hooks.json", &edited);
        let outcome = run(store.path(), repo.path(), &allow).unwrap();
        assert!(outcome.skipped.is_some(), "a changed file must ask again");
        assert!(!repo.path().join("ran.marker").exists());
    }

    /// Approving records what the person saw, not what is on disk a moment
    /// later.
    #[test]
    fn approval_of_stale_content_is_refused() {
        let repo = crate::test_support::git_repo();
        let store = tempdir().unwrap();
        write_hooks(repo.path(), "hooks.json", TOUCH);
        let err = approve_in(store.path(), repo.path(), r#"{"worktree":{}}"#).unwrap_err();
        assert!(err.contains("changed after it was shown"), "{err}");
        assert!(!is_approved_for_test(store.path(), repo.path()));
    }

    fn is_approved_for_test(store: &Path, repo: &Path) -> bool {
        preview_in(store, repo).unwrap().unwrap().approved
    }

    /// An approval is per repository: the same text in another repository is
    /// not approved there.
    #[test]
    fn an_approval_does_not_cross_repositories() {
        let one = crate::test_support::git_repo();
        let two = crate::test_support::git_repo();
        let store = tempdir().unwrap();
        write_hooks(one.path(), "hooks.json", TOUCH);
        write_hooks(two.path(), "hooks.json", TOUCH);
        approve_in(store.path(), one.path(), TOUCH).unwrap();
        assert!(is_approved_for_test(store.path(), one.path()));
        assert!(!is_approved_for_test(store.path(), two.path()));
    }

    /// Every command goes through the gate before any runs, and a refusal
    /// stops the whole list: nothing ran, not even the commands before it.
    #[cfg(unix)]
    #[test]
    fn a_refused_command_stops_the_list_before_anything_runs() {
        let repo = crate::test_support::git_repo();
        let store = tempdir().unwrap();
        let content = r#"{"worktree":{"post_create":["touch first.marker","rm -rf everything"]}}"#;
        write_hooks(repo.path(), "hooks.json", content);
        approve_in(store.path(), repo.path(), content).unwrap();
        let judged = std::sync::Mutex::new(Vec::new());
        let gate = |_: &str, argv: &[&str]| -> Result<(), String> {
            judged.lock().unwrap().push(argv.join(" "));
            if argv[2].starts_with("rm") {
                Err("refused by policy".to_string())
            } else {
                Ok(())
            }
        };
        let err = run(store.path(), repo.path(), &gate).unwrap_err();
        assert!(
            err.contains("rm -rf everything") && err.contains("refused"),
            "{err}"
        );
        assert!(!repo.path().join("first.marker").exists());
        assert_eq!(
            *judged.lock().unwrap(),
            vec!["sh -c touch first.marker", "sh -c rm -rf everything"]
        );
    }

    /// A hooks.json that does not parse is an error. It used to fall back to
    /// hooks.toml, or to no hooks at all.
    #[test]
    fn a_hooks_json_that_does_not_parse_is_an_error_not_a_fallback() {
        let repo = crate::test_support::git_repo();
        let store = tempdir().unwrap();
        write_hooks(repo.path(), "hooks.json", "{ not json");
        write_hooks(
            repo.path(),
            "hooks.toml",
            "[worktree]\npost_create = [\"echo hi\"]\n",
        );
        let err = run(store.path(), repo.path(), &allow).unwrap_err();
        assert!(err.contains("Invalid .gitpulse/hooks.json"), "{err}");
        let err = preview_in(store.path(), repo.path()).unwrap_err();
        assert!(err.contains("Invalid .gitpulse/hooks.json"), "{err}");
    }

    /// A file over the bound is refused rather than read into an approval.
    #[test]
    fn an_oversized_hooks_file_is_refused() {
        let repo = crate::test_support::git_repo();
        let big = format!(
            "{{\"worktree\":{{\"post_create\":[\"{}\"]}}}}",
            "x".repeat(MAX_HOOKS_FILE_BYTES as usize)
        );
        write_hooks(repo.path(), "hooks.json", &big);
        let err = read_hooks_file(repo.path()).unwrap_err();
        assert!(err.contains("over the"), "{err}");
    }

    /// The TOML reader split on every comma, cutting `a, b` inside one command
    /// in two; strings now own their commas, and what it cannot read is an
    /// error.
    #[test]
    fn toml_commands_keep_their_commas_and_malformed_values_are_errors() {
        let parsed = parse_simple_toml_hooks(
            "[worktree]\npost_create = [\"echo a, b\", 'x \"y\"'] # note\npre_merge = \"make test\"\n",
        )
        .unwrap();
        assert_eq!(parsed.post_create, vec!["echo a, b", "x \"y\""]);
        assert_eq!(parsed.pre_merge, vec!["make test"]);
        for bad in [
            "[worktree]\npost_create = [\"a\",\n",
            "[worktree]\npost_create = npm ci\n",
            "[worktree]\npost_create = [\"a\" \"b\"]\n",
            "[worktree]\npost_create = [\"a\"] trailing\n",
        ] {
            assert!(parse_simple_toml_hooks(bad).is_err(), "{bad}");
        }
    }

    /// The lists at the top level, without the `worktree` envelope, are read:
    /// they parsed as an empty envelope and were silently dropped.
    #[test]
    fn flat_json_lists_are_read() {
        let parsed = parse_json_hooks(r#"{"post_merge":["echo done"]}"#).unwrap();
        assert_eq!(parsed.post_merge, vec!["echo done"]);
        assert!(parse_json_hooks("[1]").is_err());
    }

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

        let loaded = read_hooks_file(temp.path()).unwrap().unwrap().config;
        assert_eq!(loaded.post_create, vec!["echo post_create_done"]);
        assert_eq!(loaded.pre_merge, vec!["echo pre_merge_done"]);
        assert!(loaded.post_merge.is_empty());
    }
}
