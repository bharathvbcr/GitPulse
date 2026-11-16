//! Bounded snapshots and descriptor-relative removal of exactly reviewed entries.
use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

const MAX_ENTRIES: usize = 250_000;
const MAX_DEPTH: usize = 48;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stamp {
    pub directory: bool,
    pub len: u64,
    pub modified: u128,
    pub identity: (u64, u64),
    pub changed: (i64, i64),
}

impl Stamp {
    fn read(meta: &fs::Metadata) -> Result<Self, String> {
        #[cfg(unix)]
        let (identity, changed) = ((meta.dev(), meta.ino()), (meta.ctime(), meta.ctime_nsec()));
        #[cfg(not(unix))]
        let (identity, changed) = ((0, 0), (0, 0));
        Ok(Self {
            directory: meta.is_dir(),
            len: meta.len(),
            identity,
            changed,
            modified: meta
                .modified()
                .map_err(|e| e.to_string())?
                .duration_since(UNIX_EPOCH)
                .map_err(|e| e.to_string())?
                .as_nanos(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub entries: BTreeMap<PathBuf, Stamp>,
    pub bytes: u64,
    pub newest: u128,
}

pub fn relative_path(value: &str) -> Result<&Path, String> {
    let path = Path::new(value);
    if value.is_empty()
        || value.len() > 4096
        || value.contains('\\')
        || value.chars().any(char::is_control)
        || path
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err("Expected a normal repository-relative directory path".into());
    }
    Ok(path)
}

pub fn no_symlinks(path: &Path) -> Result<(), String> {
    no_symlinks_checked(path).map_err(|e| e.message())
}

/// [`no_symlinks`] keeping apart a symlink on the way (`Refused`: an answer)
/// from a component that could not be inspected (`CouldNotRead`: no answer).
pub fn no_symlinks_checked(path: &Path) -> Result<(), WalkError> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component);
        // A volume prefix is not a filesystem object. Stating `C:` or
        // `\\?\C:` is ERROR_INVALID_FUNCTION (os error 1) on Windows, which
        // made every hygiene fixture that canonicalize()'d a temp path fail
        // before any policy ran. The volume RootDir (`C:\`, and `\\?\C:\`
        // after canonicalize) is the same class: it is never a user symlink
        // and stating the verbatim form is also os error 1. Each Normal
        // component is still checked.
        if matches!(component, Component::Prefix(_)) {
            continue;
        }
        #[cfg(windows)]
        if matches!(component, Component::RootDir) {
            continue;
        }
        if fs::symlink_metadata(&current)
            .map_err(|e| WalkError::io(&current, &e))?
            .file_type()
            .is_symlink()
        {
            return Err(WalkError::Refused(format!(
                "Symlink paths are not eligible: {}",
                current.display()
            )));
        }
    }
    Ok(())
}

/// What a walk may spend before it gives up.
#[derive(Debug, Clone, Copy)]
pub struct Budget {
    pub entries: usize,
    pub seconds: u64,
}

impl Budget {
    /// The default for every candidate: 250,000 entries or 15 seconds.
    pub const STANDARD: Budget = Budget {
        entries: MAX_ENTRIES,
        seconds: 15,
    };
}

/// The error a walk returns when it spent its budget. A caller that wants to
/// say "too large to verify" rather than "refused" compares against this.
pub const BUDGET_EXCEEDED: &str =
    "Measurement incomplete: traversal budget exceeded. Cleanup is unavailable.";
/// The walk's clock ran out.
pub const TIME_EXCEEDED: &str =
    "Measurement incomplete: traversal time budget exceeded. Cleanup is unavailable.";

/// Why a walk did not produce a snapshot. The kinds are different facts: a
/// rule refused the tree (`Refused`), the walk could not look (`CouldNotRead`,
/// `Cancelled`), or it was too large for its budget (`Budget`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WalkError {
    /// What was found inside is not eligible: a symlink, a special file, a
    /// protected name.
    Refused(String),
    /// An entry could not be listed or read, for a reason that may clear.
    CouldNotRead(String),
    /// The operating system said this process may not read an entry
    /// (permission denied). That is a lasting property of the folder: an
    /// answer about it, not an unfinished look.
    Denied(String),
    /// The walk visited more entries (or went deeper) than its budget: the
    /// tree is too large to verify, an answer about the tree.
    Budget,
    /// The walk ran out of time before it finished: not an answer about the
    /// tree, only that looking was not completed.
    TimedOut,
    Cancelled,
}

