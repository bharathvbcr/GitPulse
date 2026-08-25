//! Linked-worktree support.
//!
//! AI coding agents parallelize by checking a task out into its own worktree,
//! so a client that only understands "the repository" breaks down the moment
//! an agent's workflow starts. This module lists every worktree of a
//! repository — including the main checkout and bare entries — and creates and
//! removes them through the same validated, harness-gated paths as every other
//! write.

use crate::engine::git_cli::{git_text, validate_repo};
use crate::engine::git_writer::validate_oid_or_revision;
use crate::engine::git_writer::validate_ref_name;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// How many worktrees get a dirty-file scan when listing. Worktrees number in
/// the low dozens even under heavy agent use; this cap keeps a pathological
/// tree from turning one listing call into hundreds of subprocess spawns.
const MAX_DIRTY_SCANS: usize = 32;

/// One entry of `git worktree list`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorktreeInfo {
    /// Absolute path of the worktree directory.
    pub path: String,
    /// Directory name, for display where the full path does not fit.
    pub name: String,
    pub head: String,
    /// Short branch name, `None` when detached or bare.
    pub branch: Option<String>,
    pub is_bare: bool,
    pub is_detached: bool,
    /// True for the repository's primary worktree (listed first by git).
    pub is_main: bool,
    pub is_locked: bool,
    pub is_prunable: bool,
    /// Working-tree change count from `git status`; `None` when not scanned
    /// (bare entries, or past the scan cap).
    pub dirty_files: Option<usize>,
}

/// One parsed block of `worktree list --porcelain`, before dirty counting.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct ParsedWorktree {
    path: String,
    head: String,
    branch: Option<String>,
    is_bare: bool,
    is_detached: bool,
    is_locked: bool,
    is_prunable: bool,
}

/// Parses `git worktree list --porcelain` output.
///
/// The format is newline-separated fields in blank-line-delimited blocks:
/// `worktree <path>`, `HEAD <sha>`, one of `branch <ref>` / `detached`,
/// optionally `bare`, `locked [reason]`, `prunable [reason]`. Unknown lines are
/// skipped rather than rejected so a newer git can add fields without breaking
/// this parser.
fn parse_worktree_porcelain(raw: &str) -> Vec<ParsedWorktree> {
    let mut out: Vec<ParsedWorktree> = Vec::new();
    let mut current: Option<ParsedWorktree> = None;
    let flush = |current: &mut Option<ParsedWorktree>, out: &mut Vec<ParsedWorktree>| {
        if let Some(entry) = current.take() {
            if !entry.path.is_empty() {
                out.push(entry);
            }
        }
    };

    for line in raw.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            flush(&mut current, &mut out);
            continue;
        }
        let Some((key, value)) = line.split_once(' ') else {
            // A field with no payload ("detached" never appears alone, but a
            // bare "locked" or "prunable" can).
            match line {
                "detached" => {
                    if let Some(entry) = current.as_mut() {
                        entry.is_detached = true;
                    }
                }
                // `bare` carries no payload either.
                "bare" => {
                    if let Some(entry) = current.as_mut() {
                        entry.is_bare = true;
                    }
                }
                "locked" => {
                    if let Some(entry) = current.as_mut() {
                        entry.is_locked = true;
                    }
                }
                "prunable" => {
                    if let Some(entry) = current.as_mut() {
                        entry.is_prunable = true;
                    }
                }
                _ => {}
            }
            continue;
        };
        match key {
            "worktree" => {
                flush(&mut current, &mut out);
                current = Some(ParsedWorktree {
                    path: value.to_string(),
                    ..ParsedWorktree::default()
                });
            }
            "HEAD" => {
                if let Some(entry) = current.as_mut() {
                    entry.head = value.to_string();
                }
            }
            "branch" => {
                if let Some(entry) = current.as_mut() {
                    entry.branch = Some(value.trim_start_matches("refs/heads/").to_string());
                }
            }
            "bare" => {
                if let Some(entry) = current.as_mut() {
                    entry.is_bare = true;
                }
            }
            "locked" => {
                if let Some(entry) = current.as_mut() {
                    entry.is_locked = true;
                }
            }
            "prunable" => {
                if let Some(entry) = current.as_mut() {
                    entry.is_prunable = true;
                }
            }
            _ => {}
        }
    }
    flush(&mut current, &mut out);
    out
}

/// Counts entries in `git status --porcelain -z` output.
///
/// Rename/copy records carry two NUL-separated fields (new path, then origin);
/// both belong to one entry.
fn count_status_entries(bytes: &[u8]) -> usize {
    let mut count = 0usize;
    let mut i = 0usize;
    while i + 3 <= bytes.len() {
        let x = bytes[i] as char;
        let y = bytes[i + 1] as char;
        i += 3; // XY plus separator
        let end = match bytes[i..].iter().position(|&b| b == 0) {
            Some(n) => i + n,
            None => break,
        };
        i = end + 1;
        count += 1;
        if x == 'R' || x == 'C' || y == 'R' || y == 'C' {
            // Skip the paired original-path record.
            match bytes[i..].iter().position(|&b| b == 0) {
                Some(n) => i += n + 1,
                None => break,
            }
        }
    }
    count
}

