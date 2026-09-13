//! Explicit execution authority for a checkout, independent of repository data.
//!
//! Inspection never starts Git. Grants bind canonical checkout/private/common
//! Git directories and their filesystem identities. The desktop is the only
//! IPC surface that can grant trust; MCP and repository-local policy cannot.

use serde::{Deserialize, Serialize};
use std::collections::{hash_map::DefaultHasher, HashMap};
use std::fs::{self, OpenOptions};
use std::hash::{Hash, Hasher};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::UNIX_EPOCH;

pub const REQUIRED: &str = "REPOSITORY_TRUST_REQUIRED";
const MAX_METADATA: u64 = 16 * 1024;
const MAX_RECORD: u64 = 128 * 1024;
const MAX_SESSION_GRANTS: usize = 4096;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct DirectoryIdentity {
    path: PathBuf,
    created_ns: u128,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(windows)]
    file_identity: (u64, [u8; 16]),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct Identity {
    version: u32,
    checkout: DirectoryIdentity,
    git_dir: DirectoryIdentity,
    common_dir: DirectoryIdentity,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TrustPreview {
    pub path: String,
    pub git_dir: String,
    pub common_dir: String,
    /// Opaque snapshot returned to the grant command unchanged.
    pub identity: String,
    pub trusted: bool,
}

fn sessions() -> &'static Mutex<HashMap<PathBuf, Identity>> {
    static GRANTS: OnceLock<Mutex<HashMap<PathBuf, Identity>>> = OnceLock::new();
    GRANTS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn directory(path: &Path) -> Result<DirectoryIdentity, String> {
    let path = path
        .canonicalize()
        .map_err(|e| format!("Cannot resolve {}: {e}", path.display()))?;
    #[cfg(windows)]
    let handle = crate::fs_entry::pin_directory(&path)
        .map_err(|e| format!("Cannot pin repository identity: {e}"))?;
    #[cfg(windows)]
    let metadata = handle.metadata().map_err(|e| e.to_string())?;
    #[cfg(not(windows))]
    let metadata =
        fs::metadata(&path).map_err(|e| format!("Cannot inspect {}: {e}", path.display()))?;
    if !metadata.is_dir() {
        return Err(format!("Not a directory: {}", path.display()));
    }
    // Birth time distinguishes a replacement even when an inode is recycled.
    // Filesystems that cannot supply identity fail closed rather than creating
    // an approval that silently applies to a future replacement.
    let created_ns = metadata
        .created()
        .and_then(|t| t.duration_since(UNIX_EPOCH).map_err(std::io::Error::other))
        .map_err(|e| {
            format!(
                "Cannot establish repository identity for {}: {e}",
                path.display()
            )
        })?
        .as_nanos();
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;
    Ok(DirectoryIdentity {
        path,
        created_ns,
        #[cfg(unix)]
        device: metadata.dev(),
        #[cfg(unix)]
        inode: metadata.ino(),
        #[cfg(windows)]
        file_identity: crate::fs_entry::windows_file_identity(&handle)
            .map_err(|e| format!("Cannot establish repository file identity: {e}"))?,
    })
}

fn bounded_text(path: &Path, limit: u64) -> Result<String, String> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // A substituted FIFO must not park the trust prompt indefinitely.
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options
        .open(path)
        .map_err(|e| format!("Cannot read {}: {e}", path.display()))?;
    if !file.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err(format!("Expected a regular file: {}", path.display()));
    }
    let mut text = String::new();
    file.take(limit + 1)
        .read_to_string(&mut text)
        .map_err(|e| format!("Cannot read {}: {e}", path.display()))?;
    if text.len() as u64 > limit {
        return Err(format!(
            "Repository metadata exceeds {limit} bytes: {}",
            path.display()
        ));
    }
    Ok(text)
}

fn metadata_target(base: &Path, raw: &str) -> Result<PathBuf, String> {
    let raw = raw.trim_end_matches(['\r', '\n']);
    if raw.is_empty() || raw.chars().any(char::is_control) {
        return Err("Invalid Git directory pointer".into());
    }
    let path = Path::new(raw);
    Ok(if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    })
}

