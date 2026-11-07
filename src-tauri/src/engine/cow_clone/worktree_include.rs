//! `.worktreeinclude`: the ignored files a new worktree also gets.
//!
//! The convention is Claude Code's (<https://code.claude.com/docs/en/worktrees>,
//! "Copy gitignored files into worktrees"), also honoured by Worktrunk: a file
//! at the repository root, in `.gitignore` syntax, naming local files a fresh
//! checkout lacks. "Only files that match a pattern and are also gitignored are
//! copied, so tracked files are never duplicated." Claude Code stops processing
//! the file once a `WorktreeCreate` hook is configured, so GitPulse's hook has
//! to; and so that a worktree is the same whoever made it, the app's
//! add-worktree path, the MCP server and task lanes get it from the same place:
//! [`super::clone_ignored_caches`], which every one of them calls.
//!
//! # What is copied
//!
//! A file is copied only when all of these hold:
//!
//! * it is untracked in the anchor checkout and matches a `.worktreeinclude`
//!   pattern (`git ls-files --others --ignored --exclude-from`);
//! * the anchor's own ignore rules ignore it (`git ls-files --others --ignored
//!   --exclude-standard --directory`, so negations and ignored parent
//!   directories are git's answer, not a re-implementation);
//! * the new worktree's ignore rules ignore it too (`git check-ignore`), so a
//!   copy never shows up there as an untracked file;
//! * it is a regular file (a link is skipped, never followed), it sits under no
//!   cache directory the cache copy already owns, and nothing is at its path in
//!   the worktree yet (an existing file is never overwritten);
//! * no directory between the worktree root and it is a symbolic link, so a
//!   committed link cannot carry the copy out of the worktree.
//!
//! Each file is reflinked where the filesystem allows (APFS `clonefile`, Linux
//! `FICLONE`) and byte-copied otherwise, whole or not at all
//! ([`super::reflink_copy_file`]). At most [`MAX_INCLUDE_FILES`] files: past
//! that nothing is copied and the failure says why, because a list that large
//! is a dependency tree, and those belong to the cache copy.
//!
//! # Secrets
//!
//! This can copy secrets-bearing files: `.env`, `.env.local`, `*.pem`,
//! private keys, cloud credentials files, anything the repository's owner
//! listed and ignored. That is the point of the convention (the documented
//! example is `.env`), and it is acceptable because:
//!
//! * nothing leaves the machine or the user: the copy goes from one of the
//!   user's checkouts to another, on the same volume, with the source's
//!   permissions (a reflink or `fs::copy` keeps the mode bits);
//! * the copy stays ignored in the worktree (checked above), so it cannot be
//!   committed by accident any more easily than the original;
//! * only what the repository lists is copied, never a guess: no
//!   `.worktreeinclude`, no copy;
//! * an agent running in the worktree could already read the original through
//!   the main checkout, which shares the user's account.
//!
//! What is copied is logged by path, never by content.

use std::collections::HashSet;
use std::path::{Component, Path};

use super::{CacheReport, CacheSelection, STANDARD_CACHE_DIRS, VIRTUALENV_DIRS};
use crate::sysload::{Reading, LOW_DISK_FLOOR};

/// The file, at the anchor checkout's root.
pub const WORKTREE_INCLUDE_FILE: &str = ".worktreeinclude";

/// Most files one `.worktreeinclude` may copy. Local configuration is a handful
/// of files; a list past this is a dependency tree.
pub const MAX_INCLUDE_FILES: usize = 2_000;

/// The name a failure or a low-disk skip is reported under.
const REPORT_NAME: &str = WORKTREE_INCLUDE_FILE;

/// Copies the files the anchor's `.worktreeinclude` names into `worktree`,
/// recording what was copied in `report.included`, what failed in
/// `report.failures`, and a skip for want of disk in `skipped`.
///
/// No `.worktreeinclude` is no work and no report.
pub(super) fn copy_included(
    anchor: &Path,
    worktree: &Path,
    selection: &CacheSelection,
    report: &mut CacheReport,
    skipped: &mut Vec<String>,
) {
    match plan(anchor, worktree) {
        Ok(None) => {}
        Ok(Some(paths)) if paths.is_empty() => {}
        Ok(Some(paths)) => {
            if let Reading::Value(free) = super::read_free(selection.free_space, worktree) {
                if free < LOW_DISK_FLOOR {
                    skipped.push(REPORT_NAME.to_string());
                    return;
                }
            }
            copy_planned(anchor, worktree, &paths, selection, report);
        }
        Err(error) => report.failures.push(format!("{REPORT_NAME}: {error}")),
    }
}

