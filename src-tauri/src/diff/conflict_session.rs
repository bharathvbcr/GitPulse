//! Conflict writes own a source snapshot, a private index and Git's index lock.
//! Ordinary editor writes must not be used for this transaction: choosing text
//! and staging a later filesystem read can otherwise stage somebody else's edit.

use super::conflict::{ConflictDocument, ConflictResolutionChoice, ConflictResolver, FileSegment};
use crate::engine::git_cli::{
    git, git_text, git_with_index, git_with_stdin, resolve_git_dir, sandbox_join, validate_repo,
};
use crate::engine::git_writer::repo_mutation_lock;
use crate::ipc_error::CommandError;
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const MAX_BYTES: usize = 16 * 1024 * 1024;
const MAX_INDEX_BYTES: usize = 64 * 1024 * 1024;
static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConflictStage {
    pub stage: u8,
    pub mode: String,
    pub oid: String,
    pub size: u64,
    pub text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConflictSnapshot {
    pub file_path: String,
    pub revision: String,
    pub operation: String,
    pub document: Option<ConflictDocument>,
    pub stages: Vec<ConflictStage>,
    pub reason: Option<String>,
    pub worktree_mode: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ConflictFileChoice {
    Chunks(Vec<ConflictResolutionChoice>),
    Ours,
    Theirs,
    WorkingTree,
    StageOnly,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConflictSaveRequest {
    pub file_path: String,
    pub revision: String,
    pub choice: ConflictFileChoice,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConflictSaveOutcome {
    pub written: bool,
    pub staged: bool,
    pub message: String,
    pub recovery_path: Option<String>,
    pub snapshot: Option<ConflictSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Worktree {
    pub bytes: Option<Vec<u8>>,
    pub mode: String,
}

fn bounded_read(path: &Path, cap: usize) -> Result<Vec<u8>, String> {
    use crate::fs_entry::BoundedRead;
    crate::fs_entry::read_bounded_nofollow(path, cap).map_err(|error| match error {
        BoundedRead::Io(e) => format!("Cannot read {}: {e}", path.display()),
        BoundedRead::NotRegular | BoundedRead::TooLarge => format!(
            "{} is not a regular file within the {} MiB limit",
            path.display(),
            cap / 1024 / 1024
        ),
        BoundedRead::Grew => "File grew beyond the resolution size limit; reload".into(),
    })
}

/// Reject metadata paths and symlink ancestors, even links back into the repo.
/// The final entry may be a Git symlink; it is read/replaced as a link, never followed.
fn conflict_path(repo: &Path, relative: &str) -> Result<PathBuf, String> {
    let dest = sandbox_join(repo, relative)?;
    let mut current = repo.to_path_buf();
    let components: Vec<_> = Path::new(relative).components().collect();
    for (index, component) in components.iter().enumerate() {
        let Component::Normal(name) = component else {
            return Err("Conflict paths must be normalized relative paths".into());
        };
        if name.to_string_lossy().eq_ignore_ascii_case(".git") {
            return Err("Git metadata cannot be resolved as a file".into());
        }
        current.push(name);
        if index + 1 < components.len() {
            let meta = fs::symlink_metadata(&current)
                .map_err(|e| format!("Cannot access conflict parent: {e}"))?;
            if !meta.is_dir() || meta.file_type().is_symlink() {
                return Err("Conflict parent must be an existing real directory".into());
            }
        }
    }
    Ok(dest)
}

#[cfg(any(unix, windows))]
fn read_worktree(dest: &Path) -> Result<Worktree, String> {
    super::conflict_fs::read(dest)
}

#[cfg(not(any(unix, windows)))]
fn read_worktree(dest: &Path) -> Result<Worktree, String> {
    let meta = match fs::symlink_metadata(dest) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Worktree {
                bytes: None,
                mode: "missing".into(),
            })
        }
        Err(e) => return Err(e.to_string()),
    };
    if meta.file_type().is_symlink() {
        let target = fs::read_link(dest).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        let bytes = {
            use std::os::unix::ffi::OsStrExt;
            target.as_os_str().as_bytes().to_vec()
        };
        #[cfg(not(unix))]
        let bytes = target
            .to_str()
            .ok_or("Non-UTF-8 symlink target is unsupported on this platform")?
            .as_bytes()
            .to_vec();
        return Ok(Worktree {
            bytes: Some(bytes),
            mode: "120000".into(),
        });
    }
    if meta.is_dir() {
        return Ok(Worktree {
            bytes: None,
            mode: "160000".into(),
        });
    }
    let mode = {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if meta.permissions().mode() & 0o111 != 0 {
                "100755"
            } else {
                "100644"
            }
        }
        #[cfg(not(unix))]
        {
            "100644"
        }
    };
    Ok(Worktree {
        bytes: Some(bounded_read(dest, MAX_BYTES)?),
        mode: mode.into(),
    })
}

fn hash(repo: &Path, bytes: &[u8]) -> Result<String, CommandError> {
    String::from_utf8(git_with_stdin(
        repo,
        &["hash-object", "--stdin", "--no-filters"],
        bytes,
    )?)
    .map(|text| text.trim().to_owned())
    .map_err(|e| e.to_string())
    .map_err(CommandError::from)
}

fn operation(repo: &Path) -> Result<String, CommandError> {
    let dir = resolve_git_dir(repo)?;
    let mut parts = vec![git(repo, &["rev-parse", "--verify", "HEAD"])?];
    // Files that identify a parked step. Optional absence is data; a read that
    // failed is not absence. Linked worktrees use their own resolved Git dir.
    for name in [
        "HEAD",
        "MERGE_HEAD",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
        "REBASE_HEAD",
        "rebase-merge/head-name",
        "rebase-merge/onto",
        "rebase-merge/msgnum",
        "rebase-apply/next",
        "rebase-apply/onto",
        "sequencer/head",
        "sequencer/todo",
    ] {
        let path = dir.join(name);
        match fs::symlink_metadata(&path) {
            Ok(meta) => {
                let bytes = bounded_read(&path, 1024 * 1024)?;
                let modified = meta
                    .modified()
                    .map_err(|e| e.to_string())?
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_err(|e| e.to_string())?
                    .as_nanos();
                #[cfg(unix)]
                let identity = {
                    use std::os::unix::fs::MetadataExt;
                    (meta.dev(), meta.ino(), meta.ctime(), meta.ctime_nsec())
                };
                #[cfg(not(unix))]
                let identity = (0u64, 0u64, 0i64, 0i64);
                parts.push(
                    serde_json::to_vec(&(bytes, modified, identity)).map_err(|e| e.to_string())?,
                );
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => parts.push(Vec::new()),
            Err(e) => return Err(e.to_string().into()),
        }
    }
    hash(
        repo,
        &serde_json::to_vec(&parts).map_err(|e| e.to_string())?,
    )
}

fn index_stages(repo: &Path, file: &str) -> Result<Vec<ConflictStage>, CommandError> {
    let raw = git(
        repo,
        &[
            "ls-files",
            "--stage",
            "-z",
            "--",
            &format!(":(literal){file}"),
        ],
    )?;
    let mut stages = Vec::new();
    for record in raw
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
    {
        let tab = record
            .iter()
            .position(|byte| *byte == b'\t')
            .ok_or("Invalid Git index record")?;
        if &record[tab + 1..] != file.as_bytes() {
            return Err("Index returned a different path".into());
        }
        let fields: Vec<_> = std::str::from_utf8(&record[..tab])
            .map_err(|e| e.to_string())?
            .split(' ')
            .collect();
        if fields.len() != 3 {
            return Err("Invalid index fields".into());
        }
        let stage: u8 = fields[2].parse().map_err(|_| "Invalid index stage")?;
        if stage == 0 {
            return Err(
                "This file is already staged or is no longer conflicted; refresh the repository"
                    .into(),
            );
        }
        if !(1..=3).contains(&stage)
            || stages
                .iter()
                .any(|value: &ConflictStage| value.stage == stage)
        {
            return Err("Invalid or duplicate conflict stage".into());
        }
        stages.push(ConflictStage {
            mode: fields[0].into(),
            oid: fields[1].into(),
            stage,
            size: 0,
            text: None,
        });
    }
    if stages.is_empty() {
        return Err("This file is no longer conflicted; refresh the repository".into());
    }
    Ok(stages)
}

fn revision(
    repo: &Path,
    operation: &str,
    stages: &[ConflictStage],
    worktree: &Worktree,
) -> Result<String, CommandError> {
    let stages: Vec<_> = stages
        .iter()
        .map(|stage| (&stage.mode, &stage.oid, stage.stage))
        .collect();
    let blob = worktree
        .bytes
        .as_ref()
        .map(|bytes| hash(repo, bytes))
        .transpose()?;
    hash(
        repo,
        &serde_json::to_vec(&(
            repo.to_string_lossy(),
            operation,
            stages,
            &worktree.mode,
            blob,
        ))
        .map_err(|e| e.to_string())?,
    )
}

fn snapshot_inner(repo: &Path, file: &str) -> Result<(ConflictSnapshot, Worktree), CommandError> {
    let dest = conflict_path(repo, file)?;
    let before = operation(repo)?;
    let mut stages = index_stages(repo, file)?;
    let worktree = read_worktree(&dest)?;
    let mut reason = None;
    let marker_size = conflict_marker_size(repo, file)?;
    let mut document = match (&worktree.bytes, worktree.mode.as_str()) {
        (Some(bytes), "100644" | "100755") if !bytes.contains(&0) => {
            match std::str::from_utf8(bytes) {
                Ok(text) => {
                    match ConflictResolver::parse_checked_with_marker_size(file, text, marker_size)
                    {
                        Ok(doc) => Some(doc),
                        Err(error) => {
                            reason = Some(error.into());
                            None
                        }
                    }
                }
                Err(_) => {
                    reason = Some("This file is not valid UTF-8. Choose a complete side to preserve its bytes.".into());
                    None
                }
            }
        }
        _ => {
            reason = Some(match worktree.mode.as_str() {
            "missing" => "The working file is deleted. Choose a side; a missing side keeps the deletion.",
            "120000" => "This conflict contains a symbolic link. Whole-file choices preserve the link itself.",
            "160000" => "This is a submodule or directory conflict. A gitlink choice updates only the recorded commit, preserving the submodule working directory.",
            _ => "This file is binary. Whole-file choices preserve its bytes.",
        }.into());
            None
        }
    };
    if !posix_worktree() && link_limitation(&ConflictFileChoice::WorkingTree, &stages).is_err() {
        document = None;
        reason = Some(LINK_LIMITATION.into());
    }
    let identity = revision(repo, &before, &stages, &worktree)?;
    for stage in &mut stages {
        if stage.mode == "160000" {
            continue;
        }
        stage.size = git_text(repo, &["cat-file", "-s", &stage.oid])?
            .trim()
            .parse()
            .map_err(|_| "Invalid Git object size")?;
        if stage.size <= 128 * 1024 {
            let bytes = git(repo, &["cat-file", "blob", &stage.oid])?;
            if !bytes.contains(&0) {
                stage.text = String::from_utf8(bytes).ok();
            }
        }
    }
    if operation(repo)? != before
        || revision(
            repo,
            &before,
            &index_stages(repo, file)?,
            &read_worktree(&dest)?,
        )? != identity
    {
        return Err("File or operation changed while loading; reload the conflict".into());
    }
    Ok((
        ConflictSnapshot {
            file_path: file.into(),
            revision: identity,
            operation: before,
            document,
            stages,
            reason,
            worktree_mode: worktree.mode.clone(),
        },
        worktree,
    ))
}

fn conflict_marker_size(repo: &Path, file: &str) -> Result<usize, CommandError> {
    let raw = git(
        repo,
        &["check-attr", "-z", "conflict-marker-size", "--", file],
    )?;
    let fields: Vec<_> = raw.split(|byte| *byte == 0).collect();
    if fields.len() != 4 || fields[0] != file.as_bytes() || fields[1] != b"conflict-marker-size" {
        return Err("Could not determine the repository's conflict marker width".into());
    }
    let value = std::str::from_utf8(fields[2]).map_err(|e| e.to_string())?;
    if matches!(value, "unspecified" | "unset" | "set") {
        return Ok(7);
    }
    value
        .parse()
        .map_err(|_| "Invalid conflict-marker-size attribute".into())
}

pub fn snapshot(repo_path: &str, file: &str) -> Result<ConflictSnapshot, CommandError> {
    snapshot_inner(&validate_repo(repo_path)?, file).map(|(snapshot, _)| snapshot)
}

struct TempFile {
    path: PathBuf,
    keep: bool,
}
impl Drop for TempFile {
    fn drop(&mut self) {
        if !self.keep {
            if let Err(error) = fs::remove_file(&self.path) {
                if error.kind() != std::io::ErrorKind::NotFound {
                    log::warn!("Could not remove conflict transaction file: {error}");
                }
            }
        }
    }
}

fn create_temp(parent: &Path, suffix: &str) -> Result<(TempFile, File), String> {
    for _ in 0..32 {
        let path = parent.join(format!(
            ".gitpulse-conflict-{}-{}-{suffix}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        let mut options = OpenOptions::new();
        options.write(true).read(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&path) {
            Ok(file) => return Ok((TempFile { path, keep: false }, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.to_string()),
        }
    }
    Err("Could not allocate a conflict transaction file".into())
}

struct IndexTransaction {
    lock: IndexLock,
    index: PathBuf,
    original: Vec<u8>,
    private: TempFile,
}

struct IndexLock {
    path: PathBuf,
    file: File,
    keep: bool,
}

/// The gate receives the same argv that the subprocess executes. The private
/// index is an internally derived environment override, never client input.
fn mutation(
    repo: &Path,
    index: Option<&Path>,
    args: &[&str],
    input: &[u8],
    judge: &mut impl FnMut(&[&str]) -> Result<(), CommandError>,
) -> Result<Vec<u8>, CommandError> {
    let argv: Vec<_> = std::iter::once("git").chain(args.iter().copied()).collect();
    judge(&argv)?;
    match index {
        Some(index) => git_with_index(repo, index, &argv[1..], input),
        None => git_with_stdin(repo, &argv[1..], input),
    }
}
impl IndexLock {
    fn owns(&self) -> Result<bool, String> {
        let owned = self.file.metadata().map_err(|error| error.to_string())?;
        let named = match fs::symlink_metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error.to_string()),
        };
        if !named.is_file() {
            return Ok(false);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            Ok((owned.dev(), owned.ino()) == (named.dev(), named.ino()))
        }
        #[cfg(not(unix))]
        {
            Ok(owned.created().map_err(|error| error.to_string())?
                == named.created().map_err(|error| error.to_string())?
                && owned.len() == named.len())
        }
    }
}
impl IndexTransaction {
    fn begin(repo: &Path) -> Result<Self, CommandError> {
        let dir = resolve_git_dir(repo)?;
        let index = dir.join("index");
        let path = dir.join("index.lock");
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let lock_file = options
            .open(&path)
            .map_err(|e| format!("Cannot lock the Git index; no file was written: {e}"))?;
        let lock = IndexLock {
            path,
            file: lock_file,
            keep: false,
        };
        let original = bounded_read(&index, MAX_INDEX_BYTES)?;
        lock.file
            .set_permissions(
                fs::metadata(&index)
                    .map_err(|e| e.to_string())?
                    .permissions(),
            )
            .map_err(|e| e.to_string())?;
        let (private, mut file) = create_temp(&dir, "index")?;
        file.write_all(&original)
            .and_then(|_| file.sync_all())
            .map_err(|e| e.to_string())?;
        Ok(Self {
            lock,
            index,
            original,
            private,
        })
    }
    fn prepare(
        &self,
        repo: &Path,
        file: &str,
        mode: &str,
        oid: Option<&str>,
        judge: &mut impl FnMut(&[&str]) -> Result<(), CommandError>,
    ) -> Result<(), CommandError> {
        let width = match oid {
            Some(oid) => oid.len(),
            None => git_text(repo, &["rev-parse", "--verify", "HEAD"])?
                .trim()
                .len(),
        };
        if !matches!(width, 40 | 64) {
            return Err("Unsupported Git object identity width".into());
        }
        let zero = "0".repeat(width);
        let mut input = format!("0 {zero}\t{file}\0");
        if let Some(oid) = oid {
            input.push_str(&format!("{mode} {oid}\t{file}\0"));
        }
        mutation(
            repo,
            Some(&self.private.path),
            &["update-index", "-z", "--index-info"],
            input.as_bytes(),
            judge,
        )?;
        // Check the actual blob produced by clean filters, before any working
        // file is replaced. The check is restricted to this literal path.
        git_with_index(repo, &self.private.path, &[
            "-c", "core.whitespace=-blank-at-eol,-blank-at-eof,-space-before-tab,-indent-with-non-tab,-tab-in-indent,cr-at-eol",
            "diff", "--cached", "--check", "--no-ext-diff", "--no-color", "--", &format!(":(literal){file}"),
        ], &[]).map_err(|error| error.wrap(|error| format!("The prepared Git object failed the staged integrity check; no file was written. {error}")))?;
        Ok(())
    }
    fn publish(&mut self) -> Result<(), String> {
        if !self.lock.owns()? {
            return Err("Cannot publish: Git index lock ownership changed; staging refused".into());
        }
        if bounded_read(&self.index, MAX_INDEX_BYTES)? != self.original {
            return Err("The Git index changed despite the lock; resolution was not staged".into());
        }
        let bytes = bounded_read(&self.private.path, MAX_INDEX_BYTES)?;
        self.lock
            .file
            .write_all(&bytes)
            .and_then(|_| self.lock.file.sync_all())
            .map_err(|e| e.to_string())?;
        if !self.lock.owns()? || bounded_read(&self.lock.path, MAX_INDEX_BYTES)? != bytes {
            return Err(
                "Cannot publish: Git index lock changed during preparation; staging refused".into(),
            );
        }
        fs::rename(&self.lock.path, &self.index)
            .map_err(|e| format!("Cannot publish the resolved Git index: {e}"))?;
        self.lock.keep = true; // The lock path was consumed, never remove another process's lock.
        Ok(())
    }
}

impl Drop for IndexLock {
    fn drop(&mut self) {
        // A user or external process may have removed and recreated the lock.
        // Our cleanup must never remove that process's replacement.
        if !self.keep {
            match self.owns() {
                Ok(true) => {
                    if let Err(error) = fs::remove_file(&self.path) {
                        if error.kind() != std::io::ErrorKind::NotFound {
                            log::warn!("Could not remove the owned Git index lock: {error}");
                        }
                    }
                }
                Ok(false) => {}
                Err(error) => {
                    log::warn!("Could not verify Git index lock ownership during cleanup: {error}");
                }
            }
        }
    }
}

/// Whether the working tree can express what Git records in a file mode: the
/// executable bit and symbolic links. Windows cannot, so there the working file
/// is always a plain `100644` and the index is the only place those live.
fn posix_worktree() -> bool {
    #[cfg(test)]
    if SIMULATE_WINDOWS_WORKTREE.with(std::cell::Cell::get) {
        return false;
    }
    cfg!(unix)
}

#[cfg(test)]
thread_local! {
    /// Lets a Unix test drive the Windows rules: its working files are plain
    /// `100644` and cannot hold a link, whatever the host filesystem allows.
    static SIMULATE_WINDOWS_WORKTREE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

const LINK_LIMITATION: &str =
    "This conflict involves a symbolic link, which GitPulse cannot write into a Windows working tree. Choose a side that is not a link, or resolve and stage the link externally.";

/// The regular-file mode Git would record for a merge of the two sides: the
/// side that changed it from the base wins, and ours wins when both changed it
/// (or there is no base). A side that is absent or not a regular file abstains.
fn merged_regular_mode(stages: &[ConflictStage]) -> Option<&str> {
    let mode_of = |number: u8| {
        stages
            .iter()
            .find(|stage| stage.stage == number)
            .map(|stage| stage.mode.as_str())
            .filter(|mode| matches!(*mode, "100644" | "100755"))
    };
    match (mode_of(2), mode_of(3)) {
        (Some(ours), Some(theirs)) if ours != theirs => match mode_of(1) {
            Some(base) if base == ours => Some(theirs),
            _ => Some(ours),
        },
        (Some(mode), _) | (None, Some(mode)) => Some(mode),
        (None, None) => None,
    }
}

/// A symbolic link cannot be written by a platform whose working tree cannot
/// hold one. Refuse before any object is written, rather than stage a plain
/// file in the link's place.
fn link_limitation(choice: &ConflictFileChoice, stages: &[ConflictStage]) -> Result<(), String> {
    if posix_worktree() {
        return Ok(());
    }
    let link = |number: u8| {
        stages
            .iter()
            .any(|stage| stage.stage == number && stage.mode == "120000")
    };
    let blocked = match choice {
        ConflictFileChoice::Ours => link(2),
        ConflictFileChoice::Theirs => link(3),
        _ => link(2) || link(3),
    };
    if blocked {
        return Err(LINK_LIMITATION.into());
    }
    Ok(())
}

/// The one owner of the mode written to the index for a resolution. Ours and
/// Theirs already carry their stage's mode in `next`. Chunks, WorkingTree and
/// StageOnly take it from the working file, which on Windows always reads as
/// `100644`; there the executable bit comes from the index stages instead.
fn staged_mode(
    choice: &ConflictFileChoice,
    stages: &[ConflictStage],
    source: &Worktree,
    next: &Worktree,
) -> String {
    match choice {
        ConflictFileChoice::Ours | ConflictFileChoice::Theirs if source.mode == "160000" => {
            "160000".into()
        }
        ConflictFileChoice::Ours | ConflictFileChoice::Theirs => next.mode.clone(),
        _ if !posix_worktree() && matches!(next.mode.as_str(), "100644" | "100755") => {
            merged_regular_mode(stages).unwrap_or(&next.mode).into()
        }
        _ => next.mode.clone(),
    }
}

fn desired(
    repo: &Path,
    snap: &ConflictSnapshot,
    source: &Worktree,
    choice: &ConflictFileChoice,
    judge: &mut impl FnMut(&[&str]) -> Result<(), CommandError>,
) -> Result<(Worktree, Option<String>, bool), CommandError> {
    match choice {
        ConflictFileChoice::Ours | ConflictFileChoice::Theirs => {
            let side = if matches!(choice, ConflictFileChoice::Ours) {
                2
            } else {
                3
            };
            let Some(stage) = snap.stages.iter().find(|stage| stage.stage == side) else {
                if source.mode == "160000" {
                    return Err(
                        "Remove this submodule or directory externally, then refresh".into(),
                    );
                }
                return Ok((
                    Worktree {
                        bytes: None,
                        mode: "missing".into(),
                    },
                    None,
                    true,
                ));
            };
            if stage.mode == "160000" {
                if source.mode != "160000" {
                    return Err("This file/directory conflict needs an external checkout before the gitlink can be staged".into());
                }
                return Ok((source.clone(), Some(stage.oid.clone()), false));
            }
            if !matches!(stage.mode.as_str(), "100644" | "100755" | "120000")
                || stage.size > MAX_BYTES as u64
            {
                return Err("This side exceeds the 16 MiB limit or uses an unsupported Git mode; resolve it externally".into());
            }
            if source.mode == "160000" {
                return Err(
                    "A directory occupies this path; move it externally before choosing a file"
                        .into(),
                );
            }
            let bytes = if stage.mode == "120000" {
                git(repo, &["cat-file", "blob", &stage.oid])?
            } else {
                git(
                    repo,
                    &[
                        "cat-file",
                        "--filters",
                        &format!("--path={}", snap.file_path),
                        &stage.oid,
                    ],
                )?
            };
            if bytes.len() > MAX_BYTES {
                return Err("Filtered content exceeds the 16 MiB resolution limit".into());
            }
            Ok((
                Worktree {
                    bytes: Some(bytes),
                    mode: stage.mode.clone(),
                },
                Some(stage.oid.clone()),
                true,
            ))
        }
        ConflictFileChoice::Chunks(choices) => {
            let mut doc = snap
                .document
                .clone()
                .ok_or("This file cannot be resolved as UTF-8 text")?;
            if doc.total_conflicts == 0 || choices.len() != doc.total_conflicts {
                return Err("Resolution choices do not match the current conflict blocks".into());
            }
            for segment in &mut doc.segments {
                if let FileSegment::Conflict(chunk) = segment {
                    chunk.resolution = choices[chunk.chunk_index].clone();
                }
            }
            let bytes = ConflictResolver::render_resolved(&doc)?.into_bytes();
            let oid = String::from_utf8(mutation(
                repo,
                None,
                &[
                    "hash-object",
                    "-w",
                    &format!("--path={}", snap.file_path),
                    "--stdin",
                ],
                &bytes,
                judge,
            )?)
            .map_err(|e| e.to_string())?
            .trim()
            .to_owned();
            Ok((
                Worktree {
                    bytes: Some(bytes),
                    mode: source.mode.clone(),
                },
                Some(oid),
                true,
            ))
        }
        ConflictFileChoice::WorkingTree | ConflictFileChoice::StageOnly => {
            if source.mode == "missing" {
                return Ok((source.clone(), None, false));
            }
            if !matches!(source.mode.as_str(), "100644" | "100755" | "120000") {
                return Err("Choose a complete side for a submodule conflict".into());
            }
            let bytes = source.bytes.as_ref().ok_or("Missing working content")?;
            if source.mode != "120000" && !bytes.contains(&0) {
                if let Ok(text) = std::str::from_utf8(bytes) {
                    ConflictResolver::validate_marker_free(
                        text,
                        conflict_marker_size(repo, &snap.file_path)?,
                    )?;
                }
            }
            let filter = if source.mode == "120000" {
                "--no-filters".into()
            } else {
                format!("--path={}", snap.file_path)
            };
            let oid = String::from_utf8(mutation(
                repo,
                None,
                &["hash-object", "-w", &filter, "--stdin"],
                bytes,
                judge,
            )?)
            .map_err(|e| e.to_string())?
            .trim()
            .to_owned();
            Ok((source.clone(), Some(oid), false))
        }
    }
}

fn write_prepared(
    repo: &Path,
    file: &str,
    source: &Worktree,
    next: &Worktree,
) -> Result<Option<super::conflict_fs::Recovery>, String> {
    super::conflict_fs::replace(&conflict_path(repo, file)?, source, next)
}

pub fn save(
    repo_path: &str,
    request: &ConflictSaveRequest,
) -> Result<ConflictSaveOutcome, CommandError> {
    save_with_gate(repo_path, request, |_, _| Ok(()), |_| Ok(()))
}

/// The checked source determines the required file capability. Every mutating
/// Git command is separately judged with its exact argv before execution.
pub fn save_with_gate(
    repo_path: &str,
    request: &ConflictSaveRequest,
    mut authorize_file: impl FnMut(&str, &str) -> Result<(), String>,
    mut judge: impl FnMut(&[&str]) -> Result<(), CommandError>,
) -> Result<ConflictSaveOutcome, CommandError> {
    if let ConflictFileChoice::Chunks(choices) = &request.choice {
        let bytes = choices.iter().fold(0usize, |sum, choice| {
            sum.saturating_add(match choice {
                ConflictResolutionChoice::Custom(text) => text.len(),
                _ => 0,
            })
        });
        if choices.len() > super::conflict::MAX_CONFLICT_CHUNKS
            || bytes > super::conflict::MAX_CONFLICT_TEXT_BYTES
        {
            return Err("Resolution choices exceed the editor limit".into());
        }
    }
    let repo = validate_repo(repo_path)?;
    let mutex = repo_mutation_lock(&repo);
    let _guard = mutex.lock().map_err(|_| {
        "A previous repository mutation panicked; restart GitPulse before resolving"
    })?;
    let mut transaction = IndexTransaction::begin(&repo)?;
    let (current, source) = snapshot_inner(&repo, &request.file_path)?;
    if current.revision != request.revision {
        return Err("The file, Git conflict stages, or operation changed since loading. Reload; your draft is retained.".into());
    }
    let selected_stage = match request.choice {
        ConflictFileChoice::Ours => Some(2),
        ConflictFileChoice::Theirs => Some(3),
        _ => None,
    };
    let file_op = match selected_stage {
        Some(stage) if !current.stages.iter().any(|entry| entry.stage == stage) => "delete",
        Some(_) if source.mode == "missing" => "create",
        None if source.mode == "missing"
            && matches!(
                request.choice,
                ConflictFileChoice::WorkingTree | ConflictFileChoice::StageOnly
            ) =>
        {
            "delete"
        }
        _ => "modify",
    };
    link_limitation(&request.choice, &current.stages)?;
    authorize_file(&request.file_path, file_op)?;
    let (next, oid, write) = desired(&repo, &current, &source, &request.choice, &mut judge)?;
    let mode = staged_mode(&request.choice, &current.stages, &source, &next);
    transaction.prepare(&repo, &request.file_path, &mode, oid.as_deref(), &mut judge)?;
    // Git's selected executable bit belongs in the index. A working tree that
    // cannot express it holds the file as 100644, so compare physical snapshots
    // in that mode.
    let next = if posix_worktree() || next.mode != "100755" {
        next
    } else {
        Worktree {
            mode: "100644".into(),
            bytes: next.bytes,
        }
    };
    let write = write && source != next;
    // Filters may run arbitrary configured tools and take time. Recheck after
    // all preparation and before publishing any working-tree mutation.
    let (fresh, _) = snapshot_inner(&repo, &request.file_path)?;
    if fresh.revision != current.revision {
        return Err(
            "Source changed while preparing the resolution; no file was written. Reload.".into(),
        );
    }
    let mut recovery = if write {
        write_prepared(&repo, &request.file_path, &source, &next)?
    } else {
        None
    };
    let finish = (|| -> Result<_, CommandError> {
        if operation(&repo)? != current.operation
            || read_worktree(&conflict_path(&repo, &request.file_path)?)? != next
        {
            return Err(
                "The file or operation changed during save; the resolution was not staged".into(),
            );
        }
        transaction.publish().map_err(CommandError::from)
    })();
    if let Err(message) = finish {
        let (snapshot, detail) = match snapshot_inner(&repo, &request.file_path) {
            Ok((snap, worktree))
                if worktree == next
                    && snap.operation == current.operation
                    && snap.stages == current.stages =>
            {
                (Some(snap), String::new())
            }
            Ok(_) => (
                None,
                " The source changed again; reload and review it before staging.".into(),
            ),
            Err(error) => (
                None,
                format!(
                    " The saved source could not be reloaded: {error}. Reload before staging.",
                    error = error.message()
                ),
            ),
        };
        return Ok(ConflictSaveOutcome {
            written: write,
            staged: false,
            message: format!(
                "Resolution prepared, but staging failed: {message}{detail}",
                message = message.message()
            ),
            recovery_path: recovery
                .as_ref()
                .map(|temp| temp.path.to_string_lossy().into_owned()),
            snapshot,
        });
    }
    if let Some(temp) = recovery.as_mut() {
        temp.keep = false;
    }
    Ok(ConflictSaveOutcome {
        written: write,
        staged: true,
        message: "Resolution staged. Review the staged changes before continuing.".into(),
        recovery_path: None,
        snapshot: None,
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn a_replaced_index_lock_is_neither_published_nor_deleted() {
        let temp = tempfile::TempDir::new().unwrap();
        let repo = temp.path().canonicalize().unwrap();
        crate::test_support::git_in(&repo, &["init", "-b", "main"]);
        fs::write(repo.join("file"), b"original\n").unwrap();
        git(&repo, &["add", "file"]).unwrap();
        let original = fs::read(repo.join(".git/index")).unwrap();
        let mut transaction = IndexTransaction::begin(&repo).unwrap();
        fs::remove_file(repo.join(".git/index.lock")).unwrap();
        fs::write(repo.join(".git/index.lock"), b"another owner").unwrap();
        assert!(transaction.publish().is_err());
        drop(transaction);
        assert_eq!(fs::read(repo.join(".git/index")).unwrap(), original);
        assert_eq!(
            fs::read(repo.join(".git/index.lock")).unwrap(),
            b"another owner"
        );
    }

    #[test]
    fn publication_failure_retains_original_content_and_does_not_replace_the_index() {
        let temp = tempfile::TempDir::new().unwrap();
        let repo = temp.path().canonicalize().unwrap();
        crate::test_support::git_in(&repo, &["init", "-b", "main"]);
        fs::write(repo.join("file"), b"original\n").unwrap();
        git(&repo, &["add", "file"]).unwrap();
        git(
            &repo,
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-m",
                "base",
            ],
        )
        .unwrap();
        let original_index = fs::read(repo.join(".git/index")).unwrap();
        let mut transaction = IndexTransaction::begin(&repo).unwrap();
        let oid = String::from_utf8(
            git_with_stdin(&repo, &["hash-object", "-w", "--stdin"], b"resolved\n").unwrap(),
        )
        .unwrap();
        transaction
            .prepare(&repo, "file", "100644", Some(oid.trim()), &mut |_| Ok(()))
            .unwrap();
        let source = read_worktree(&repo.join("file")).unwrap();
        let next = Worktree {
            bytes: Some(b"resolved\n".to_vec()),
            mode: "100644".into(),
        };
        let recovery = write_prepared(&repo, "file", &source, &next)
            .unwrap()
            .unwrap();
        // Deterministically simulate losing the owned lock after replacement.
        fs::remove_file(repo.join(".git/index.lock")).unwrap();
        assert!(transaction
            .publish()
            .unwrap_err()
            .contains("Cannot publish"));
        assert_eq!(fs::read(repo.join("file")).unwrap(), b"resolved\n");
        assert_eq!(fs::read(repo.join(".git/index")).unwrap(), original_index);
        let path = recovery.path.clone();
        drop(recovery);
        assert_eq!(fs::read(path).unwrap(), b"original\n");
    }

    struct WindowsWorktree;
    impl WindowsWorktree {
        fn simulate() -> Self {
            SIMULATE_WINDOWS_WORKTREE.with(|flag| flag.set(true));
            Self
        }
    }
    impl Drop for WindowsWorktree {
        fn drop(&mut self) {
            SIMULATE_WINDOWS_WORKTREE.with(|flag| flag.set(false));
        }
    }

    fn commit_file(repo: &Path, content: &str, exec: bool, message: &str) {
        fs::write(repo.join("file"), content).unwrap();
        use std::os::unix::fs::PermissionsExt;
        let bits = if exec { 0o755 } else { 0o644 };
        fs::set_permissions(repo.join("file"), fs::Permissions::from_mode(bits)).unwrap();
        crate::test_support::git_in(repo, &["add", "file"]);
        crate::test_support::git_in(repo, &["commit", "-m", message]);
    }

    /// A real content conflict whose sides carry the given executable bits.
    fn merge_conflict(base_exec: bool, ours_exec: bool, theirs_exec: bool) -> tempfile::TempDir {
        let temp = tempfile::TempDir::new().unwrap();
        let repo = temp.path().canonicalize().unwrap();
        crate::test_support::git_in(&repo, &["init", "-b", "main"]);
        crate::test_support::git_in(&repo, &["config", "core.filemode", "true"]);
        commit_file(&repo, "base\n", base_exec, "base");
        crate::test_support::git_in(&repo, &["checkout", "-b", "incoming"]);
        commit_file(&repo, "theirs\n", theirs_exec, "theirs");
        crate::test_support::git_in(&repo, &["checkout", "main"]);
        commit_file(&repo, "ours\n", ours_exec, "ours");
        assert!(git(&repo, &["merge", "incoming"]).is_err());
        temp
    }

    fn make_worktree_plain(repo: &Path) {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(repo.join("file"), fs::Permissions::from_mode(0o644)).unwrap();
    }

    fn save_choice(
        repo: &Path,
        choice: ConflictFileChoice,
    ) -> Result<ConflictSaveOutcome, CommandError> {
        let repo_path = repo.to_str().unwrap();
        let snapshot = snapshot(repo_path, "file")?;
        save(
            repo_path,
            &ConflictSaveRequest {
                file_path: "file".into(),
                revision: snapshot.revision,
                choice,
            },
        )
    }

    fn index_mode(repo: &Path) -> String {
        let listing =
            String::from_utf8(git(repo, &["ls-files", "--stage", "--", "file"]).unwrap()).unwrap();
        listing.split_whitespace().next().unwrap_or("").into()
    }

    fn assert_regular_bytes(repo: &Path, expected: &[u8]) {
        use std::os::unix::fs::PermissionsExt;
        let meta = fs::symlink_metadata(repo.join("file")).unwrap();
        assert!(meta.is_file());
        assert_eq!(fs::read(repo.join("file")).unwrap(), expected);
        assert_eq!(meta.permissions().mode() & 0o111, 0);
    }

    #[test]
    fn windows_chunk_resolution_keeps_the_index_executable_bit() {
        let temp = merge_conflict(true, true, true);
        let repo = temp.path().canonicalize().unwrap();
        make_worktree_plain(&repo);
        let _windows = WindowsWorktree::simulate();
        let outcome = save_choice(
            &repo,
            ConflictFileChoice::Chunks(vec![ConflictResolutionChoice::AcceptTheirs]),
        )
        .unwrap();
        assert!(outcome.staged, "{}", outcome.message);
        assert_eq!(index_mode(&repo), "100755");
        assert_regular_bytes(&repo, b"theirs\n");
    }

    #[test]
    fn windows_working_tree_resolution_keeps_the_index_executable_bit() {
        let temp = merge_conflict(true, true, true);
        let repo = temp.path().canonicalize().unwrap();
        fs::write(repo.join("file"), "hand merged\n").unwrap();
        make_worktree_plain(&repo);
        let _windows = WindowsWorktree::simulate();
        let outcome = save_choice(&repo, ConflictFileChoice::WorkingTree).unwrap();
        assert!(outcome.staged, "{}", outcome.message);
        assert_eq!(index_mode(&repo), "100755");
        assert_regular_bytes(&repo, b"hand merged\n");
    }

    #[test]
    fn windows_ours_and_theirs_pass_the_stage_mode_through() {
        for (choice, expected) in [
            (ConflictFileChoice::Ours, "100644"),
            (ConflictFileChoice::Theirs, "100755"),
        ] {
            let temp = merge_conflict(false, false, true);
            let repo = temp.path().canonicalize().unwrap();
            make_worktree_plain(&repo);
            let _windows = WindowsWorktree::simulate();
            let outcome = save_choice(&repo, choice).unwrap();
            assert!(outcome.staged, "{}", outcome.message);
            assert_eq!(index_mode(&repo), expected);
        }
    }

    #[test]
    fn windows_chunks_take_the_mode_the_side_that_changed_it_chose() {
        // Theirs made the file executable and ours left the base alone.
        let temp = merge_conflict(false, false, true);
        let repo = temp.path().canonicalize().unwrap();
        make_worktree_plain(&repo);
        let _windows = WindowsWorktree::simulate();
        let choices = vec![ConflictResolutionChoice::AcceptOurs];
        let outcome = save_choice(&repo, ConflictFileChoice::Chunks(choices)).unwrap();
        assert!(outcome.staged, "{}", outcome.message);
        assert_eq!(index_mode(&repo), "100755");
        drop(_windows);

        // Ours dropped the executable bit and theirs left the base alone.
        let temp = merge_conflict(true, false, true);
        let repo = temp.path().canonicalize().unwrap();
        make_worktree_plain(&repo);
        let _windows = WindowsWorktree::simulate();
        let choices = vec![ConflictResolutionChoice::AcceptTheirs];
        let outcome = save_choice(&repo, ConflictFileChoice::Chunks(choices)).unwrap();
        assert!(outcome.staged, "{}", outcome.message);
        assert_eq!(index_mode(&repo), "100644");
    }

    #[test]
    fn merged_regular_mode_rules() {
        let stage = |number: u8, mode: &str| ConflictStage {
            stage: number,
            mode: mode.into(),
            oid: String::new(),
            size: 0,
            text: None,
        };
        let merged = |stages: &[ConflictStage]| merged_regular_mode(stages).map(str::to_owned);
        // Both sides changed the bit differently from nothing (add/add): ours.
        assert_eq!(
            merged(&[stage(2, "100644"), stage(3, "100755")]).as_deref(),
            Some("100644")
        );
        // Only one side changed it from the base: that side.
        assert_eq!(
            merged(&[stage(1, "100644"), stage(2, "100644"), stage(3, "100755")]).as_deref(),
            Some("100755")
        );
        assert_eq!(
            merged(&[stage(1, "100755"), stage(2, "100644"), stage(3, "100755")]).as_deref(),
            Some("100644")
        );
        // Deleted on one side, or a gitlink or link on the other: abstain.
        assert_eq!(
            merged(&[stage(1, "100644"), stage(3, "100755")]).as_deref(),
            Some("100755")
        );
        assert_eq!(merged(&[stage(2, "160000")]), None);
        assert_eq!(
            merged(&[stage(2, "120000"), stage(3, "100755")]).as_deref(),
            Some("100755")
        );
        assert_eq!(merged(&[]), None);
    }

    #[test]
    fn windows_modify_delete_conflict_keeps_the_surviving_executable_bit() {
        let temp = merge_conflict(true, true, true);
        let repo = temp.path().canonicalize().unwrap();
        // Theirs deleted the file: only stages 1 and 2 remain.
        let blob = String::from_utf8(git(&repo, &["rev-parse", ":2:file"]).unwrap()).unwrap();
        let input = format!(
            "0 {}\tfile\0100755 {} 2\tfile\0",
            "0".repeat(40),
            blob.trim()
        );
        git_with_stdin(
            &repo,
            &["update-index", "-z", "--index-info"],
            input.as_bytes(),
        )
        .unwrap();
        fs::write(repo.join("file"), "kept\n").unwrap();
        make_worktree_plain(&repo);
        let _windows = WindowsWorktree::simulate();
        let outcome = save_choice(&repo, ConflictFileChoice::WorkingTree).unwrap();
        assert!(outcome.staged, "{}", outcome.message);
        assert_eq!(index_mode(&repo), "100755");
    }

    #[test]
    fn windows_symlink_conflict_is_a_typed_limitation_not_a_plain_file() {
        let temp = merge_conflict(false, false, false);
        let repo = temp.path().canonicalize().unwrap();
        let blob = String::from_utf8(
            git_with_stdin(&repo, &["hash-object", "-w", "--stdin"], b"target").unwrap(),
        )
        .unwrap();
        let input = format!(
            "0 {}\tfile\0120000 {} 3\tfile\0",
            "0".repeat(40),
            blob.trim()
        );
        // Ours keeps its regular file; theirs is a link, written as a plain
        // file by a checkout with core.symlinks=false.
        let ours = String::from_utf8(git(&repo, &["rev-parse", ":2:file"]).unwrap()).unwrap();
        let input = format!("{input}100644 {} 2\tfile\0", ours.trim());
        git_with_stdin(
            &repo,
            &["update-index", "-z", "--index-info"],
            input.as_bytes(),
        )
        .unwrap();
        fs::write(repo.join("file"), "target").unwrap();
        let unmerged = git(&repo, &["ls-files", "--stage"]).unwrap();
        let _windows = WindowsWorktree::simulate();

        let snapshot = snapshot(repo.to_str().unwrap(), "file").unwrap();
        assert_eq!(snapshot.reason.as_deref(), Some(LINK_LIMITATION));
        assert!(snapshot.document.is_none());
        assert!(!LINK_LIMITATION.contains("preserve"));
        assert!(LINK_LIMITATION.contains("side that is not a link"));

        for choice in [
            ConflictFileChoice::Theirs,
            ConflictFileChoice::WorkingTree,
            ConflictFileChoice::StageOnly,
            ConflictFileChoice::Chunks(vec![ConflictResolutionChoice::AcceptOurs]),
        ] {
            let error = save_choice(&repo, choice).unwrap_err();
            assert_eq!(error, LINK_LIMITATION);
            assert_eq!(git(&repo, &["ls-files", "--stage"]).unwrap(), unmerged);
            assert_eq!(fs::read(repo.join("file")).unwrap(), b"target");
        }
        // The side that is not a link is still an ordinary choice.
        assert!(save_choice(&repo, ConflictFileChoice::Ours).unwrap().staged);
        assert_eq!(index_mode(&repo), "100644");
    }

    #[test]
    fn unix_chunk_resolution_still_follows_the_working_file_mode() {
        let temp = merge_conflict(true, true, true);
        let repo = temp.path().canonicalize().unwrap();
        let choices = vec![ConflictResolutionChoice::AcceptTheirs];
        let outcome = save_choice(&repo, ConflictFileChoice::Chunks(choices)).unwrap();
        assert!(outcome.staged, "{}", outcome.message);
        assert_eq!(index_mode(&repo), "100755");
    }

    /// A conflict whose sides carry the given executable bits; `None` is absent
    /// for the base and deleted for a side. Returns `None` when Git merges the
    /// combination cleanly, since there is then nothing to resolve.
    fn merge_modes(
        base: Option<bool>,
        ours: Option<bool>,
        theirs: Option<bool>,
    ) -> Option<tempfile::TempDir> {
        let temp = tempfile::TempDir::new().unwrap();
        let repo = temp.path().canonicalize().unwrap();
        crate::test_support::git_in(&repo, &["init", "-b", "main"]);
        crate::test_support::git_in(&repo, &["config", "core.filemode", "true"]);
        crate::test_support::git_in(&repo, &["commit", "--allow-empty", "-m", "root"]);
        if let Some(exec) = base {
            commit_file(&repo, "base\n", exec, "base");
        }
        let apply = |side: Option<bool>, text: &str, message: &str| match side {
            Some(exec) => commit_file(&repo, text, exec, message),
            None => {
                if base.is_some() {
                    crate::test_support::git_in(&repo, &["rm", "-f", "file"]);
                }
                crate::test_support::git_in(&repo, &["commit", "--allow-empty", "-m", message]);
            }
        };
        crate::test_support::git_in(&repo, &["checkout", "-b", "incoming"]);
        apply(theirs, "theirs\n", "theirs");
        crate::test_support::git_in(&repo, &["checkout", "main"]);
        apply(ours, "ours\n", "ours");
        git(&repo, &["merge", "incoming"]).is_err().then_some(temp)
    }

    #[test]
    fn windows_staged_mode_matches_what_git_merge_leaves_in_the_worktree() {
        // Git's own merge is the oracle: the executable bit it writes to the
        // working file for each conflict is what a resolution must stage.
        let states = [None, Some(false), Some(true)];
        let mut conflicts = 0;
        for base in states {
            for ours in states {
                for theirs in states {
                    let Some(temp) = merge_modes(base, ours, theirs) else {
                        continue;
                    };
                    let repo = temp.path().canonicalize().unwrap();
                    use std::os::unix::fs::PermissionsExt;
                    let oracle = match fs::metadata(repo.join("file")) {
                        Ok(meta) if meta.permissions().mode() & 0o111 != 0 => "100755",
                        Ok(_) => "100644",
                        Err(_) => continue,
                    };
                    fs::write(repo.join("file"), "resolved\n").unwrap();
                    make_worktree_plain(&repo);
                    let _windows = WindowsWorktree::simulate();
                    let outcome = save_choice(&repo, ConflictFileChoice::WorkingTree).unwrap();
                    let case = format!("{base:?} {ours:?} {theirs:?}");
                    assert!(outcome.staged, "{case}: {}", outcome.message);
                    assert_eq!(index_mode(&repo), oracle, "{case}");
                    conflicts += 1;
                }
            }
        }
        assert!(
            conflicts >= 15,
            "the matrix must exercise real conflicts: {conflicts}"
        );
    }

    #[test]
    fn windows_stage_only_takes_the_mode_from_the_stages() {
        let temp = merge_conflict(false, false, true);
        let repo = temp.path().canonicalize().unwrap();
        fs::write(repo.join("file"), "resolved\n").unwrap();
        make_worktree_plain(&repo);
        let _windows = WindowsWorktree::simulate();
        let outcome = save_choice(&repo, ConflictFileChoice::StageOnly).unwrap();
        assert!(outcome.staged, "{}", outcome.message);
        assert_eq!(index_mode(&repo), "100755");
    }

    #[test]
    fn a_link_that_appears_after_the_snapshot_is_refused_on_save() {
        let temp = merge_conflict(false, false, false);
        let repo = temp.path().canonicalize().unwrap();
        make_worktree_plain(&repo);
        let _windows = WindowsWorktree::simulate();
        let repo_path = repo.to_str().unwrap();
        let loaded = snapshot(repo_path, "file").unwrap();
        assert_eq!(loaded.reason, None);
        let blob = String::from_utf8(
            git_with_stdin(&repo, &["hash-object", "-w", "--stdin"], b"target").unwrap(),
        )
        .unwrap();
        let input = format!(
            "0 {}\tfile\0120000 {} 3\tfile\0",
            "0".repeat(40),
            blob.trim()
        );
        git_with_stdin(
            &repo,
            &["update-index", "-z", "--index-info"],
            input.as_bytes(),
        )
        .unwrap();
        let unmerged = git(&repo, &["ls-files", "--stage"]).unwrap();
        let error = save(
            repo_path,
            &ConflictSaveRequest {
                file_path: "file".into(),
                revision: loaded.revision,
                choice: ConflictFileChoice::Theirs,
            },
        )
        .unwrap_err();
        assert!(
            error.contains("changed since loading"),
            "{error}",
            error = error.message()
        );
        assert_eq!(git(&repo, &["ls-files", "--stage"]).unwrap(), unmerged);
    }

    #[test]
    fn windows_submodule_conflict_keeps_the_gitlink_and_refuses_a_working_file() {
        let temp = merge_conflict(false, false, false);
        let repo = temp.path().canonicalize().unwrap();
        let input = format!(
            "0 {zero}\tfile\0160000 {a} 1\tfile\0160000 {b} 2\tfile\0160000 {c} 3\tfile\0",
            zero = "0".repeat(40),
            a = "1".repeat(40),
            b = "2".repeat(40),
            c = "3".repeat(40),
        );
        git_with_stdin(
            &repo,
            &["update-index", "-z", "--index-info"],
            input.as_bytes(),
        )
        .unwrap();
        fs::remove_file(repo.join("file")).unwrap();
        fs::create_dir(repo.join("file")).unwrap();
        let unmerged = git(&repo, &["ls-files", "--stage"]).unwrap();
        let _windows = WindowsWorktree::simulate();
        for choice in [
            ConflictFileChoice::WorkingTree,
            ConflictFileChoice::StageOnly,
            ConflictFileChoice::Chunks(vec![ConflictResolutionChoice::AcceptOurs]),
        ] {
            assert!(save_choice(&repo, choice).is_err());
            assert_eq!(git(&repo, &["ls-files", "--stage"]).unwrap(), unmerged);
        }
        assert!(
            save_choice(&repo, ConflictFileChoice::Theirs)
                .unwrap()
                .staged
        );
        assert_eq!(index_mode(&repo), "160000");
    }
}