fn identity(repo_path: &str) -> Result<Identity, String> {
    let checkout = directory(&crate::engine::git_cli::validate_repo_path(repo_path)?)?;
    let dotgit = checkout.path.join(".git");
    let git_path = if dotgit.is_dir() {
        dotgit
    } else if dotgit.is_file() {
        let contents = bounded_text(&dotgit, MAX_METADATA)?;
        let raw = contents
            .strip_prefix("gitdir: ")
            .ok_or("Invalid .git file: expected gitdir pointer")?;
        metadata_target(&checkout.path, raw)?
    } else {
        checkout.path.clone()
    };
    let git_dir = directory(&git_path)?;
    let common_pointer = git_dir.path.join("commondir");
    let common_dir = match fs::symlink_metadata(&common_pointer) {
        Ok(_) => directory(&metadata_target(
            &git_dir.path,
            &bounded_text(&common_pointer, MAX_METADATA)?,
        )?)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => git_dir.clone(),
        Err(error) => return Err(format!("Cannot inspect common Git directory: {error}")),
    };
    if !git_dir.path.join("HEAD").is_file() || !common_dir.path.join("objects").is_dir() {
        return Err("Incomplete Git repository metadata".into());
    }
    Ok(Identity {
        version: 1,
        checkout,
        git_dir,
        common_dir,
    })
}

fn record_path(root: &Path, checkout: &Path) -> PathBuf {
    // This hash is only a bounded filename, never an authentication check.
    // A collision cannot grant trust: the entire Identity must match below.
    let mut key = DefaultHasher::new();
    checkout.hash(&mut key);
    root.join(format!("{:016x}.json", key.finish()))
}

fn persistent_root() -> Result<PathBuf, String> {
    crate::tool_config::default_config_dir()
        .map(|dir| dir.join("repository-trust-v1"))
        .ok_or_else(|| "Cannot resolve GitPulse repository trust storage".into())
}

fn read_grant(root: &Path, current: &Identity) -> Result<bool, String> {
    let path = record_path(root, &current.checkout.path);
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(format!("Cannot read repository trust: {error}")),
        Ok(metadata) if !metadata.is_file() || metadata.file_type().is_symlink() => {
            return Err("Repository trust record is not a regular file".into())
        }
        Ok(_) => {}
    }
    let saved: Identity = serde_json::from_str(&bounded_text(&path, MAX_RECORD)?)
        .map_err(|e| format!("Invalid repository trust record: {e}"))?;
    Ok(saved.version == 1 && saved == *current)
}

fn trusted(current: &Identity) -> Result<bool, String> {
    if sessions()
        .lock()
        .map_err(|_| "Repository trust state is unavailable")?
        .get(&current.checkout.path)
        == Some(current)
    {
        return Ok(true);
    }
    read_grant(&persistent_root()?, current)
}

pub fn inspect(repo_path: &str) -> Result<TrustPreview, String> {
    let current = identity(repo_path)?;
    Ok(TrustPreview {
        path: current.checkout.path.to_string_lossy().into_owned(),
        git_dir: current.git_dir.path.to_string_lossy().into_owned(),
        common_dir: current.common_dir.path.to_string_lossy().into_owned(),
        identity: serde_json::to_string(&current).map_err(|e| e.to_string())?,
        trusted: trusted(&current)?,
    })
}

/// Metadata-only family resolution must not execute an unapproved sibling's
/// Git configuration merely to discover a shared ledger or watch directory.
pub(crate) fn git_directories(repo: &Path) -> Result<(PathBuf, PathBuf), String> {
    let current = identity(repo.to_str().ok_or("Repository path is not UTF-8")?)?;
    Ok((current.git_dir.path, current.common_dir.path))
}