impl WalkError {
    /// A read failure of `path`, kept apart by whether it can clear.
    fn io(path: &Path, error: &std::io::Error) -> Self {
        let message = format!("{}: {error}", path.display());
        if error.kind() == std::io::ErrorKind::PermissionDenied {
            Self::Denied(message)
        } else {
            Self::CouldNotRead(message)
        }
    }
}

impl From<String> for WalkError {
    fn from(message: String) -> Self {
        Self::CouldNotRead(message)
    }
}

impl WalkError {
    pub fn message(&self) -> String {
        match self {
            Self::Refused(message) | Self::CouldNotRead(message) | Self::Denied(message) => {
                message.clone()
            }
            Self::Budget => BUDGET_EXCEEDED.into(),
            Self::TimedOut => TIME_EXCEEDED.into(),
            Self::Cancelled => "Cancelled".into(),
        }
    }
}

pub fn snapshot(root: &Path, strict: bool, cancel: &AtomicBool) -> Result<Snapshot, String> {
    snapshot_within(root, strict, cancel, Budget::STANDARD).map_err(|e| e.message())
}

/// [`snapshot`] with an explicit budget. A snapshot holds one map entry per
/// file and directory (a relative path and a 64-byte stamp, about 250 bytes
/// with the map's own overhead), so the entry budget is also the memory bound.
pub fn snapshot_within(
    root: &Path,
    strict: bool,
    cancel: &AtomicBool,
    budget: Budget,
) -> Result<Snapshot, WalkError> {
    let mut result = Snapshot {
        entries: BTreeMap::new(),
        bytes: 0,
        newest: 0,
    };
    let deadline = Instant::now() + Duration::from_secs(budget.seconds);
    #[cfg(unix)]
    let mut inodes = Inodes::new();
    #[cfg(not(unix))]
    let mut inodes: Inodes = ();
    walk(
        root,
        Path::new(""),
        strict,
        (deadline, budget.entries),
        cancel,
        &mut result,
        &mut inodes,
    )?;
    #[cfg(unix)]
    {
        result.bytes = inodes
            .values()
            .filter(|file| file.links_in_tree >= file.links)
            .fold(result.bytes, |sum, file| sum.saturating_add(file.len));
    }
    Ok(result)
}

/// What the walk saw of each distinct file (device, inode): its size, how many
/// names the filesystem says it has, and how many of them are inside the tree.
/// A Cargo target hard-links heavily (an uplifted binary beside its `deps`
/// copy, `incremental` beside `build`), so bytes are counted per file, not per
/// name, and only for a file every name of which is inside the tree: removing
/// the tree frees nothing for a file that keeps a name outside it.
#[cfg(unix)]
struct SeenFile {
    len: u64,
    links: u64,
    links_in_tree: u64,
}
#[cfg(unix)]
type Inodes = std::collections::HashMap<(u64, u64), SeenFile>;
/// Nothing to track without inode identities.
#[cfg(not(unix))]
type Inodes = ();

/// The first entry of a snapshot that lives on another device than its root: a
/// mount point inside the candidate. Removing the candidate would reach into a
/// different filesystem, which nothing reviewed. Pure over the snapshot, so a
/// test hands it entries with other device numbers.
pub fn crossing_device(snapshot: &Snapshot) -> Option<&Path> {
    let root = snapshot.entries.get(Path::new(""))?.identity.0;
    snapshot
        .entries
        .iter()
        .find(|(_, stamp)| stamp.identity.0 != root)
        .map(|(path, _)| path.as_path())
}