/// The anchor-relative paths to copy, `None` when there is no
/// `.worktreeinclude`. Copies nothing.
fn plan(anchor: &Path, worktree: &Path) -> Result<Option<Vec<String>>, String> {
    let include = anchor.join(WORKTREE_INCLUDE_FILE);
    let meta = match include.symlink_metadata() {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("could not read {}: {e}", include.display())),
    };
    if !meta.file_type().is_file() {
        return Err(format!(
            "{} is not a regular file, so it was not read",
            include.display()
        ));
    }
    let include_text = include
        .to_str()
        .ok_or("the .worktreeinclude path is not UTF-8")?;
    let exclude_from = format!("--exclude-from={include_text}");
    let listed = nul_list(git(
        anchor,
        &[
            "ls-files",
            "-z",
            "--others",
            "--ignored",
            exclude_from.as_str(),
        ],
    )?);
    let listed: Vec<String> = listed
        .into_iter()
        .filter(|path| safe_relative(path) && !owned_by_cache_copy(path))
        .collect();
    if listed.is_empty() {
        return Ok(Some(Vec::new()));
    }
    if listed.len() > MAX_INCLUDE_FILES {
        return Err(format!(
            "it matches {} files, more than the {MAX_INCLUDE_FILES} it may copy, so none were copied; \
             list configuration files there, and leave dependency directories to the cache copy",
            listed.len()
        ));
    }
    let ignored_in_anchor = ignored_set(anchor)?;
    let candidates: Vec<String> = listed
        .into_iter()
        .filter(|path| ignored_in_anchor.covers(path))
        .collect();
    let ignored_in_worktree = ignored_among(worktree, &candidates)?;
    Ok(Some(
        candidates
            .into_iter()
            .filter(|path| ignored_in_worktree.contains(path))
            .collect(),
    ))
}

fn copy_planned(
    anchor: &Path,
    worktree: &Path,
    paths: &[String],
    selection: &CacheSelection,
    report: &mut CacheReport,
) {
    let mut byte_copied: u64 = 0;
    for relative in paths {
        let source = anchor.join(relative);
        let meta = match source.symlink_metadata() {
            Ok(meta) => meta,
            // Gone since it was listed: nothing to copy, nothing wrong.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                report
                    .failures
                    .push(format!("{REPORT_NAME}: could not read {relative}: {e}"));
                continue;
            }
        };
        if !meta.file_type().is_file() {
            log::info!("{REPORT_NAME}: not copying {relative}: it is not a regular file");
            continue;
        }
        let destination = worktree.join(relative);
        if destination.symlink_metadata().is_ok() {
            continue;
        }
        if byte_copied.saturating_add(meta.len()) > selection.caps.max_bytes {
            report.failures.push(format!(
                "{REPORT_NAME}: {relative} would take the copy past {} bytes, so it and the files after it were not copied",
                selection.caps.max_bytes
            ));
            break;
        }
        if let Err(error) = make_parents(worktree, relative) {
            report
                .failures
                .push(format!("{REPORT_NAME}: {relative}: {error}"));
            continue;
        }
        match super::reflink_copy_file(&source, &destination) {
            Ok(copied) => {
                if !copied.is_cow {
                    byte_copied = byte_copied.saturating_add(copied.bytes_cloned);
                }
                report.included.push(relative.clone());
            }
            Err(error) => report.failures.push(format!(
                "{REPORT_NAME}: {relative}: {}",
                error.into_message()
            )),
        }
    }
    if !report.included.is_empty() {
        log::info!(
            "copied {} file(s) named by {REPORT_NAME} into {}: {}",
            report.included.len(),
            worktree.display(),
            report.included.join(", ")
        );
    }
}

