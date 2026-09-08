//! Conflict writes own a source snapshot, a private index and Git's index lock.
//! Ordinary editor writes must not be used for this transaction: choosing text
//! and staging a later filesystem read can otherwise stage somebody else's edit.

use super::conflict::{ConflictDocument, ConflictResolutionChoice, ConflictResolver, FileSegment};
use crate::engine::git_cli::{
    git, git_text, git_with_index, git_with_stdin, resolve_git_dir, sandbox_join, validate_repo,
};
use crate::engine::git_writer::repo_mutation_lock;
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
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
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options
        .open(path)
        .map_err(|e| format!("Cannot read {}: {e}", path.display()))?;
    let meta = file.metadata().map_err(|e| e.to_string())?;
    if !meta.is_file() || meta.len() > cap as u64 {
        return Err(format!(
            "{} is not a regular file within the {} MiB limit",
            path.display(),
            cap / 1024 / 1024
        ));
    }
    let mut bytes = Vec::new();
    file.take(cap as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > cap {
        return Err("File grew beyond the resolution size limit; reload".into());
    }
    Ok(bytes)
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

fn hash(repo: &Path, bytes: &[u8]) -> Result<String, String> {
    String::from_utf8(git_with_stdin(
        repo,
        &["hash-object", "--stdin", "--no-filters"],
        bytes,
    )?)
    .map(|text| text.trim().to_owned())
    .map_err(|e| e.to_string())
}

fn operation(repo: &Path) -> Result<String, String> {
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
            Err(e) => return Err(e.to_string()),
        }
    }
    hash(
        repo,
        &serde_json::to_vec(&parts).map_err(|e| e.to_string())?,
    )
}

fn index_stages(repo: &Path, file: &str) -> Result<Vec<ConflictStage>, String> {
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
) -> Result<String, String> {
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

fn snapshot_inner(repo: &Path, file: &str) -> Result<(ConflictSnapshot, Worktree), String> {
    let dest = conflict_path(repo, file)?;
    let before = operation(repo)?;
    let mut stages = index_stages(repo, file)?;
    let worktree = read_worktree(&dest)?;
    let mut reason = None;
    let marker_size = conflict_marker_size(repo, file)?;
    let document = match (&worktree.bytes, worktree.mode.as_str()) {
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

fn conflict_marker_size(repo: &Path, file: &str) -> Result<usize, String> {
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

pub fn snapshot(repo_path: &str, file: &str) -> Result<ConflictSnapshot, String> {
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
    judge: &mut impl FnMut(&[&str]) -> Result<(), String>,
) -> Result<Vec<u8>, String> {
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
    fn begin(repo: &Path) -> Result<Self, String> {
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
        judge: &mut impl FnMut(&[&str]) -> Result<(), String>,
    ) -> Result<(), String> {
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
        ], &[]).map_err(|error| format!("The prepared Git object failed the staged integrity check; no file was written. {error}"))?;
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

fn desired(
    repo: &Path,
    snap: &ConflictSnapshot,
    source: &Worktree,
    choice: &ConflictFileChoice,
    judge: &mut impl FnMut(&[&str]) -> Result<(), String>,
) -> Result<(Worktree, Option<String>, bool), String> {
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

pub fn save(repo_path: &str, request: &ConflictSaveRequest) -> Result<ConflictSaveOutcome, String> {
    save_with_gate(repo_path, request, |_, _| Ok(()), |_| Ok(()))
}

/// The checked source determines the required file capability. Every mutating
/// Git command is separately judged with its exact argv before execution.
pub fn save_with_gate(
    repo_path: &str,
    request: &ConflictSaveRequest,
    mut authorize_file: impl FnMut(&str, &str) -> Result<(), String>,
    mut judge: impl FnMut(&[&str]) -> Result<(), String>,
) -> Result<ConflictSaveOutcome, String> {
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
    authorize_file(&request.file_path, file_op)?;
    let (next, oid, write) = desired(&repo, &current, &source, &request.choice, &mut judge)?;
    let mode = if matches!(
        request.choice,
        ConflictFileChoice::Ours | ConflictFileChoice::Theirs
    ) && source.mode == "160000"
    {
        "160000"
    } else {
        &next.mode
    };
    transaction.prepare(&repo, &request.file_path, mode, oid.as_deref(), &mut judge)?;
    // Git's selected executable bit belongs in the index. Windows worktree
    // files do not expose it, so compare physical snapshots in their own mode.
    #[cfg(windows)]
    let next = Worktree {
        mode: if next.mode == "100755" {
            "100644".into()
        } else {
            next.mode
        },
        bytes: next.bytes,
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
    let finish = (|| {
        if operation(&repo)? != current.operation
            || read_worktree(&conflict_path(&repo, &request.file_path)?)? != next
        {
            return Err(
                "The file or operation changed during save; the resolution was not staged".into(),
            );
        }
        transaction.publish()
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
                format!(" The saved source could not be reloaded: {error}. Reload before staging."),
            ),
        };
        return Ok(ConflictSaveOutcome {
            written: write,
            staged: false,
            message: format!("Resolution prepared, but staging failed: {message}{detail}"),
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
        git(&repo, &["init", "-b", "main"]).unwrap();
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
        git(&repo, &["init", "-b", "main"]).unwrap();
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
}
