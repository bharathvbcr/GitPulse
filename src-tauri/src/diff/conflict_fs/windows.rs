//! Windows replacement retains the displaced file until index publication.
//! Ancestor handles deny deletion/rename, and every opened entry rejects reparse
//! points. ReplaceFileW preserves metadata without ignoring ACL merge failures.
use super::Worktree;
use std::ffi::c_void;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::os::windows::io::AsRawHandle;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const CAP: u64 = 16 * 1024 * 1024;
const SHARE_READ: u32 = 1;
const SHARE_WRITE: u32 = 2;
const SHARE_DELETE: u32 = 4;
const OPEN_REPARSE_POINT: u32 = 0x0020_0000;
const BACKUP_SEMANTICS: u32 = 0x0200_0000;
const REPARSE_POINT: u32 = 0x400;
static NEXT: AtomicU64 = AtomicU64::new(1);

#[repr(C)]
#[derive(Default)]
struct FileInformation {
    attributes: u32,
    creation_time: [u32; 2],
    access_time: [u32; 2],
    write_time: [u32; 2],
    volume: u32,
    size_high: u32,
    size_low: u32,
    links: u32,
    index_high: u32,
    index_low: u32,
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetFileInformationByHandle(file: *mut c_void, info: *mut FileInformation) -> i32;
    fn ReplaceFileW(
        target: *const u16,
        replacement: *const u16,
        backup: *const u16,
        flags: u32,
        exclude: *mut c_void,
        reserved: *mut c_void,
    ) -> i32;
}

type Identity = (u32, u32, u32);

fn identity(file: &File) -> Result<Identity, String> {
    let mut info = FileInformation::default();
    // SAFETY: File owns the live handle; info has the documented C layout.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    Ok((info.volume, info.index_high, info.index_low))
}

fn wide(path: &Path) -> Result<Vec<u16>, String> {
    crate::fs_entry::windows_path(path).map_err(|error| error.to_string())
}

fn pin_directory(path: &Path) -> Result<File, String> {
    let file = OpenOptions::new()
        // FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES. Metadata-only opens do
        // not participate in Windows share-access checks, so they cannot pin
        // a directory name. No backup privilege or ACL override is enabled.
        .access_mode(0x81)
        .share_mode(SHARE_READ | SHARE_WRITE) // No DELETE: the name stays pinned.
        .custom_flags(OPEN_REPARSE_POINT | BACKUP_SEMANTICS)
        .open(path)
        .map_err(|e| format!("Cannot pin conflict parent: {e}"))?;
    let meta = file.metadata().map_err(|e| e.to_string())?;
    if !meta.is_dir() || meta.file_attributes() & REPARSE_POINT != 0 {
        return Err("Conflict parent must be an existing real directory".into());
    }
    Ok(file)
}

fn parents(path: &Path) -> Result<Vec<File>, String> {
    if !path.is_absolute() || path.file_name().is_none() {
        return Err("Conflict path must be an absolute file path".into());
    }
    let parent = path.parent().ok_or("Missing conflict parent")?;
    let mut current = PathBuf::new();
    let mut handles = Vec::new();
    for component in parent.components() {
        if handles.len() >= 256 {
            return Err("Conflict path exceeds the directory depth limit".into());
        }
        match component {
            Component::Prefix(_) => current.push(component),
            Component::RootDir | Component::Normal(_) => {
                current.push(component);
                handles.push(pin_directory(&current)?);
            }
            _ => return Err("Conflict path must have normalized parents".into()),
        }
    }
    Ok(handles)
}

// Holding the returned file denies writers for the lifetime of this snapshot.
// DELETE is shared so ReplaceFileW can retain it under the recovery name.
fn read_entry(path: &Path) -> Result<(Option<File>, Worktree), String> {
    let file = match OpenOptions::new()
        .read(true)
        .share_mode(SHARE_READ | SHARE_DELETE)
        .custom_flags(OPEN_REPARSE_POINT | BACKUP_SEMANTICS)
        .open(path)
    {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok((
                None,
                Worktree {
                    bytes: None,
                    mode: "missing".into(),
                },
            ));
        }
        Err(e) => return Err(format!("Cannot read conflict entry: {e}")),
    };
    let meta = file.metadata().map_err(|e| e.to_string())?;
    if meta.file_attributes() & REPARSE_POINT != 0 {
        return Err("Windows reparse points must be resolved and staged externally".into());
    }
    if meta.is_dir() {
        return Ok((
            Some(file),
            Worktree {
                bytes: None,
                mode: "160000".into(),
            },
        ));
    }
    if !meta.is_file() || meta.len() > CAP {
        return Err("Conflict entry is not a regular file within the 16 MiB limit".into());
    }
    let mut bytes = Vec::new();
    (&file)
        .take(CAP + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > CAP {
        return Err("Conflict file grew beyond the 16 MiB limit".into());
    }
    Ok((
        Some(file),
        Worktree {
            bytes: Some(bytes),
            mode: "100644".into(),
        },
    ))
}