fn walk(
    root: &Path,
    relative: &Path,
    strict: bool,
    (deadline, max_entries): (Instant, usize),
    cancel: &AtomicBool,
    result: &mut Snapshot,
    inodes: &mut Inodes,
) -> Result<(), WalkError> {
    if cancel.load(Ordering::Relaxed) {
        return Err(WalkError::Cancelled);
    }
    if result.entries.len() >= max_entries || relative.components().count() > MAX_DEPTH {
        return Err(WalkError::Budget);
    }
    if Instant::now() >= deadline {
        return Err(WalkError::TimedOut);
    }
    // `join("")` appends a separator, and a trailing slash makes lstat follow
    // a symlinked root: the walk then measured the target as if it were the root.
    let path = if relative.as_os_str().is_empty() {
        root.to_path_buf()
    } else {
        root.join(relative)
    };
    let meta = fs::symlink_metadata(&path).map_err(|e| WalkError::io(&path, &e))?;
    if meta.file_type().is_symlink() {
        // A symlinked root measures nothing: returning an empty snapshot made
        // the inventory report 0 bytes for a cache it never looked at.
        if relative.as_os_str().is_empty() {
            return Err(WalkError::Refused(format!(
                "{} is a symlink; it was not measured",
                path.display()
            )));
        }
        if strict {
            return Err(WalkError::Refused(format!(
                "Symlink inside candidate: {}",
                relative.display()
            )));
        }
        return Ok(());
    }
    if !meta.is_file() && !meta.is_dir() {
        return Err(WalkError::Refused(
            "Special files inside candidate; cleanup is unavailable".into(),
        ));
    }
    if strict && !relative.as_os_str().is_empty() {
        if devmap_query::hygiene::protected_entry_in(root, relative) {
            return Err(WalkError::Refused(format!(
                "Protected content inside candidate: {}",
                relative.display()
            )));
        }
        if root.file_name().is_some_and(|n| n == "__pycache__")
            && (!meta.is_file()
                || !matches!(
                    path.extension().and_then(|s| s.to_str()),
                    Some("pyc" | "pyo")
                ))
        {
            return Err(WalkError::Refused(
                "Python bytecode directory contains non-bytecode content".into(),
            ));
        }
    }
    let stamp = Stamp::read(&meta)?;
    result.newest = result.newest.max(stamp.modified);
    if meta.is_file() {
        #[cfg(unix)]
        {
            let seen = inodes.entry(stamp.identity).or_insert(SeenFile {
                len: meta.len(),
                links: meta.nlink().max(1),
                links_in_tree: 0,
            });
            seen.links_in_tree += 1;
        }
        // No inode identity here: every name counts as its own file.
        #[cfg(not(unix))]
        {
            let _ = &inodes;
            result.bytes = result.bytes.saturating_add(meta.len());
        }
    }
    result.entries.insert(relative.to_path_buf(), stamp);
    if meta.is_dir() {
        for entry in fs::read_dir(&path).map_err(|e| WalkError::io(&path, &e))? {
            let entry = entry.map_err(|e| WalkError::io(&path, &e))?;
            walk(
                root,
                &relative.join(entry.file_name()),
                strict,
                (deadline, max_entries),
                cancel,
                result,
                inodes,
            )?;
        }
    }
    Ok(())
}

pub fn require_age(snapshot: &Snapshot, days: u32, now: SystemTime) -> Result<(), String> {
    devmap_query::hygiene::validate_retention(days, false)?;
    let now = now
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    let age = u128::from(days) * 86_400 * 1_000_000_000;
    if now < snapshot.newest.saturating_add(age) {
        return Err(format!(
            "Used or modified within {days} day(s). Keep it until the retention period passes."
        ));
    }
    Ok(())
}

