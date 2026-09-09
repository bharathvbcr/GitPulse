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
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component);
        if fs::symlink_metadata(&current)
            .map_err(|e| format!("{}: {e}", current.display()))?
            .file_type()
            .is_symlink()
        {
            return Err(format!(
                "Symlink paths are not eligible: {}",
                current.display()
            ));
        }
    }
    Ok(())
}

pub fn snapshot(root: &Path, strict: bool, cancel: &AtomicBool) -> Result<Snapshot, String> {
    let mut result = Snapshot {
        entries: BTreeMap::new(),
        bytes: 0,
        newest: 0,
    };
    let deadline = Instant::now() + Duration::from_secs(15);
    walk(root, Path::new(""), strict, deadline, cancel, &mut result)?;
    Ok(result)
}

fn walk(
    root: &Path,
    relative: &Path,
    strict: bool,
    deadline: Instant,
    cancel: &AtomicBool,
    result: &mut Snapshot,
) -> Result<(), String> {
    if cancel.load(Ordering::Relaxed) {
        return Err("Cancelled".into());
    }
    if Instant::now() >= deadline
        || result.entries.len() >= MAX_ENTRIES
        || relative.components().count() > MAX_DEPTH
    {
        return Err(
            "Measurement incomplete: traversal budget exceeded. Cleanup is unavailable.".into(),
        );
    }
    let path = root.join(relative);
    let meta = fs::symlink_metadata(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    if meta.file_type().is_symlink() {
        if strict {
            return Err(format!("Symlink inside candidate: {}", relative.display()));
        }
        return Ok(());
    }
    if !meta.is_file() && !meta.is_dir() {
        return Err("Special files inside candidate; cleanup is unavailable".into());
    }
    if strict && !relative.as_os_str().is_empty() {
        if devmap_query::hygiene::protected_entry(relative) {
            return Err(format!(
                "Protected content inside candidate: {}",
                relative.display()
            ));
        }
        if root.file_name().is_some_and(|n| n == "__pycache__")
            && (!meta.is_file()
                || !matches!(
                    path.extension().and_then(|s| s.to_str()),
                    Some("pyc" | "pyo")
                ))
        {
            return Err("Python bytecode directory contains non-bytecode content".into());
        }
    }
    let stamp = Stamp::read(&meta)?;
    result.newest = result.newest.max(stamp.modified);
    if meta.is_file() {
        result.bytes = result.bytes.saturating_add(meta.len());
    }
    result.entries.insert(relative.to_path_buf(), stamp);
    if meta.is_dir() {
        for entry in fs::read_dir(&path).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            walk(
                root,
                &relative.join(entry.file_name()),
                strict,
                deadline,
                cancel,
                result,
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
    use super::{Snapshot, Stamp};
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

    pub fn remove(root: &Path, expected: &Snapshot, cancel: &AtomicBool) -> Result<(), String> {
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
        let deadline = Instant::now() + Duration::from_secs(60);
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
            if !approved.directory
                && (raw.st_size < 0
                    || u64::try_from(raw.st_size).ok() != Some(approved.len)
                    || changed != approved.changed)
            {
                return Err(
                    "File content changed after preview; stopped before removing it".into(),
                );
            }
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
        }
        Ok(())
    }
}

pub fn remove_reviewed(
    root: &Path,
    expected: &Snapshot,
    cancel: &AtomicBool,
) -> Result<(), String> {
    #[cfg(unix)]
    {
        unix::remove(root, expected, cancel)
    }
    #[cfg(not(unix))]
    {
        let _ = (root, expected, cancel);
        Err(
            "Local artifact cleanup is unavailable on this platform; use the native build tool"
                .into(),
        )
    }
}