pub(in crate::diff) fn read(path: &Path) -> Result<Worktree, String> {
    let _parents = parents(path)?;
    Ok(read_entry(path)?.1)
}

pub(in crate::diff) struct Recovery {
    pub path: PathBuf,
    pub keep: bool,
    directory: PathBuf,
    directory_handle: Option<File>,
    _parents: Vec<File>,
    prepared: PathBuf,
    prepared_id: Option<Identity>,
    original_id: Option<Identity>,
}

impl Recovery {
    fn create(path: &Path, parents: Vec<File>) -> Result<Self, String> {
        for _ in 0..32 {
            let directory = path
                .parent()
                .ok_or("Missing conflict parent")?
                .join(format!(
                    ".gitpulse-conflict-{}-{}-recovery",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                ));
            match fs::create_dir(&directory) {
                Ok(()) => {
                    let directory_handle = pin_directory(&directory)?;
                    return Ok(Self {
                        path: directory.join("original"),
                        prepared: directory.join("prepared"),
                        directory,
                        directory_handle: Some(directory_handle),
                        _parents: parents,
                        keep: false,
                        prepared_id: None,
                        original_id: None,
                    });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(format!("Cannot reserve recovery directory: {e}")),
            }
        }
        Err("Cannot reserve a unique conflict recovery directory".into())
    }
}

fn remove_owned(path: &Path, expected: Option<Identity>) -> Result<(), String> {
    let (file, _) = read_entry(path)?;
    if let Some(file) = file {
        if Some(identity(&file)?) != expected {
            return Err("Recovery entry changed ownership; it has been retained".into());
        }
        fs::remove_file(path).map_err(|e| e.to_string())?;
    }
    Ok(())
}

impl Drop for Recovery {
    fn drop(&mut self) {
        if self.keep {
            return;
        }
        let cleaned = remove_owned(&self.prepared, self.prepared_id)
            .and_then(|()| remove_owned(&self.path, self.original_id));
        if let Err(error) = cleaned {
            log::warn!(
                "Conflict recovery retained at {}: {error}",
                self.directory.display()
            );
            return;
        }
        drop(self.directory_handle.take());
        if let Err(error) = fs::remove_dir(&self.directory) {
            log::warn!(
                "Cannot remove conflict recovery directory {}: {error}",
                self.directory.display()
            );
        }
    }
}

fn move_new(source: &Path, target: &Path) -> Result<(), String> {
    crate::fs_entry::rename_noreplace(source, target).map_err(|error| error.to_string())
}

pub(in crate::diff) fn replace(
    path: &Path,
    source: &Worktree,
    next: &Worktree,
) -> Result<Option<Recovery>, String> {
    if source == next {
        return Ok(None);
    }
    for entry in [source, next] {
        if !matches!(entry.mode.as_str(), "missing" | "100644")
            || (entry.mode == "missing") != entry.bytes.is_none()
            || entry
                .bytes
                .as_ref()
                .is_some_and(|bytes| bytes.len() as u64 > CAP)
        {
            return Err(
                "Windows conflict replacement requires a bounded regular file or deletion".into(),
            );
        }
    }
    let parents = parents(path)?;
    let (source_handle, current) = read_entry(path)?;
    if &current != source {
        return Err("Conflict source changed; reload before saving".into());
    }
    let source_id = source_handle.as_ref().map(identity).transpose()?;
    let mut recovery = Recovery::create(path, parents)?;
    if let Some(bytes) = &next.bytes {
        let mut prepared = OpenOptions::new()
            .write(true)
            .create_new(true)
            .custom_flags(OPEN_REPARSE_POINT)
            .open(&recovery.prepared)
            .map_err(|e| e.to_string())?;
        recovery.prepared_id = Some(identity(&prepared)?);
        prepared.write_all(bytes).map_err(|e| e.to_string())?;
        prepared.sync_all().map_err(|e| e.to_string())?;
    }
    // Revalidate after preparation. A rename can displace the locked source;
    // it cannot change its bytes, and any later displaced entry is retained.
    let (fresh_handle, fresh) = read_entry(path)?;
    if &fresh != source || fresh_handle.as_ref().map(identity).transpose()? != source_id {
        return Err("Conflict source changed while preparing the resolution".into());
    }
    drop(fresh_handle);
    if source.bytes.is_none() {
        move_new(&recovery.prepared, path)?;
        return Ok(None);
    }
    recovery.original_id = source_id;
    // ReplaceFileW may move the original even on failure (error 1177). Retain
    // both paths on every attempted mutation until publication is confirmed.
    recovery.keep = true;
    let result = if next.bytes.is_none() {
        move_new(path, &recovery.path)
    } else {
        let target = wide(path)?;
        let prepared = wide(&recovery.prepared)?;
        let backup = wide(&recovery.path)?;
        // SAFETY: buffers remain live; reserved parameters are NULL, flags=0
        // requires all metadata/ACL preservation to succeed.
        if unsafe {
            ReplaceFileW(
                target.as_ptr(),
                prepared.as_ptr(),
                backup.as_ptr(),
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        } == 0
        {
            Err(std::io::Error::last_os_error().to_string())
        } else {
            Ok(())
        }
    };
    result.map_err(|e| format!("Conflict replacement failed: {e}. Recovery files retained at {}; reload before staging", recovery.directory.display()))?;
    let (displaced, original) = read_entry(&recovery.path)?;
    if &original != source || displaced.as_ref().map(identity).transpose()? != source_id {
        return Err(format!("Conflict source changed during replacement; displaced content retained at {}. Reload before staging", recovery.path.display()));
    }
    Ok(Some(recovery))
}

#[cfg(test)]
mod tests {
    use super::{identity, parents, read, read_entry, replace, Worktree, CAP};
    use std::fs::{self, OpenOptions};
    use std::os::windows::fs::OpenOptionsExt;

    fn content(bytes: &[u8]) -> Worktree {
        Worktree {
            bytes: Some(bytes.to_vec()),
            mode: "100644".into(),
        }
    }

    #[test]
    fn replacement_retains_exact_original_until_publication() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().canonicalize().unwrap().join("file");
        fs::write(&path, b"original\0\xff").unwrap();
        let mut recovery = replace(
            &path,
            &content(b"original\0\xff"),
            &content(b"resolved\0\xfe"),
        )
        .unwrap()
        .unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"resolved\0\xfe");
        assert_eq!(fs::read(&recovery.path).unwrap(), b"original\0\xff");
        let backup = recovery.path.clone();
        recovery.keep = false;
        drop(recovery);
        assert!(!backup.exists());
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn an_unpublished_resolution_keeps_recovery_and_pins_ancestors() {
        let dir = tempfile::TempDir::new().unwrap();
        let parent = dir.path().canonicalize().unwrap().join("nested");
        fs::create_dir(&parent).unwrap();
        let path = parent.join("file");
        fs::write(&path, b"original").unwrap();
        let recovery = replace(&path, &content(b"original"), &content(b"resolved"))
            .unwrap()
            .unwrap();
        assert!(fs::rename(&parent, dir.path().join("moved")).is_err());
        let backup = recovery.path.clone();
        drop(recovery);
        assert_eq!(fs::read(backup).unwrap(), b"original");
        fs::rename(&parent, dir.path().join("moved")).unwrap();
    }

    #[test]
    fn stale_locked_and_oversized_sources_cannot_be_overwritten() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().canonicalize().unwrap().join("file");
        fs::write(&path, b"external").unwrap();
        assert!(replace(&path, &content(b"original"), &content(b"next")).is_err());
        let locked = OpenOptions::new()
            .write(true)
            .share_mode(0)
            .open(&path)
            .unwrap();
        assert!(replace(&path, &content(b"external"), &content(b"next")).is_err());
        drop(locked);
        assert_eq!(fs::read(&path).unwrap(), b"external");
        let file = OpenOptions::new().write(true).open(&path).unwrap();
        file.set_len(CAP + 1).unwrap();
        drop(file);
        assert!(read(&path).is_err());
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn pinned_directories_and_source_locks_prevent_redirection_and_writes() {
        let dir = tempfile::TempDir::new().unwrap();
        let parent = dir.path().canonicalize().unwrap().join("nested");
        fs::create_dir(&parent).unwrap();
        let path = parent.join("file");
        fs::write(&path, b"original").unwrap();
        let pins = parents(&path).unwrap();
        assert!(fs::rename(&parent, dir.path().join("moved")).is_err());
        let (source, bytes) = read_entry(&path).unwrap();
        assert_eq!(bytes, content(b"original"));
        assert!(fs::write(&path, b"external").is_err());
        drop(source);
        drop(pins);
        fs::rename(&parent, dir.path().join("moved")).unwrap();
    }

    #[test]
    fn replaced_recovery_entries_are_retained_instead_of_deleted() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().canonicalize().unwrap().join("file");
        fs::write(&path, b"original").unwrap();
        let mut recovery = replace(&path, &content(b"original"), &content(b"resolved"))
            .unwrap()
            .unwrap();
        fs::rename(&recovery.path, recovery.directory.join("saved")).unwrap();
        fs::write(&recovery.path, b"other owner").unwrap();
        let backup = recovery.path.clone();
        assert_ne!(
            Some(identity(&fs::File::open(&backup).unwrap()).unwrap()),
            recovery.original_id
        );
        recovery.keep = false;
        drop(recovery);
        assert_eq!(fs::read(backup).unwrap(), b"other owner");
    }