#[cfg(unix)]
mod unix {
    use super::{PartialRemoval, Snapshot, Stamp};
    use std::ffi::CString;
    use std::fs::File;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;
    use std::path::{Component, Path};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    fn open_at(parent: &File, name: &std::ffi::OsStr) -> Result<File, String> {
        let name = CString::new(name.as_bytes()).map_err(|e| e.to_string())?;
        // SAFETY: owned, NUL-terminated name; parent stays alive. Every hop
        // refuses symlinks. The returned descriptor is owned exactly once.
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    fn descend(root: &File, path: &Path) -> Result<File, String> {
        let mut current = root.try_clone().map_err(|e| e.to_string())?;
        for part in path.components() {
            let Component::Normal(name) = part else {
                return Err("Invalid descriptor path".into());
            };
            current = open_at(&current, name)?;
        }
        Ok(current)
    }

    /// Whether a file still matches its stamp from the preview. Its size must
    /// match. A file none of whose names this removal has unlinked must also
    /// have the same change time. One it has (another name of the same file)
    /// has a new change time because of that unlink, so the change time is
    /// excused and the modification time, which our unlink does not move, is
    /// checked instead: a write after the preview still stops the removal.
    pub(super) fn file_unchanged(
        approved: &Stamp,
        size: i64,
        changed: (i64, i64),
        modified: u128,
        other_name_unlinked: bool,
    ) -> bool {
        if u64::try_from(size).ok() != Some(approved.len) {
            return false;
        }
        if other_name_unlinked {
            modified == approved.modified
        } else {
            changed == approved.changed
        }
    }

    /// Removes the reviewed entries. A failure after some were already
    /// unlinked says so: the directory is then partly gone, and "Entry was
    /// replaced" alone reads as if nothing had happened.
    pub fn remove(
        root: &Path,
        expected: &Snapshot,
        cancel: &AtomicBool,
    ) -> Result<u64, PartialRemoval> {
        let mut removed = 0usize;
        let mut bytes = 0u64;
        remove_counted(root, expected, cancel, &mut removed, &mut bytes)
            .map(|()| bytes)
            .map_err(|error| PartialRemoval {
                error: if removed == 0 || error.contains("already have been removed") {
                    error
                } else {
                    format!("{error}; {removed} reviewed entries were already removed")
                },
                entries: removed,
                bytes,
            })
    }

    fn remove_counted(
        root: &Path,
        expected: &Snapshot,
        cancel: &AtomicBool,
        removed: &mut usize,
        removed_bytes: &mut u64,
    ) -> Result<(), String> {
        let slash = File::open("/").map_err(|e| e.to_string())?;
        let absolute = root.strip_prefix("/").map_err(|e| e.to_string())?;
        let parent = descend(&slash, absolute.parent().ok_or("Missing parent")?)?;
        let name = absolute
            .file_name()
            .ok_or("Cannot remove filesystem root")?;
        let held = open_at(&parent, name)?;
        let first = expected
            .entries
            .get(Path::new(""))
            .ok_or("Missing reviewed directory")?;
        if Stamp::read(&held.metadata().map_err(|e| e.to_string())?)? != *first {
            return Err("Directory changed after preview".into());
        }
        // 60 seconds, and 1 more for every 5,000 reviewed entries: a build
        // directory the agent scopes may reach holds several hundred thousand,
        // and a run that stops half way leaves a partly removed folder.
        let deadline = Instant::now()
            + Duration::from_secs(60 + u64::try_from(expected.entries.len() / 5_000).unwrap_or(0));
        // Files of which this removal has already unlinked a name.
        let mut unlinked_files: std::collections::HashSet<(u64, u64)> =
            std::collections::HashSet::new();
        // Children sort after their parents in BTreeMap; reverse traversal
        // deletes only reviewed leaves, then empty directories. New entries
        // survive and cause ENOTEMPTY, never an unreviewed recursive sweep.
        for (relative, approved) in expected.entries.iter().rev() {
            if cancel.load(Ordering::Relaxed) {
                return Err(
                    "Cleanup cancelled; some reviewed entries may already have been removed".into(),
                );
            }
            if Instant::now() >= deadline {
                return Err(
                    "Cleanup deadline reached; some reviewed entries may already have been removed"
                        .into(),
                );
            }
            let (container, leaf) = if relative.as_os_str().is_empty() {
                (parent.try_clone().map_err(|e| e.to_string())?, name)
            } else {
                (
                    descend(&held, relative.parent().ok_or("Missing parent")?)?,
                    relative.file_name().ok_or("Missing name")?,
                )
            };
            let leaf = CString::new(leaf.as_bytes()).map_err(|e| e.to_string())?;
            // SAFETY: fstatat initializes the supplied stat on success and
            // AT_SYMLINK_NOFOLLOW prevents following a substituted leaf.
            let mut raw = std::mem::MaybeUninit::<libc::stat>::uninit();
            if unsafe {
                libc::fstatat(
                    container.as_raw_fd(),
                    leaf.as_ptr(),
                    raw.as_mut_ptr(),
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            } != 0
            {
                return Err(std::io::Error::last_os_error().to_string());
            }
            let raw = unsafe { raw.assume_init() };
            #[allow(clippy::unnecessary_cast)]
            let identity = (raw.st_dev as u64, raw.st_ino as u64);
            if identity != approved.identity
                || (raw.st_mode & libc::S_IFMT == libc::S_IFDIR) != approved.directory
            {
                return Err(
                    "Entry was replaced after preview; stopped without following it".into(),
                );
            }
            #[allow(clippy::unnecessary_cast)]
            let changed = (raw.st_ctime as i64, raw.st_ctime_nsec as i64);
            let modified = u128::try_from(
                i128::from(raw.st_mtime) * 1_000_000_000 + i128::from(raw.st_mtime_nsec),
            )
            .unwrap_or(0);
            // A file that has several names changes its ctime when another
            // name is unlinked. If this removal has already unlinked a name of
            // this very file, that is expected and everything else is checked.
            if !approved.directory
                && !file_unchanged(
                    approved,
                    raw.st_size,
                    changed,
                    modified,
                    unlinked_files.contains(&identity),
                )
            {
                return Err(
                    "File content changed after preview; stopped before removing it".into(),
                );
            }
            let frees_it = raw.st_nlink <= 1;
            let has_other_names = raw.st_nlink > 1;
            // SAFETY: the leaf is relative to an open, non-symlink parent.
            // unlinkat never traverses the final component, even in a race.
            if unsafe {
                libc::unlinkat(
                    container.as_raw_fd(),
                    leaf.as_ptr(),
                    if approved.directory {
                        libc::AT_REMOVEDIR
                    } else {
                        0
                    },
                )
            } != 0
            {
                return Err(std::io::Error::last_os_error().to_string());
            }
            *removed += 1;
            if !approved.directory {
                // Only the unlink of a file's last name frees it: a file that
                // keeps a name outside the tree is not freed, and a file with
                // several names inside is counted once, on its last.
                if frees_it {
                    *removed_bytes = removed_bytes.saturating_add(approved.len);
                }
                if has_other_names {
                    unlinked_files.insert(identity);
                }
            }
        }
        Ok(())
    }
}

/// A removal that stopped before it finished: why, and what it had already
/// unlinked. The bytes are real and gone whatever the error, so a caller that
/// budgets a run charges them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartialRemoval {
    pub error: String,
    /// Reviewed entries already unlinked.
    pub entries: usize,
    /// Bytes of the files among them.
    pub bytes: u64,
}

/// Removes the reviewed entries and says how many bytes it removed, on failure
/// too.
pub fn remove_reviewed_counted(
    root: &Path,
    expected: &Snapshot,
    cancel: &AtomicBool,
) -> Result<u64, PartialRemoval> {
    #[cfg(unix)]
    {
        unix::remove(root, expected, cancel)
    }
    #[cfg(not(unix))]
    {
        let _ = (root, expected, cancel);
        Err(PartialRemoval {
            error:
                "Local artifact cleanup is unavailable on this platform; use the native build tool"
                    .into(),
            entries: 0,
            bytes: 0,
        })
    }
}

pub fn remove_reviewed(
    root: &Path,
    expected: &Snapshot,
    cancel: &AtomicBool,
) -> Result<(), String> {
    remove_reviewed_counted(root, expected, cancel)
        .map(|_| ())
        .map_err(|partial| partial.error)
}

#[cfg(test)]
mod tests {
    use super::{no_symlinks, remove_reviewed, snapshot};
    use std::fs;
    use std::path::Path;
    use std::sync::atomic::AtomicBool;