fn display_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| path.to_string())
}

/// Lists every worktree of the repository, main entry first, with dirty-file
/// counts for the worktrees closest to the front of the list.
pub fn list_worktrees(repo_path: &str) -> Result<Vec<WorktreeInfo>, String> {
    let repo = validate_repo(repo_path)?;
    let stdout = git_text(&repo, &["worktree", "list", "--porcelain"])?;
    let parsed = parse_worktree_porcelain(&stdout);

    let scan_targets: Vec<usize> = parsed
        .iter()
        .enumerate()
        .filter(|(_, entry)| !entry.is_bare)
        .map(|(idx, _)| idx)
        .take(MAX_DIRTY_SCANS)
        .collect();

    let dirty: HashMap<usize, usize> = scan_targets
        .into_par_iter()
        .filter_map(|idx| {
            let dir = Path::new(&parsed[idx].path);
            if !dir.is_dir() {
                return None;
            }
            let stdout = git_text(dir, &["status", "--porcelain", "-z"]).ok()?;
            Some((idx, count_status_entries(stdout.as_bytes())))
        })
        .collect();

    Ok(parsed
        .into_iter()
        .enumerate()
        .map(|(idx, entry)| WorktreeInfo {
            name: display_name(&entry.path),
            is_main: idx == 0,
            dirty_files: dirty.get(&idx).copied(),
            path: entry.path,
            head: entry.head,
            branch: entry.branch,
            is_bare: entry.is_bare,
            is_detached: entry.is_detached,
            is_locked: entry.is_locked,
            is_prunable: entry.is_prunable,
        })
        .collect())
}

use std::collections::HashMap;

fn validate_target_path(target: &str) -> Result<(), String> {
    if target.is_empty() || target.contains('\0') || target.starts_with('-') {
        return Err("Invalid worktree path".into());
    }
    if !Path::new(target).is_absolute() {
        return Err("Worktree path must be absolute".into());
    }
    Ok(())
}

/// Creates a linked worktree. Exactly one of `new_branch` / `detach` shapes the
/// checkout; `start_point` may name any commit-ish.
pub fn add_worktree(
    repo_path: &str,
    target_path: &str,
    new_branch: Option<&str>,
    start_point: Option<&str>,
    detach: bool,
) -> Result<String, String> {
    let repo = validate_repo(repo_path)?;
    validate_target_path(target_path)?;
    if let Some(branch) = new_branch {
        validate_ref_name(branch)?;
    }
    if let Some(start) = start_point {
        validate_oid_or_revision(start)?;
    }

    let mut args: Vec<&str> = vec!["worktree", "add"];
    if let Some(branch) = new_branch {
        args.push("-b");
        args.push(branch);
    } else if detach {
        args.push("--detach");
    }
    args.push(target_path);
    if let Some(start) = start_point {
        args.push(start);
    }
    git_text(&repo, &args)?;
    Ok(target_path.to_string())
}

/// Removes a linked worktree. Git refuses the main worktree itself.
pub fn remove_worktree(repo_path: &str, target_path: &str, force: bool) -> Result<(), String> {
    let repo = validate_repo(repo_path)?;
    validate_target_path(target_path)?;
    let mut args: Vec<&str> = vec!["worktree", "remove"];
    if force {
        args.push("--force");
    }
    args.push(target_path);
    git_text(&repo, &args)?;
    Ok(())
}

