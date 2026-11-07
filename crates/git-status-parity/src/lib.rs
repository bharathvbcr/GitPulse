//! Parity oracle and cost harness for gix's in-process status.
//!
//! Both sides are reduced to the same shape, sorted `(path, index, worktree)`
//! rows, where the two status letters are the `XY` pair of
//! `git status --porcelain=v1`.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use gix::bstr::{BString, ByteSlice};
use gix::status::{Item, UntrackedFiles};

/// One status row: repository-relative path, index letter (X), worktree letter (Y).
pub type Row = (String, char, char);

/// How `git` and gix are told to behave for the measurements and the oracle:
/// no user or system config, so a `core.fsmonitor` or `core.untrackedCache` on
/// the machine cannot move either side. Process-wide; call before the first
/// repository is opened.
pub fn hermetic_environment(home: &Path) {
    std::env::set_var("GIT_CONFIG_GLOBAL", "/dev/null");
    std::env::set_var("GIT_CONFIG_NOSYSTEM", "1");
    std::env::set_var("HOME", home);
    std::env::set_var("GIT_AUTHOR_NAME", "Parity");
    std::env::set_var("GIT_AUTHOR_EMAIL", "parity@example.com");
    std::env::set_var("GIT_COMMITTER_NAME", "Parity");
    std::env::set_var("GIT_COMMITTER_EMAIL", "parity@example.com");
}

/// Runs `git` in `cwd` and returns stdout; panics with stderr on failure
/// (fixture construction only).
pub fn git(cwd: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("spawn git");
    assert!(
        output.status.success(),
        "git {args:?} failed in {}: {}",
        cwd.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// The reference: `git status --porcelain=v1 -u -z`, exactly the argv
/// GitPulse spawns, parsed into rows. A rename record is `XY to NUL from NUL`;
/// the origin is consumed and dropped.
pub fn git_status(cwd: &Path) -> Result<Vec<Row>, String> {
    let output = Command::new("git")
        .args(["status", "--porcelain=v1", "-u", "-z"])
        .current_dir(cwd)
        .output()
        .map_err(|e| format!("spawn git: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned());
    }
    Ok(parse_porcelain_z(&output.stdout))
}

/// Parses `--porcelain=v1 -z` output.
pub fn parse_porcelain_z(bytes: &[u8]) -> Vec<Row> {
    let mut rows = Vec::new();
    let mut fields = bytes.split(|b| *b == 0).filter(|f| !f.is_empty());
    while let Some(record) = fields.next() {
        if record.len() < 4 {
            continue;
        }
        let (x, y) = (record[0] as char, record[1] as char);
        let path = String::from_utf8_lossy(&record[3..]).into_owned();
        if x == 'R' || x == 'C' || y == 'R' || y == 'C' {
            fields.next();
        }
        rows.push((path, x, y));
    }
    rows.sort();
    rows
}

/// What the directory walk is asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Untracked {
    /// No walk at all.
    Off,
    /// Each untracked file, as `git status -u` lists them.
    Files,
}

/// gix's status for the repository at `cwd` (opened fresh), as rows.
pub fn gix_status_at(cwd: &Path, untracked: Untracked) -> Result<Vec<Row>, String> {
    let repo = gix::open(cwd).map_err(|e| format!("open: {e}"))?;
    gix_status(&repo, untracked)
}

/// gix's status for an already-open repository, as rows.
pub fn gix_status(repo: &gix::Repository, untracked: Untracked) -> Result<Vec<Row>, String> {
    let mode = match untracked {
        Untracked::Off => UntrackedFiles::None,
        Untracked::Files => UntrackedFiles::Files,
    };
    let platform = repo
        .status(gix::progress::Discard)
        .map_err(|e| format!("status: {e}"))?
        .untracked_files(mode)
        .index_worktree_submodules(gix::status::Submodule::AsConfigured { check_dirty: true });
    let iter = platform
        .into_iter(Vec::<BString>::new())
        .map_err(|e| format!("into_iter: {e}"))?;
    // Tracked paths merge their index and worktree letters into one row, as git
    // prints one line per path; untracked paths are separate rows, because a
    // path can be both staged-deleted and untracked and git prints both.
    let mut tracked: BTreeMap<String, (char, char)> = BTreeMap::new();
    let mut rows: Vec<Row> = Vec::new();
    for item in iter {
        let item = item.map_err(|e| format!("item: {e}"))?;
        match item {
            Item::TreeIndex(change) => {
                use gix::diff::index::Change;
                let (path, letter) = match &change {
                    Change::Addition { location, .. } => (location, 'A'),
                    Change::Deletion { location, .. } => (location, 'D'),
                    Change::Modification { location, .. } => (location, 'M'),
                    Change::Rewrite { location, copy, .. } => {
                        (location, if *copy { 'C' } else { 'R' })
                    }
                };
                tracked
                    .entry(path.to_str_lossy().into_owned())
                    .or_insert((' ', ' '))
                    .0 = letter;
            }
            Item::IndexWorktree(item) => {
                use gix::status::index_worktree::iter::Summary;
                let Some(summary) = item.summary() else {
                    continue;
                };
                let path = item.rela_path().to_str_lossy().into_owned();
                match summary {
                    Summary::Added => rows.push((path, '?', '?')),
                    other => {
                        let letter = match other {
                            Summary::Removed => 'D',
                            Summary::Modified => 'M',
                            Summary::TypeChange => 'T',
                            Summary::Conflict => 'U',
                            Summary::Renamed => 'R',
                            Summary::Copied => 'C',
                            Summary::IntentToAdd => 'A',
                            Summary::Added => unreachable!("handled above"),
                        };
                        tracked.entry(path).or_insert((' ', ' ')).1 = letter;
                    }
                }
            }
        }
    }
    rows.extend(tracked.into_iter().map(|(p, (x, y))| (p, x, y)));
    rows.sort();
    Ok(rows)
}