    /// Cargo's own layout: scratch `tmp`, a build-script directory named like a
    /// crate (`log`), and a `--target <triple>` tree with its own `tmp`.
    fn cargo_layout(root: &Path, marked: bool) {
        for dir in [
            "tmp",
            "debug/build/log/0123abcd",
            "aarch64-apple-darwin/debug",
            "aarch64-apple-darwin/tmp",
        ] {
            fs::create_dir_all(root.join(dir)).unwrap();
        }
        for file in [
            "tmp/x.txt",
            "debug/build/log/0123abcd/out",
            "aarch64-apple-darwin/debug/.cargo-lock",
            "aarch64-apple-darwin/tmp/y.txt",
        ] {
            fs::write(root.join(file), "x").unwrap();
        }
        if marked {
            fs::write(
                root.join("CACHEDIR.TAG"),
                "Signature: 8a477f597d28d172789f06886806bc55",
            )
            .unwrap();
            fs::write(root.join(".rustc_info.json"), "{}").unwrap();
        }
    }

    fn walk(root: &Path) -> Result<super::Snapshot, String> {
        snapshot(root, true, &AtomicBool::new(false))
    }

    /// The walk passes its own root to the name protection, so Cargo's own
    /// directory names inside a marked target are not refused, and the same
    /// tree without the markers still is.
    #[test]
    fn a_marked_cargo_target_walks_clean_and_an_unmarked_one_is_refused() {
        let temp = tempfile::tempdir().unwrap();
        let root = crate::engine::process::canonicalize_plain(temp.path()).unwrap();
        let marked = root.join("marked");
        cargo_layout(&marked, true);
        let seen = walk(&marked).expect("Cargo's own tmp and crate-named directories");
        assert!(seen
            .entries
            .contains_key(Path::new("debug/build/log/0123abcd/out")));
        let unmarked = root.join("unmarked");
        cargo_layout(&unmarked, false);
        let refused = walk(&unmarked).expect_err("no markers, no waiver");
        assert!(
            refused.contains("Protected content inside candidate"),
            "{refused}"
        );
    }