    #[test]
    fn create_and_delete_preserve_missing_state_and_original_bytes() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().canonicalize().unwrap().join("file");
        let missing = Worktree {
            bytes: None,
            mode: "missing".into(),
        };
        assert!(replace(&path, &missing, &content(b"created"))
            .unwrap()
            .is_none());
        let recovery = replace(&path, &content(b"created"), &missing)
            .unwrap()
            .unwrap();
        assert!(!path.exists());
        assert_eq!(fs::read(&recovery.path).unwrap(), b"created");
    }

    #[test]
    fn replacement_api_failure_retains_prepared_content_and_the_source() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().canonicalize().unwrap().join("file");
        fs::write(&path, b"original").unwrap();
        // Reads remain possible but ReplaceFileW cannot acquire DELETE access.
        let lock = OpenOptions::new()
            .read(true)
            .share_mode(super::SHARE_READ)
            .open(&path)
            .unwrap();
        let error = replace(&path, &content(b"original"), &content(b"prepared"))
            .err()
            .unwrap();
        assert!(error.contains("Recovery files retained"), "{error}");
        assert_eq!(fs::read(&path).unwrap(), b"original");
        let recovery = fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|entry| entry.is_dir())
            .unwrap();
        assert_eq!(fs::read(recovery.join("prepared")).unwrap(), b"prepared");
        drop(lock);
    }

    #[test]
    fn create_refuses_a_concurrent_destination_and_hardlinks_are_not_written_through() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let path = root.join("file");
        let staged = root.join("prepared");
        fs::write(&path, b"external").unwrap();
        fs::write(&staged, b"prepared").unwrap();
        assert!(super::move_new(&staged, &path).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"external");
        fs::hard_link(&path, root.join("alias")).unwrap();
        let mut recovery = replace(&path, &content(b"external"), &content(b"resolved"))
            .unwrap()
            .unwrap();
        assert_eq!(fs::read(root.join("alias")).unwrap(), b"external");
        recovery.keep = false;
    }

    #[test]
    fn long_descendants_of_short_roots_support_replacement_and_missing_destinations() {
        let dir = tempfile::TempDir::new().unwrap();
        let mut parent = dir.path().canonicalize().unwrap();
        for _ in 0..18 {
            parent.push("nested-directory");
        }
        fs::create_dir_all(&parent).unwrap();
        let canonical = parent.join("file");
        fs::write(&canonical, b"original").unwrap();
        let plain =
            std::path::PathBuf::from(canonical.to_str().unwrap().strip_prefix(r"\\?\").unwrap());
        assert!(plain.as_os_str().len() > 260);
        let encoded = super::wide(&plain).unwrap();
        assert!(String::from_utf16(&encoded[..encoded.len() - 1])
            .unwrap()
            .starts_with(r"\\?\"));
        let mut recovery = replace(&plain, &content(b"original"), &content(b"resolved"))
            .unwrap()
            .unwrap();
        assert_eq!(fs::read(&canonical).unwrap(), b"resolved");
        recovery.keep = false;
        drop(recovery);
        let missing = Worktree {
            bytes: None,
            mode: "missing".into(),
        };
        let new_path = plain.with_file_name("new");
        assert!(replace(&new_path, &missing, &content(b"created"))
            .unwrap()
            .is_none());
        assert_eq!(fs::read(parent.join("new")).unwrap(), b"created");
    }
}