/// Creates the directories between `root` and `relative`'s file, refusing any
/// that already exists as something other than a real directory.
fn make_parents(root: &Path, relative: &str) -> Result<(), String> {
    let Some(parent) = Path::new(relative).parent() else {
        return Ok(());
    };
    let mut at = root.to_path_buf();
    for component in parent.components() {
        let Component::Normal(name) = component else {
            return Err("its path is not a plain relative path".into());
        };
        at.push(name);
        match at.symlink_metadata() {
            Ok(meta) if meta.file_type().is_dir() => {}
            Ok(_) => {
                return Err(format!(
                    "{} is not a directory (a link is never followed), so it was not copied",
                    at.display()
                ))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir(&at)
                    .map_err(|e| format!("could not create {}: {e}", at.display()))?;
            }
            Err(e) => return Err(format!("could not read {}: {e}", at.display())),
        }
    }
    Ok(())
}

/// A path git listed that is safe to join under a root: relative, with no
/// `..`, root or prefix component.
fn safe_relative(path: &str) -> bool {
    !path.is_empty()
        && Path::new(path)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
}

/// Whether the path lies under a directory the cache copy clones whole (or,
/// for virtualenvs, deliberately does not: they embed their own absolute path).
fn owned_by_cache_copy(path: &str) -> bool {
    let first = path.split('/').next().unwrap_or_default();
    STANDARD_CACHE_DIRS.contains(&first) || VIRTUALENV_DIRS.contains(&first)
}

/// The anchor's ignored, untracked entries: files, and wholly ignored
/// directories (with their trailing `/`).
struct IgnoredSet {
    files: HashSet<String>,
    dirs: Vec<String>,
}

impl IgnoredSet {
    fn covers(&self, path: &str) -> bool {
        self.files.contains(path) || self.dirs.iter().any(|dir| path.starts_with(dir.as_str()))
    }
}

fn ignored_set(anchor: &Path) -> Result<IgnoredSet, String> {
    let entries = nul_list(git(
        anchor,
        &[
            "ls-files",
            "-z",
            "--others",
            "--ignored",
            "--exclude-standard",
            "--directory",
        ],
    )?);
    let (dirs, files): (Vec<String>, Vec<String>) =
        entries.into_iter().partition(|entry| entry.ends_with('/'));
    Ok(IgnoredSet {
        files: files.into_iter().collect(),
        dirs,
    })
}

/// Which of `paths` the worktree's ignore rules ignore. A `git check-ignore`
/// that gives no answer (anything but 0 or 1) is an error, never "none".
fn ignored_among(worktree: &Path, paths: &[String]) -> Result<HashSet<String>, String> {
    if paths.is_empty() {
        return Ok(HashSet::new());
    }
    let mut stdin = Vec::with_capacity(paths.iter().map(|p| p.len() + 1).sum());
    for path in paths {
        stdin.extend_from_slice(path.as_bytes());
        stdin.push(0);
    }
    let run = crate::engine::git_cli::git_captured_with_stdin(
        worktree,
        &["check-ignore", "-z", "--stdin"],
        &stdin,
    )
    .map_err(|e| e.into_message())?;
    if let Some(reason) = &run.incomplete {
        return Err(format!(
            "git check-ignore's answer was cut short ({}), so nothing was copied",
            reason.describe()
        ));
    }
    match run.status_code {
        0 => Ok(nul_list(run.stdout).into_iter().collect()),
        1 => Ok(HashSet::new()),
        code => Err(format!(
            "git check-ignore could not say which listed files the new worktree ignores (exit {code}): {}",
            String::from_utf8_lossy(&run.stderr).trim()
        )),
    }
}

fn git(repo: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    crate::engine::git_cli::git(repo, args).map_err(|e| e.into_message())
}

fn nul_list(bytes: Vec<u8>) -> Vec<String> {
    bytes
        .split(|b| *b == 0)
        .filter(|entry| !entry.is_empty())
        .filter_map(|entry| std::str::from_utf8(entry).ok().map(str::to_string))
        .collect()
}

#[cfg(test)]
mod tests;