/// The exact argv [`add_worktree`] would run, built independently so the
/// command gate judges the same command line the writer executes.
pub fn add_worktree_argv(
    target_path: &str,
    new_branch: Option<&str>,
    start_point: Option<&str>,
    detach: bool,
) -> Vec<String> {
    let mut argv: Vec<String> = vec!["git".into(), "worktree".into(), "add".into()];
    if let Some(branch) = new_branch {
        argv.push("-b".into());
        argv.push(branch.into());
    } else if detach {
        argv.push("--detach".into());
    }
    argv.push(target_path.into());
    if let Some(start) = start_point {
        argv.push(start.into());
    }
    argv
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_porcelain_full_blocks() {
        let raw = "\
worktree /repos/main
HEAD abc123
branch refs/heads/main

worktree /repos/task-a
HEAD def456
detached

";
        let parsed = parse_worktree_porcelain(raw);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].path, "/repos/main");
        assert_eq!(parsed[0].head, "abc123");
        assert_eq!(parsed[0].branch.as_deref(), Some("main"));
        assert!(!parsed[0].is_detached);
        assert_eq!(parsed[1].path, "/repos/task-a");
        assert!(parsed[1].is_detached);
        assert!(parsed[1].branch.is_none());
    }

    #[test]
    fn test_parse_porcelain_bare_locked_prunable_and_unknown_fields() {
        let raw = "\
worktree /srv/bare.git
bare

worktree /repos/locked-one
HEAD abc
branch refs/heads/wip
locked reason why
prunable
some-future-field whatever

";
        let parsed = parse_worktree_porcelain(raw);
        assert_eq!(parsed.len(), 2);
        assert!(parsed[0].is_bare);
        assert!(!parsed[0].is_detached);
        assert!(parsed[1].is_locked);
        assert!(parsed[1].is_prunable);
        assert_eq!(parsed[1].branch.as_deref(), Some("wip"));
    }

    #[test]
    fn test_parse_porcelain_empty_input_yields_nothing() {
        assert!(parse_worktree_porcelain("").is_empty());
        assert!(parse_worktree_porcelain("\n\n").is_empty());
    }

    #[test]
    fn test_count_status_entries_counts_renames_once() {
        // M + R (two NUL fields) + untracked
        let mut bytes: Vec<u8> = Vec::new();
        bytes.extend_from_slice(b"M  src/a.rs\0");
        bytes.extend_from_slice(b"R  new-name.rs\0old-name.rs\0");
        bytes.extend_from_slice(b"?? notes.txt\0");
        assert_eq!(count_status_entries(&bytes), 3);
    }

    #[test]
    fn test_count_status_entries_empty_and_truncated() {
        assert_eq!(count_status_entries(b""), 0);
        assert_eq!(count_status_entries(b"M"), 0);
        // A record with a structurally valid but empty path still counts:
        // git never emits one, so the parser stays simple rather than
        // second-guessing its own input.
        assert_eq!(count_status_entries(b"M  \0"), 1);
    }

    #[test]
    fn test_parse_porcelain_bare_without_payload() {
        let parsed = parse_worktree_porcelain("worktree /srv/bare.git\nbare\n");
        assert_eq!(parsed.len(), 1);
        assert!(parsed[0].is_bare);
    }

    #[test]
    fn test_validate_target_path_rejects_flags_and_relative() {
        assert!(validate_target_path("/tmp/wt").is_ok());
        assert!(validate_target_path("--force").is_err());
        assert!(validate_target_path("relative/path").is_err());
        assert!(validate_target_path("").is_err());
    }

    #[test]
    fn test_add_worktree_argv_matches_writer_shape() {
        let argv = add_worktree_argv("/tmp/wt", Some("agent/task"), Some("main"), false);
        assert_eq!(
            argv,
            vec![
                "git",
                "worktree",
                "add",
                "-b",
                "agent/task",
                "/tmp/wt",
                "main"
            ]
        );
        let detached = add_worktree_argv("/tmp/wt2", None, None, true);
        assert_eq!(
            detached,
            vec!["git", "worktree", "add", "--detach", "/tmp/wt2"]
        );
        let plain = add_worktree_argv("/tmp/wt3", None, None, false);
        assert_eq!(plain, vec!["git", "worktree", "add", "/tmp/wt3"]);
    }

    #[test]
    fn test_display_name_handles_root_paths() {
        assert_eq!(display_name("/repos/main"), "main");
        assert_eq!(display_name("/"), "/");
    }

    #[cfg(test)]
    fn git_in(dir: &Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .args([
                "-c",
                "user.name=GitPulse",
                "-c",
                "user.email=gitpulse@test.local",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .current_dir(dir)
            .output()
            .expect("spawn git");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn test_list_add_remove_roundtrip() {
        let main = tempfile::TempDir::new().unwrap();
        git_in(main.path(), &["init", "-b", "main"]);
        std::fs::write(main.path().join("seed.txt"), "seed").unwrap();
        git_in(main.path(), &["add", "."]);
        git_in(main.path(), &["commit", "-m", "init"]);

        let listed = list_worktrees(main.path().to_str().unwrap()).expect("list main only");
        assert_eq!(listed.len(), 1);
        assert!(listed[0].is_main);
        assert_eq!(listed[0].dirty_files, Some(0));

        let parent = tempfile::TempDir::new().unwrap();
        let wt_path = parent.path().join("agent-task");
        let created = add_worktree(
            main.path().to_str().unwrap(),
            wt_path.to_str().unwrap(),
            Some("agent/task"),
            Some("main"),
            false,
        )
        .expect("add worktree");
        assert_eq!(created, wt_path.to_str().unwrap());

        let listed = list_worktrees(main.path().to_str().unwrap()).expect("list two");
        assert_eq!(listed.len(), 2);
        assert!(!listed[1].is_main);
        assert_eq!(listed[1].branch.as_deref(), Some("agent/task"));
        assert_eq!(listed[1].dirty_files, Some(0));

        remove_worktree(main.path().to_str().unwrap(), created.as_str(), false)
            .expect("remove worktree");
        let listed = list_worktrees(main.path().to_str().unwrap()).expect("list back to one");
        assert_eq!(listed.len(), 1);
    }

    #[test]
    fn test_list_worktrees_rejects_non_repo() {
        let dir = tempfile::TempDir::new().unwrap();
        assert!(list_worktrees(dir.path().to_str().unwrap()).is_err());
    }
}