/// Require a current grant. Called at admission and again at process spawn.
pub fn require(repo: &Path) -> Result<(), String> {
    let current = identity(repo.to_str().ok_or("Repository path is not UTF-8")?)?;
    if trusted(&current)? {
        return Ok(());
    }
    Err(format!("{REQUIRED}: Open {} in GitPulse and explicitly trust this checkout before running repository commands.", current.checkout.path.display()))
}

/// Whether `message` is — or carries, once a caller has wrapped it — the
/// refusal [`require`] returns.
///
/// A refusal is not a verdict about the repository. It says this process was
/// never allowed to look, so nothing was examined and nothing was found
/// wanting. Callers that turn an error string into a category must ask here
/// rather than matching [`REQUIRED`] themselves: the one classifier that did
/// not ask fell through to its "we looked, and it is broken" arm and told
/// users that six perfectly valid checkouts were `[invalid_worktree]`.
///
/// A substring rather than a prefix test because the marker travels inside
/// whatever context its caller wrapped around it.
pub fn refused(message: &str) -> bool {
    message.contains(REQUIRED)
}

fn save_grant(root: &Path, current: &Identity) -> Result<(), String> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    fs::create_dir_all(root).map_err(|e| format!("Cannot create repository trust storage: {e}"))?;
    let path = record_path(root, &current.checkout.path);
    let temporary = root.join(format!(
        ".grant-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temporary)
        .map_err(|e| format!("Cannot prepare repository trust: {e}"))?;
    let result = (|| {
        let data = serde_json::to_vec(current).map_err(|e| e.to_string())?;
        file.write_all(&data)
            .and_then(|_| file.sync_all())
            .map_err(|e| format!("Cannot save repository trust: {e}"))?;
        drop(file);
        fs::rename(&temporary, &path).map_err(|e| format!("Cannot publish repository trust: {e}"))
    })();
    if result.is_err() {
        if let Err(error) = fs::remove_file(&temporary) {
            if error.kind() != std::io::ErrorKind::NotFound {
                log::warn!("Cannot remove incomplete trust record: {error}");
            }
        }
    }
    result
}

/// The exact inspected identity must accompany a deliberate approval. A
/// session grant is useful to callers with an in-process approval UI and tests;
/// it uses the same identity checks and never grants another process authority.
pub fn grant(repo_path: &str, expected_identity: &str, remember: bool) -> Result<(), String> {
    if expected_identity.len() as u64 > MAX_RECORD {
        return Err("Repository identity is too large".into());
    }
    let current = identity(repo_path)?;
    if serde_json::to_string(&current).map_err(|e| e.to_string())? != expected_identity {
        return Err(
            "Repository changed while trust was being requested. Inspect and approve it again."
                .into(),
        );
    }
    if remember {
        save_grant(&persistent_root()?, &current)
    } else {
        let mut grants = sessions()
            .lock()
            .map_err(|_| "Repository trust state is unavailable")?;
        if grants.len() >= MAX_SESSION_GRANTS && !grants.contains_key(&current.checkout.path) {
            return Err("Too many session repository grants".into());
        }
        grants.insert(current.checkout.path.clone(), current);
        Ok(())
    }
}

/// Revocation blocks subsequent operations; already-started processes retain
/// the authority granted when they started and must be stopped separately.
pub fn revoke(repo_path: &str) -> Result<(), String> {
    let repo = crate::engine::git_cli::validate_repo_path(repo_path)?;
    sessions()
        .lock()
        .map_err(|_| "Repository trust state is unavailable")?
        .remove(&repo);
    match fs::remove_file(record_path(&persistent_root()?, &repo)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("Cannot revoke repository trust: {error}")),
    }
}

