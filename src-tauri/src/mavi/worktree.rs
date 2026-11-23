//! What changed in a worktree during one Mavi turn, without writing anything.
//!
//! The agent reports its own file edits (`Edit`, `Write`, `MultiEdit` with
//! exact hunks in `tool_use_result.structuredPatch`), and the page renders
//! those live. A shell command, a formatter, a code generator or the person
//! typing in an editor changes files the agent never reports. So at each turn
//! boundary the host takes a **fingerprint**: the porcelain status of every
//! changed path plus each file's size and modification time. Two fingerprints
//! name every path whose state moved during the turn.
//!
//! Deliberately not a turn checkpoint (`engine::turn_checkpoint`): a checkpoint
//! writes a commit and a hidden ref holding file contents, which the person
//! turns on with `turn_checkpoints.enabled` (off by default). A fingerprint is
//! read-only and kept in memory: no object, no ref, no file contents. A file's
//! patch is read only when the person opens it ([`file_diff`]), redacted the
//! same way the checkpoint diff is.
//!
//! **Limits, stated.** Two edits inside one filesystem timestamp tick that
//! leave the same size are one state (git's own racy-clean caveat). A
//! worktree with more than [`MAX_ENTRIES`] changed paths is fingerprinted
//! partially, and the turn says so. Another program editing the worktree
//! during the turn is indistinguishable from the agent's shell; the page calls
//! those paths "other changes during this turn", never "the agent's".

use crate::procguard::LockedSpawn;
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Component, Path};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub(crate) const MAX_ENTRIES: usize = 5_000;
const STATUS_DEADLINE: Duration = Duration::from_secs(20);
const MAX_STATUS_BYTES: usize = 4 * 1024 * 1024;
pub(crate) const MAX_DIFF_BYTES: usize = 256 * 1024;

/// One changed path's state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct State {
    pub code: String,
    pub len: Option<u64>,
    pub mtime_ns: Option<u128>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Fingerprint {
    pub paths: BTreeMap<String, State>,
    pub partial: bool,
}

/// One path that moved between two fingerprints.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct Moved {
    pub path: String,
    /// The porcelain code now (`" M"`, `"??"`, `"D "`…), or `"  "` when the
    /// path is clean again (the turn reverted it, or committed it).
    pub code: String,
}

/// Runs `program args` in `cwd` with a deadline and an output cap.
pub(crate) fn run_capped(
    program: &str,
    args: &[&str],
    cwd: &Path,
    deadline: Duration,
    max: usize,
) -> Result<(bool, Vec<u8>, bool), String> {
    let mut child = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn_locked()
        .map_err(|e| format!("Could not start {program}: {e}"))?;
    let stdout = child.stdout.take();
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let mut cut = false;
        if let Some(pipe) = stdout {
            let mut limited = pipe.take(max as u64 + 1);
            let _ = limited.read_to_end(&mut buf);
            if buf.len() > max {
                buf.truncate(max);
                cut = true;
            }
        }
        (buf, cut)
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() > deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "{program} took longer than {}s.",
                    deadline.as_secs()
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => return Err(e.to_string()),
        }
    };
    let (buf, cut) = reader.join().map_err(|_| "reader panicked".to_string())?;
    Ok((status.success(), buf, cut))
}

/// Parses `git status --porcelain=v1 -z --untracked-files=all` output into
/// `(code, path)` pairs. A rename's second (source) path is skipped.
pub(crate) fn parse_porcelain(bytes: &[u8]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut fields = bytes.split(|b| *b == 0).filter(|f| !f.is_empty());
    while let Some(field) = fields.next() {
        if field.len() < 4 {
            continue;
        }
        let code = String::from_utf8_lossy(&field[..2]).into_owned();
        let path = String::from_utf8_lossy(&field[3..]).into_owned();
        if code.starts_with('R') || code.starts_with('C') {
            fields.next();
        }
        out.push((code, path));
    }
    out
}

/// Takes a fingerprint of the worktree at `root`.
pub(crate) fn fingerprint(root: &Path) -> Result<Fingerprint, String> {
    let (ok, bytes, cut) = run_capped(
        "git",
        &[
            "--no-optional-locks",
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--ignore-submodules=dirty",
        ],
        root,
        STATUS_DEADLINE,
        MAX_STATUS_BYTES,
    )?;
    if !ok {
        return Err("git status failed here.".into());
    }
    let mut print = Fingerprint {
        partial: cut,
        ..Fingerprint::default()
    };
    for (code, path) in parse_porcelain(&bytes) {
        if print.paths.len() >= MAX_ENTRIES {
            print.partial = true;
            break;
        }
        let meta = std::fs::symlink_metadata(root.join(&path)).ok();
        let mtime_ns = meta
            .as_ref()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos());
        print.paths.insert(
            path,
            State {
                code,
                len: meta.map(|m| m.len()),
                mtime_ns,
            },
        );
    }
    Ok(print)
}