    /// The waiver stops at what is protected for its own sake: a repository
    /// inside `tmp`, a `.env`, and `node_modules` stay refused even inside a
    /// marked target.
    #[test]
    fn protection_resumes_below_cargos_tmp_and_for_dependencies() {
        let temp = tempfile::tempdir().unwrap();
        let root = crate::engine::process::canonicalize_plain(temp.path()).unwrap();
        for (index, hidden) in ["tmp/repo/.git/config", "tmp/.env", "debug/node_modules/x"]
            .into_iter()
            .enumerate()
        {
            let target = root.join(format!("target-{index}"));
            cargo_layout(&target, true);
            let file = target.join(hidden);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(&file, "x").unwrap();
            let refused = walk(&target).expect_err(hidden);
            assert!(
                refused.contains("Protected content inside candidate"),
                "{hidden}: {refused}"
            );
        }
    }

    /// What the cleaner's agent scopes need: a real-shaped target, `tmp`
    /// included, is walked and removed, not refused.
    #[test]
    fn a_marked_cargo_target_with_cargos_own_tmp_is_reviewed_and_removed() {
        let temp = tempfile::tempdir().unwrap();
        let root = crate::engine::process::canonicalize_plain(temp.path()).unwrap();
        let target = root.join("x-target");
        cargo_layout(&target, true);
        let reviewed = walk(&target).unwrap();
        remove_reviewed(&target, &reviewed, &AtomicBool::new(false)).unwrap();
        assert!(!target.exists());
    }

    fn stamp(dev: u64) -> super::Stamp {
        super::Stamp {
            directory: true,
            len: 0,
            modified: 0,
            identity: (dev, 1),
            changed: (0, 0),
        }
    }

    /// A candidate holding an entry on another device (a mount point) is
    /// reported, naming the entry; one wholly on a device is not. The snapshot
    /// is the stat seam: it carries each entry's device as the walk read it.
    #[test]
    fn an_entry_on_another_device_is_reported() {
        let mut snapshot = super::Snapshot {
            entries: std::collections::BTreeMap::new(),
            bytes: 0,
            newest: 0,
        };
        snapshot.entries.insert("".into(), stamp(7));
        snapshot.entries.insert("debug".into(), stamp(7));
        assert_eq!(super::crossing_device(&snapshot), None);
        snapshot.entries.insert("debug/mnt".into(), stamp(9));
        assert_eq!(
            super::crossing_device(&snapshot),
            Some(Path::new("debug/mnt"))
        );
        // A real tree is on one device.
        let temp = tempfile::tempdir().unwrap();
        let root = crate::engine::process::canonicalize_plain(temp.path()).unwrap();
        cargo_layout(&root.join("t"), true);
        let real = walk(&root.join("t")).unwrap();
        assert_eq!(super::crossing_device(&real), None);
    }

