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
/// A refusal that approving the repository cannot fix, so it must not carry
/// [`REQUIRED`]: the desktop reads that marker as "ask for approval and retry"
/// (`repoStore.openRepo`, `externalTools`), and a prompt whose approval
/// changes nothing is a loop, not a diagnosis. This one says the *name* of the
/// directory is writable by other principals, which is a filesystem change to
/// make, not a decision to take.
pub const SHARED_DIRECTORY: &str = "REPOSITORY_DIRECTORY_IS_SHARED";
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
    require_identified(repo).map(|_| ())
}

/// [`require`], returning the identity it validated.
///
/// Admission needs the identity itself, not just the verdict: it has to prove
/// that the object this grant was checked against is the same object the child
/// will be anchored to. Re-reading the path for that would be one more lookup a
/// rename could redirect, so the single sample that was approved is the one
/// that gets compared.
fn require_identified(repo: &Path) -> Result<Identity, String> {
    let current = identity(repo.to_str().ok_or("Repository path is not UTF-8")?)?;
    if trusted(&current)? {
        return Ok(current);
    }
    Err(format!("{REQUIRED}: Open {} in GitPulse and explicitly trust this checkout before running repository commands.", current.checkout.path.display()))
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

/// Whatever must outlive admission for the spawn to land where it was
/// admitted, so callers hold it across `spawn` on every platform.
///
/// What that amounts to differs, and the empty Unix shape is not an oversight:
/// there the guarantee travels inside the `Command` itself, as a `pre_exec`
/// closure owning the pinned descriptor, so nothing is left for the caller to
/// keep alive. On Windows it is these directory handles, whose share mode is
/// the only thing stopping a component of the path from being renamed while
/// the child starts.
pub(crate) struct AdmittedCwd {
    #[cfg(windows)]
    _pinned: Vec<std::fs::File>,
}

/// All bounded child processes share this last admission check. Global probes
/// get the executable volume's root, never an inherited repository directory.
/// A project tool's explicit directory is checked against its nearest repo.
///
/// The directory is *pinned*, not merely inspected. `Command::current_dir`
/// stores a path and the operating system resolves it again at spawn, so
/// admitting the object a path names and then letting the spawn re-walk that
/// path authorizes one directory and runs in another: renaming any component
/// and leaving a symlink in its place redirects the child into a checkout that
/// was never approved, whose `core.fsmonitor` and aliases then execute. Every
/// directory is pinned, not only one already inside a repository — otherwise
/// the same substitution turns an unremarkable working directory into a
/// repository after the point where admission decided none was involved.
pub(crate) fn check_command(cmd: &mut std::process::Command) -> Result<AdmittedCwd, String> {
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
    let cwd = cmd
        .get_current_dir()
        .ok_or("Missing process directory")?
        .to_path_buf();
    admit_directory(cmd, &cwd)
}

/// Resolve a directory, pin it, prove the pinned object is still the one the
/// path names, and require trust for whatever repository holds it.
///
/// One owner for that sequence: both the process seam and the PTY seam need
/// exactly it, and they differ only in what they can do about a path that
/// another principal could still substitute afterwards.
#[cfg(unix)]
fn pin_and_admit(cwd: &Path) -> Result<crate::fs_entry::PinnedDir, String> {
    use std::os::unix::fs::MetadataExt;
    // Canonicalize first. A canonical path holds no symlink by construction,
    // so the no-follow walk below accepts every legitimate location — macOS
    // temporary directories under `/var`, a symlinked home — and refuses only
    // a component substituted *after* this resolution, which is the attack.
    let resolved = cwd
        .canonicalize()
        .map_err(|error| format!("Cannot resolve {}: {error}", cwd.display()))?;
    let pinned = crate::fs_entry::pin_dir_nofollow(&resolved)
        .map_err(|error| format!("Cannot pin process directory {}: {error}", cwd.display()))?;
    let held = pinned.dir.metadata().map_err(|e| e.to_string())?;
    let named = fs::metadata(&resolved).map_err(|e| e.to_string())?;
    if (held.dev(), held.ino()) != (named.dev(), named.ino()) {
        return Err("Process directory changed while it was being admitted".into());
    }
    let approved = match crate::engine::git_cli::find_git_root(&resolved) {
        Some(repo) => Some(require_identified(&repo)?),
        None => None,
    };
    if !pinned.exclusive {
        // The child will be anchored to the pinned descriptor rather than to
        // the path, so the repository that was trusted has to be the pinned
        // object's OWN repository. `find_git_root` answers about the path, and
        // between the pin and that walk a rename can make the two describe
        // different trees — a swap that is reverted before the walk leaves the
        // descriptor inside an untrusted checkout while the path shows a
        // trusted one, or none at all. Reading it back through the descriptor
        // is the only answer a rename cannot reach.
        //
        // Paid only on this path, like the re-anchor itself: where every
        // component is beyond other users' reach the two walks cannot diverge,
        // and the ancestor walk would cost the hot `git` seam for nothing.
        let anchored = crate::fs_entry::repository_identity_of_pin(&pinned.dir)
            .map_err(|error| format!("Cannot identify the pinned repository: {error}"))?;
        let trusted = approved
            .as_ref()
            .map(|identity| (identity.checkout.device, identity.checkout.inode));
        if anchored != trusted {
            return Err(
                "Process directory changed while it was being admitted: the approved repository \
                 is not the one this process would run in"
                    .into(),
            );
        }
    }
    Ok(pinned)
}

/// Admit a directory for an interface that cannot re-anchor its own child.
///
/// A `std::process::Command` can be handed the pinned descriptor and corrected
/// after the fact; a PTY cannot. `portable_pty` builds the command itself,
/// owns the only `pre_exec` hook on it, and exposes no slave descriptor, so
/// the child's working directory comes from resolving the path and nothing can
/// fix it afterwards. Where the path is provably beyond another principal's
/// reach that resolution is safe. Where it is not, this refuses: starting a
/// long-lived interactive session in a directory that may have been swapped
/// between approval and launch is the one outcome the gate exists to prevent,
/// and there is no correcting hook to fall back on.
pub(crate) fn require_unsubstitutable_dir(dir: &Path) -> Result<AdmittedCwd, String> {
    #[cfg(unix)]
    {
        let pinned = pin_and_admit(dir)?;
        if let Some(holder) = pinned.shared_holder {
            return Err(format!(
                "{SHARED_DIRECTORY}: cannot start a session in {} because {} is writable by other \
                 users, so that path can be pointed at a different directory after this checkout \
                 is approved. Approving the repository again will not change this — remove group \
                 and other write access from that directory, or set its sticky bit.",
                dir.display(),
                holder.display()
            ));
        }
        Ok(AdmittedCwd {})
    }
    #[cfg(windows)]
    {
        // Windows holds the path open instead: the pinned chain omits
        // FILE_SHARE_DELETE, so no component can be renamed while the caller
        // keeps this guard alive across the spawn.
        admit_directory_windows(dir)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = dir;
        Err("Process admission cannot pin a directory on this platform".into())
    }
}

/// The pinned descriptor and the path must name the same object, or a
/// component moved between the two resolutions and neither answer describes
/// what would run.
#[cfg(unix)]
fn admit_directory(cmd: &mut std::process::Command, cwd: &Path) -> Result<AdmittedCwd, String> {
    use std::os::fd::AsRawFd;
    use std::os::unix::process::CommandExt;
    let pinned = pin_and_admit(cwd)?;
    if pinned.exclusive {
        // Nobody outside this user can rename a component of this path, so the
        // spawn's own resolution reaches the object just admitted. Re-anchoring
        // anyway would buy nothing and cost the `posix_spawn` fast path: a
        // `pre_exec` closure forces `fork` + `exec` of a process holding the
        // parent's whole address space, measured at +123% per spawn by
        // `benches/process_spawn.rs`, on the seam every `git` call goes through.
        return Ok(AdmittedCwd {});
    }
    // A shared ancestor can be substituted by someone else between here and the
    // spawn, so stop trusting the path and hand the child the descriptor.
    let dir = pinned.dir;
    // SAFETY: `fchdir` is async-signal-safe and the closure allocates nothing,
    // takes no lock, and touches no other state — the only requirements between
    // `fork` and `exec`. The descriptor stays open until exec closes it.
    unsafe {
        cmd.pre_exec(move || {
            if libc::fchdir(dir.as_raw_fd()) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    Ok(AdmittedCwd {})
}

#[cfg(windows)]
fn admit_directory(_cmd: &mut std::process::Command, cwd: &Path) -> Result<AdmittedCwd, String> {
    admit_directory_windows(cwd)
}

#[cfg(windows)]
fn admit_directory_windows(cwd: &Path) -> Result<AdmittedCwd, String> {
    // Canonical first, for the reason the Unix arm gives: `pin_directory`
    // refuses a reparse point, and a junction anywhere in the caller's spelling
    // is ordinary, not hostile.
    let resolved = cwd
        .canonicalize()
        .map_err(|error| format!("Cannot resolve {}: {error}", cwd.display()))?;
    let pinned = crate::fs_entry::pin_dir_chain(&resolved, false)
        .map_err(|error| format!("Cannot pin process directory {}: {error}", cwd.display()))?;
    let leaf = pinned.last().ok_or("Missing pinned process directory")?;
    let held = crate::fs_entry::windows_file_identity(leaf)
        .map_err(|error| format!("Cannot identify process directory: {error}"))?;
    let named = crate::fs_entry::pin_directory(&resolved)
        .and_then(|handle| crate::fs_entry::windows_file_identity(&handle))
        .map_err(|error| format!("Cannot identify process directory: {error}"))?;
    if held != named {
        return Err("Process directory changed while it was being admitted".into());
    }
    if let Some(repo) = crate::engine::git_cli::find_git_root(&resolved) {
        require(&repo)?;
    }
    // The handles stay open across the spawn: without FILE_SHARE_DELETE no
    // component of this path can be renamed or removed while the child starts.
    // That also removes the reason the Unix arm reads the repository back
    // through the pinned descriptor: the pin is taken before this trust check,
    // and nothing can rename a component while it is held, so the path cannot
    // come to mean something else between the two reads.
    Ok(AdmittedCwd { _pinned: pinned })
}

#[cfg(not(any(unix, windows)))]
fn admit_directory(_cmd: &mut std::process::Command, _cwd: &Path) -> Result<AdmittedCwd, String> {
    Err("Process admission cannot pin a directory on this platform".into())
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

    /// THE CLASS: admission resolved a path, the spawn resolved it again, and
    /// only the second one decided where the child actually ran. Substituting
    /// the directory in between authorized one checkout and executed another —
    /// whose `core.fsmonitor` and aliases are exactly what the gate exists to
    /// keep from running. No race is needed to prove it: the substitution
    /// happens between the two calls the real seam makes back to back.
    ///
    /// The holding directory is world-writable, which is precisely when
    /// another principal could do this and therefore when admission stops
    /// trusting the path and hands the child a descriptor instead.
    #[cfg(unix)]
    #[test]
    fn a_substituted_directory_cannot_redirect_an_admitted_child() {
        use std::os::unix::fs::PermissionsExt;
        let base = tempfile::tempdir().unwrap();
        let base = base.path().canonicalize().unwrap();
        fs::set_permissions(&base, fs::Permissions::from_mode(0o777)).unwrap();
        let admitted_path = base.join("admitted");
        let elsewhere = base.join("elsewhere");
        fs::create_dir(&admitted_path).unwrap();
        fs::create_dir(&elsewhere).unwrap();

        let mut cmd = std::process::Command::new("/bin/pwd");
        cmd.current_dir(&admitted_path);
        // Named, not dropped: on Windows this holds the directory handles open
        // across the spawn; on Unix the re-anchor lives in the command itself.
        let _admission = check_command(&mut cmd).expect("a plain directory is admitted");

        // The directory that was admitted keeps its identity under a new name;
        // its old name now points somewhere that was never admitted.
        let moved = base.join("moved");
        fs::rename(&admitted_path, &moved).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &admitted_path).unwrap();

        let output = cmd.output().expect("spawn pwd");
        let ran_in = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim())
            .canonicalize()
            .unwrap();
        assert_eq!(
            ran_in, moved,
            "the child ran in a directory that was never admitted"
        );
        assert_ne!(ran_in, elsewhere.canonicalize().unwrap());
    }

    /// The fast path is taken on exactly one condition, so that condition is
    /// asserted directly rather than inferred from a timing difference: a
    /// private chain is exclusive, and one world-writable ancestor is enough
    /// to lose it.
    #[cfg(unix)]
    /// No-narrowing for the strict path, and the one that would bite hardest
    /// if the descriptor-side repository walk disagreed with `find_git_root`:
    /// an ordinary trusted checkout under a world-writable holder must still
    /// be admitted, from its root and from a subdirectory, every single time.
    ///
    /// The two walks answer the same question by different means, so a
    /// mismatch in how either reads `.git` — a worktree's `.git` file, a
    /// symlink, a bare layout — would show up here as a refusal of a
    /// repository nobody was attacking.
    #[cfg(unix)]
    #[test]
    fn a_trusted_checkout_under_a_shared_holder_is_still_admitted_every_time() {
        use std::os::unix::fs::PermissionsExt;
        let base = tempfile::tempdir().unwrap();
        let base = base.path().canonicalize().unwrap();
        let holder = base.join("holder");
        fs::create_dir(&holder).unwrap();
        let checkout = holder.join("checkout");
        fs::create_dir(&checkout).unwrap();
        let nested = checkout.join("src").join("deep");
        fs::create_dir_all(&nested).unwrap();
        let init = std::process::Command::new("git")
            .args(["init", "-q", "-b", "main"])
            .current_dir(&checkout)
            .status()
            .expect("git init");
        assert!(init.success());
        crate::test_support::trust_repo(&checkout);
        fs::set_permissions(&holder, fs::Permissions::from_mode(0o777)).unwrap();

        assert!(
            !crate::fs_entry::pin_dir_nofollow(&checkout)
                .unwrap()
                .exclusive,
            "the layout must put admission on the strict path, or this proves nothing"
        );

        for cwd in [&checkout, &nested] {
            for attempt in 0..25 {
                let mut cmd = std::process::Command::new("/bin/pwd");
                cmd.current_dir(cwd);
                let _admission = check_command(&mut cmd).unwrap_or_else(|error| {
                    panic!(
                        "attempt {attempt} refused a trusted checkout at {}: {error}",
                        cwd.display()
                    )
                });
                let output = cmd.output().expect("spawn pwd");
                let ran_in = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim())
                    .canonicalize()
                    .unwrap();
                assert_eq!(&ran_in, cwd, "attempt {attempt} landed elsewhere");
            }
        }
    }

    #[test]
    fn only_a_privately_held_path_skips_the_child_side_re_anchor() {
        use std::os::unix::fs::PermissionsExt;
        let base = tempfile::tempdir().unwrap();
        let base = base.path().canonicalize().unwrap();
        let nested = base.join("outer").join("inner");
        fs::create_dir_all(&nested).unwrap();
        assert!(
            crate::fs_entry::pin_dir_nofollow(&nested)
                .unwrap()
                .exclusive,
            "a chain under a private temporary directory is reachable only by this user"
        );
        fs::set_permissions(base.join("outer"), fs::Permissions::from_mode(0o777)).unwrap();
        assert!(
            !crate::fs_entry::pin_dir_nofollow(&nested)
                .unwrap()
                .exclusive,
            "a world-writable holder lets another user rename the component under it"
        );
        // The leaf's own permissions are not what decides this: renaming an
        // entry needs write on the directory holding it.
        fs::set_permissions(base.join("outer"), fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(&nested, fs::Permissions::from_mode(0o777)).unwrap();
        assert!(
            crate::fs_entry::pin_dir_nofollow(&nested)
                .unwrap()
                .exclusive
        );
    }

    /// Pinning must not narrow what is admissible: a repository reached
    /// through a symlinked ancestor is ordinary, and the no-follow walk runs
    /// on the canonical path precisely so it stays that way.
    #[cfg(unix)]
    #[test]
    fn a_directory_reached_through_a_symlinked_ancestor_is_still_admitted() {
        let base = tempfile::tempdir().unwrap();
        let real = base.path().join("real");
        fs::create_dir_all(real.join("inner")).unwrap();
        let alias = base.path().join("alias");
        std::os::unix::fs::symlink(&real, &alias).unwrap();

        let mut cmd = std::process::Command::new("/bin/pwd");
        cmd.current_dir(alias.join("inner"));
        let _admission = check_command(&mut cmd).expect("a symlinked ancestor is not an attack");
        let output = cmd.output().expect("spawn pwd");
        assert_eq!(
            PathBuf::from(String::from_utf8_lossy(&output.stdout).trim())
                .canonicalize()
                .unwrap(),
            real.join("inner").canonicalize().unwrap()
        );
    }

    /// Load-bearing for the whole admission design, not just for grants:
    /// discovery walks a path to find the repository that must be trusted, and
    /// that walk races anything able to rewrite the tree. It stays harmless
    /// only because approval is bound to a *location as well as an object* —
    /// so no attacker can move an already-approved checkout into the path
    /// being walked and have the grant come with it.
    #[test]
    fn a_grant_does_not_travel_with_the_directory_it_approved() {
        let parent = tempfile::tempdir().unwrap();
        let original = parent.path().join("approved");
        fs::create_dir(&original).unwrap();
        let status = std::process::Command::new("git")
            .args(["init", "-q"])
            .arg(&original)
            .status()
            .unwrap();
        assert!(status.success());

        let path = original.to_str().unwrap();
        let preview = inspect(path).unwrap();
        grant(path, &preview.identity, false).unwrap();
        require(&original).expect("the approved checkout is trusted where it was approved");

        let moved = parent.path().join("relocated");
        fs::rename(&original, &moved).unwrap();
        assert!(
            require(&moved).is_err(),
            "the grant followed the directory to a path that was never approved"
        );
        assert!(
            require(&original).is_err(),
            "a vanished path still answered as trusted"
        );
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