/// Every path whose state differs between `before` and `after`, sorted.
pub(crate) fn moved(before: &Fingerprint, after: &Fingerprint) -> Vec<Moved> {
    let mut out = Vec::new();
    for (path, now) in &after.paths {
        if before.paths.get(path) != Some(now) {
            out.push(Moved {
                path: path.clone(),
                code: now.code.clone(),
            });
        }
    }
    for path in before.paths.keys() {
        if !after.paths.contains_key(path) {
            out.push(Moved {
                path: path.clone(),
                code: "  ".into(),
            });
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// Whether `path` is a plain relative path inside the worktree: no root, no
/// `..`, no NUL, at most 4096 bytes.
pub(crate) fn safe_relative(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 4096
        && !path.contains('\0')
        && Path::new(path)
            .components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
}

/// The patch of one path against `HEAD` (or, for an untracked file, against
/// nothing), bounded and redacted. Returns `(patch, truncated)`.
/// Whether Git tracks `path` (in the index), so an empty diff means "unchanged".
fn tracked(root: &Path, path: &str) -> bool {
    run_capped(
        "git",
        &[
            "--no-optional-locks",
            "ls-files",
            "--error-unmatch",
            "--",
            path,
        ],
        root,
        STATUS_DEADLINE,
        4096,
    )
    .map(|(ok, _, _)| ok)
    .unwrap_or(false)
}

pub(crate) fn file_diff(root: &Path, path: &str) -> Result<(String, bool), String> {
    if !safe_relative(path) {
        return Err("That path is not inside this worktree.".into());
    }
    let flags = [
        "--no-ext-diff",
        "--no-textconv",
        "--no-color",
        "--no-renames",
    ];
    let mut args = vec!["--no-optional-locks", "diff", "HEAD"];
    args.extend(flags);
    args.extend(["--", path]);
    let (_, mut bytes, mut cut) = run_capped("git", &args, root, STATUS_DEADLINE, MAX_DIFF_BYTES)?;
    if bytes.is_empty() && root.join(path).is_file() && !tracked(root, path) {
        // Untracked: `diff --no-index` exits 1 when the files differ, which is
        // the answer, not a failure. A tracked file with no diff is unchanged
        // and must not be shown as wholly new.
        let mut args = vec!["--no-optional-locks", "diff", "--no-index"];
        args.extend(flags);
        args.extend(["--", "/dev/null", path]);
        (_, bytes, cut) = run_capped("git", &args, root, STATUS_DEADLINE, MAX_DIFF_BYTES)?;
    }
    let text = String::from_utf8_lossy(&bytes);
    Ok((dc_redact::redact_secrets(&text), cut))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(root: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .args([
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .current_dir(root)
            .status_locked()
            .unwrap()
            .success();
        assert!(ok, "git {args:?}");
    }

    #[test]
    fn porcelain_parsing_handles_renames_and_spaces() {
        let raw = b" M src/a b.rs\0R  new.rs\0old.rs\0?? x\0";
        assert_eq!(
            parse_porcelain(raw),
            vec![
                (" M".to_string(), "src/a b.rs".to_string()),
                ("R ".to_string(), "new.rs".to_string()),
                ("??".to_string(), "x".to_string()),
            ]
        );
    }

    #[test]
    fn an_unchanged_tracked_file_has_no_diff_and_an_untracked_one_is_new() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        git(root, &["init", "-q"]);
        std::fs::write(root.join("same.txt"), "a\n").unwrap();
        git(root, &["add", "."]);
        git(root, &["commit", "-qm", "init"]);
        std::fs::write(root.join("new.txt"), "n\n").unwrap();
        assert_eq!(file_diff(root, "same.txt").unwrap().0, "");
        assert!(file_diff(root, "new.txt").unwrap().0.contains("+n"));
    }

    #[test]
    fn relative_paths_cannot_escape() {
        assert!(safe_relative("src/a.rs"));
        assert!(safe_relative("./a"));
        assert!(!safe_relative("../a"));
        assert!(!safe_relative("/etc/passwd"));
        assert!(!safe_relative("a/../../b"));
        assert!(!safe_relative(""));
    }

    #[test]
    fn a_turn_names_every_path_that_moved_including_ones_made_clean() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        git(root, &["init", "-q"]);
        std::fs::write(root.join("kept.txt"), "a\n").unwrap();
        std::fs::write(root.join("reverted.txt"), "a\n").unwrap();
        git(root, &["add", "."]);
        git(root, &["commit", "-qm", "init"]);
        std::fs::write(root.join("reverted.txt"), "dirty\n").unwrap();
        std::fs::write(root.join("still.txt"), "untracked\n").unwrap();
        let before = fingerprint(root).unwrap();

        // The turn: a shell edit, a new file, and a revert.
        std::fs::write(root.join("kept.txt"), "a\nb\n").unwrap();
        std::fs::write(root.join("new.txt"), "n\n").unwrap();
        std::fs::write(root.join("reverted.txt"), "a\n").unwrap();
        let after = fingerprint(root).unwrap();

        let moved = moved(&before, &after);
        let names: Vec<_> = moved
            .iter()
            .map(|m| (m.path.as_str(), m.code.as_str()))
            .collect();
        assert_eq!(
            names,
            vec![
                ("kept.txt", " M"),
                ("new.txt", "??"),
                ("reverted.txt", "  ")
            ]
        );
        assert!(!after.partial);

        let (patch, cut) = file_diff(root, "kept.txt").unwrap();
        assert!(patch.contains("+b") && !cut);
        let (untracked, _) = file_diff(root, "new.txt").unwrap();
        assert!(untracked.contains("+n"), "{untracked}");
        assert!(file_diff(root, "../x").is_err());
    }
}