    /// A budget smaller than the tree stops the walk with the error a caller
    /// recognises as "too large", not as a refusal.
    #[test]
    fn a_walk_past_its_budget_returns_the_budget_error() {
        let temp = tempfile::tempdir().unwrap();
        let root = crate::engine::process::canonicalize_plain(temp.path()).unwrap();
        cargo_layout(&root.join("t"), true);
        let error = super::snapshot_within(
            &root.join("t"),
            true,
            &AtomicBool::new(false),
            super::Budget {
                entries: 3,
                seconds: 30,
            },
        )
        .expect_err("more than three entries");
        assert_eq!(error, super::WalkError::Budget);
        assert_eq!(error.message(), super::BUDGET_EXCEEDED);
    }

    /// Running out of time is not the tree being too large: it is a walk that
    /// did not finish, and says so differently.
    #[test]
    fn a_walk_that_runs_out_of_time_is_not_a_too_large_tree() {
        let temp = tempfile::tempdir().unwrap();
        let root = crate::engine::process::canonicalize_plain(temp.path()).unwrap();
        cargo_layout(&root.join("t"), true);
        let error = super::snapshot_within(
            &root.join("t"),
            true,
            &AtomicBool::new(false),
            super::Budget {
                entries: 1_000_000,
                seconds: 0,
            },
        )
        .expect_err("no time at all");
        assert_eq!(error, super::WalkError::TimedOut);
        assert_eq!(error.message(), super::TIME_EXCEEDED);
    }

    /// A tree holding `names` as hard links of one `len`-byte file.
    #[cfg(unix)]
    fn linked_tree(root: &Path, names: &[&str], len: usize) {
        fs::create_dir_all(root).unwrap();
        let first = root.join(names[0]);
        fs::create_dir_all(first.parent().unwrap()).unwrap();
        fs::write(&first, vec![b'x'; len]).unwrap();
        for name in &names[1..] {
            let to = root.join(name);
            fs::create_dir_all(to.parent().unwrap()).unwrap();
            fs::hard_link(&first, &to).unwrap();
        }
    }

    /// Cargo hard-links heavily: removing one name changes the ctime of the
    /// others, which the per-file check read as "content changed after
    /// preview". A target holding two names of one file stopped half way.
    #[cfg(unix)]
    #[test]
    fn two_hard_links_inside_a_tree_remove_completely_and_count_once() {
        let temp = tempfile::tempdir().unwrap();
        let root = crate::engine::process::canonicalize_plain(temp.path())
            .unwrap()
            .join("t");
        linked_tree(&root, &["debug/foo", "debug/deps/foo-1234"], 4096);
        let reviewed = super::snapshot_within(
            &root,
            false,
            &AtomicBool::new(false),
            super::Budget::STANDARD,
        )
        .unwrap();
        assert_eq!(reviewed.bytes, 4096, "one file, two names: counted once");
        let freed = super::remove_reviewed_counted(&root, &reviewed, &AtomicBool::new(false))
            .expect("every name of the file is inside the tree");
        assert_eq!(freed, 4096);
        assert!(!root.exists());
    }

    #[cfg(unix)]
    #[test]
    fn three_hard_links_and_two_separate_pairs_remove_completely() {
        let temp = tempfile::tempdir().unwrap();
        let root = crate::engine::process::canonicalize_plain(temp.path())
            .unwrap()
            .join("t");
        linked_tree(&root, &["a/x", "b/x", "c/deeper/x"], 1000);
        linked_tree(&root.join("pair"), &["p", "q"], 500);
        let reviewed = super::snapshot_within(
            &root,
            false,
            &AtomicBool::new(false),
            super::Budget::STANDARD,
        )
        .unwrap();
        assert_eq!(reviewed.bytes, 1500);
        let freed =
            super::remove_reviewed_counted(&root, &reviewed, &AtomicBool::new(false)).unwrap();
        assert_eq!(freed, 1500);
        assert!(!root.exists());
    }