/// All bounded child processes share this last admission check. Global probes
/// get the executable volume's root, never an inherited repository directory.
/// A project tool's explicit directory is checked against its nearest repo.
pub(crate) fn check_command(cmd: &mut std::process::Command) -> Result<(), String> {
    if cmd.get_current_dir().is_none() {
        let executable = std::env::current_exe()
            .and_then(|path| path.canonicalize())
            .map_err(|error| format!("Cannot resolve a neutral process directory: {error}"))?;
        let root = executable
            .ancestors()
            .last()
            .ok_or("Missing executable volume root")?;
        cmd.current_dir(root);
    }
    if let Some(cwd) = cmd.get_current_dir() {
        if let Some(repo) = crate::engine::git_cli::find_git_root(cwd) {
            require(&repo)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn equal_creation_times_do_not_authorize_a_replaced_directory() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("checkout");
        fs::create_dir(&path).unwrap();
        let approved = directory(&path).unwrap();
        fs::rename(&path, root.path().join("original")).unwrap();
        fs::create_dir(&path).unwrap();
        let mut replacement = directory(&path).unwrap();
        // Creation timestamps are mutable on Windows. Even an exact match
        // must not make the replacement equal to the previously approved object.
        replacement.created_ns = approved.created_ns;
        assert_ne!(approved, replacement);
    }

    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let status = std::process::Command::new("git")
            .args(["init", "-q"])
            .arg(dir.path())
            .status()
            .unwrap();
        assert!(status.success());
        dir
    }

    #[test]
    fn persistent_records_require_a_complete_matching_identity() {
        let repo = fixture();
        let other = fixture();
        let storage = tempfile::tempdir().unwrap();
        let current = identity(repo.path().to_str().unwrap()).unwrap();
        assert!(!read_grant(storage.path(), &current).unwrap());
        save_grant(storage.path(), &current).unwrap();
        assert!(read_grant(storage.path(), &current).unwrap());
        let record = record_path(storage.path(), &current.checkout.path);
        for content in [
            "{".to_owned(),
            "null".to_owned(),
            "x".repeat(MAX_RECORD as usize + 1),
        ] {
            fs::write(&record, content).unwrap();
            assert!(read_grant(storage.path(), &current).is_err());
        }
        let mut changed = current.clone();
        changed.version = 2;
        fs::write(&record, serde_json::to_vec(&changed).unwrap()).unwrap();
        assert!(!read_grant(storage.path(), &current).unwrap());
        let foreign = identity(other.path().to_str().unwrap()).unwrap();
        fs::write(&record, serde_json::to_vec(&foreign).unwrap()).unwrap();
        assert!(!read_grant(storage.path(), &current).unwrap());
    }

    #[test]
    fn watcher_cannot_start_before_trust_and_can_stop_after_revocation() {
        let repo = fixture();
        let path = repo.path().to_str().unwrap();
        let state = crate::watcher::WatcherState::default();
        assert!(
            crate::watcher::start_watch_inner(&state, path.into(), |_| {})
                .unwrap_err()
                .contains(REQUIRED)
        );
        let preview = inspect(path).unwrap();
        grant(path, &preview.identity, false).unwrap();
        crate::watcher::start_watch_inner(&state, path.into(), |_| {}).unwrap();
        revoke(path).unwrap();
        crate::watcher::unwatch(&state, path.into()).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn symlink_trust_records_and_fifo_metadata_fail_closed_without_waiting() {
        use std::os::unix::fs::symlink;
        let repo = fixture();
        let storage = tempfile::tempdir().unwrap();
        let current = identity(repo.path().to_str().unwrap()).unwrap();
        let data = storage.path().join("forged");
        fs::write(&data, serde_json::to_vec(&current).unwrap()).unwrap();
        symlink(&data, record_path(storage.path(), &current.checkout.path)).unwrap();
        assert!(read_grant(storage.path(), &current).is_err());
        let pointer = repo.path().join(".git/commondir");
        let cpath = std::ffi::CString::new(pointer.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: cpath is a live NUL-terminated path in our owned fixture.
        assert_eq!(unsafe { libc::mkfifo(cpath.as_ptr(), 0o600) }, 0);
        let start = std::time::Instant::now();
        assert!(inspect(repo.path().to_str().unwrap()).is_err());
        assert!(start.elapsed() < std::time::Duration::from_secs(2));
    }
}