    /// A name outside the tree keeps the file: removing the tree unlinks the
    /// names inside, changes the outside name's ctime and nothing else, frees
    /// nothing, and says so.
    #[cfg(unix)]
    #[test]
    fn a_link_to_a_file_outside_the_tree_is_removed_without_touching_it_or_counting_it() {
        let temp = tempfile::tempdir().unwrap();
        let base = crate::engine::process::canonicalize_plain(temp.path()).unwrap();
        let outside = base.join("outside.bin");
        fs::write(&outside, vec![b'o'; 2048]).unwrap();
        let root = base.join("t");
        fs::create_dir_all(root.join("debug")).unwrap();
        fs::hard_link(&outside, root.join("debug/linked")).unwrap();
        fs::write(root.join("debug/own"), vec![b'x'; 100]).unwrap();
        let reviewed = super::snapshot_within(
            &root,
            false,
            &AtomicBool::new(false),
            super::Budget::STANDARD,
        )
        .unwrap();
        assert_eq!(reviewed.bytes, 100, "the linked file stays: not freed");
        let freed =
            super::remove_reviewed_counted(&root, &reviewed, &AtomicBool::new(false)).unwrap();
        assert_eq!(freed, 100);
        assert!(!root.exists());
        assert_eq!(fs::read(&outside).unwrap(), vec![b'o'; 2048]);
    }

    /// A file changed after the preview still stops the removal, through either
    /// of its names, before anything is unlinked.
    #[cfg(unix)]
    #[test]
    fn a_hard_linked_file_changed_after_the_preview_stops_the_removal() {
        let temp = tempfile::tempdir().unwrap();
        let root = crate::engine::process::canonicalize_plain(temp.path())
            .unwrap()
            .join("t");
        linked_tree(&root, &["debug/foo", "debug/deps/foo-1234"], 4096);
        let reviewed = super::snapshot_within(
            &root,
            false,
            &AtomicBool::new(false),
            super::Budget::STANDARD,
        )
        .unwrap();
        fs::write(root.join("debug/deps/foo-1234"), vec![b'y'; 4096]).unwrap();
        let partial = super::remove_reviewed_counted(&root, &reviewed, &AtomicBool::new(false))
            .expect_err("the content changed");
        assert!(
            partial.error.contains("changed after preview"),
            "{partial:?}"
        );
        assert!(root.join("debug/foo").exists());
        assert!(root.join("debug/deps/foo-1234").exists());
    }

    /// What a name whose file this removal already unlinked another name of may
    /// differ in: the ctime, nothing else. Any other entry is checked in full.
    #[cfg(unix)]
    #[test]
    fn the_unchanged_check_excuses_only_the_ctime_of_an_already_unlinked_file() {
        let approved = super::Stamp {
            directory: false,
            len: 10,
            modified: 5_000_000_000,
            identity: (1, 2),
            changed: (7, 0),
        };
        let now = |size, changed, modified, unlinked| {
            super::unix::file_unchanged(&approved, size, changed, modified, unlinked)
        };
        assert!(now(10, (7, 0), 5_000_000_000, false));
        assert!(
            !now(10, (8, 0), 5_000_000_000, false),
            "a file none of whose names was touched is checked in full"
        );
        assert!(
            now(10, (8, 0), 5_000_000_000, true),
            "our own unlink moved it"
        );
        assert!(!now(11, (8, 0), 5_000_000_000, true), "size");
        assert!(!now(10, (8, 0), 5_000_000_001, true), "content written");
    }

    #[test]
    fn no_symlinks_walks_a_canonical_temp_path() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().canonicalize().unwrap().join("owned");
        fs::create_dir_all(&path).unwrap();
        no_symlinks(&path)
            .expect("canonical paths start with a volume prefix; that prefix is not statted");
        let plain = crate::engine::process::canonicalize_plain(temp.path())
            .unwrap()
            .join("owned");
        no_symlinks(&plain)
            .expect("git-facing paths use canonicalize_plain; volume roots are still not statted");
    }
}
