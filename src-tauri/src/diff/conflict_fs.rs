//! Worktree entries are accessed relative to a pinned, symlink-free directory.
//! A concurrent ancestor rename cannot redirect resolution I/O through a link.
use super::conflict_session::Worktree;
use std::path::Path;
#[cfg(not(windows))]
use std::path::PathBuf;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub(super) use windows::{read, replace, Recovery};

#[cfg(unix)]
mod unix {
    use super::*;
    use std::ffi::CString;
    use std::fs::File;
    use std::io::{Read, Write};
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    };
    static NEXT: AtomicU64 = AtomicU64::new(1);
    const CAP: usize = 16 * 1024 * 1024;

    fn error() -> String {
        std::io::Error::last_os_error().to_string()
    }
    fn name(path: &Path) -> Result<CString, String> {
        CString::new(
            path.file_name()
                .ok_or("Missing conflict filename")?
                .as_bytes(),
        )
        .map_err(|e| e.to_string())
    }
    fn opened(fd: libc::c_int) -> Result<File, String> {
        if fd < 0 {
            return Err(error());
        }
        // SAFETY: the syscall returned a newly owned descriptor.
        Ok(unsafe { File::from_raw_fd(fd) })
    }
    fn parent(path: &Path) -> Result<Arc<File>, String> {
        crate::fs_entry::pin_parent(path, false)
            .map(Arc::new)
            .map_err(|e| e.to_string())
    }
    fn read_at(directory: &File, name: &CString) -> Result<Worktree, String> {
        read_at_with_limit(directory, name, CAP)
    }
    fn read_at_with_limit(
        directory: &File,
        name: &CString,
        cap: usize,
    ) -> Result<Worktree, String> {
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: initialized descriptor and name; fstatat initializes stat on success.
        if unsafe {
            libc::fstatat(
                directory.as_raw_fd(),
                name.as_ptr(),
                stat.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } != 0
        {
            let error = std::io::Error::last_os_error();
            return if error.kind() == std::io::ErrorKind::NotFound {
                Ok(Worktree {
                    bytes: None,
                    mode: "missing".into(),
                })
            } else {
                Err(error.to_string())
            };
        }
        // SAFETY: the successful call above initialized this value.
        let stat = unsafe { stat.assume_init() };
        if stat.st_mode & libc::S_IFMT == libc::S_IFLNK {
            let mut bytes = vec![0u8; 64 * 1024];
            // SAFETY: writable buffer with the passed capacity and a live parent fd.
            let count = unsafe {
                libc::readlinkat(
                    directory.as_raw_fd(),
                    name.as_ptr(),
                    bytes.as_mut_ptr().cast(),
                    bytes.len(),
                )
            };
            if count < 0 {
                return Err(error());
            }
            let count = usize::try_from(count).map_err(|e| e.to_string())?;
            if count >= bytes.len() {
                return Err("Symlink target exceeds the supported size".into());
            }
            bytes.truncate(count);
            return Ok(Worktree {
                bytes: Some(bytes),
                mode: "120000".into(),
            });
        }
        if stat.st_mode & libc::S_IFMT == libc::S_IFDIR {
            return Ok(Worktree {
                bytes: None,
                mode: "160000".into(),
            });
        }
        // SAFETY: bounded read of a non-followed entry relative to the pinned parent.
        let file = opened(unsafe {
            libc::openat(
                directory.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
            )
        })?;
        let meta = file.metadata().map_err(|e| e.to_string())?;
        if !meta.is_file() || meta.len() > cap as u64 {
            return Err(format!(
                "File entry is not a regular file within the {} MiB limit",
                cap / (1024 * 1024)
            ));
        }
        let mut bytes = Vec::new();
        file.take(cap as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() > cap {
            return Err(format!(
                "File grew beyond the {} MiB limit",
                cap / (1024 * 1024)
            ));
        }
        Ok(Worktree {
            bytes: Some(bytes),
            mode: if meta.mode() & 0o111 != 0 {
                "100755"
            } else {
                "100644"
            }
            .into(),
        })
    }
    pub(super) fn read(path: &Path) -> Result<Worktree, String> {
        let directory = parent(path)?;
        read_at(&directory, &name(path)?)
    }

    pub(super) fn read_for_save(path: &Path, cap: usize) -> Result<Worktree, String> {
        let directory = crate::fs_entry::pin_parent(path, true).map_err(|e| e.to_string())?;
        let target = name(path)?;
        // Check the same write authority as an ordinary save, without ever
        // truncating the inode (which may also have names outside the repo).
        // SAFETY: the pinned directory and single-component name stay live;
        // no-follow/nonblocking flags refuse links and prevent FIFO waits.
        let writable = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                target.as_ptr(),
                libc::O_WRONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
            )
        };
        if writable < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::NotFound {
                return Err(error.to_string());
            }
        } else if !opened(writable)?
            .metadata()
            .map_err(|e| e.to_string())?
            .is_file()
        {
            return Err("Only regular files can be saved".into());
        }
        read_at_with_limit(&directory, &target, cap)
    }

    pub struct Recovery {
        pub path: PathBuf,
        pub keep: bool,
        directory: Arc<File>,
        name: CString,
    }
    impl Drop for Recovery {
        fn drop(&mut self) {
            if !self.keep {
                // SAFETY: removes only our temporary name in the directory in
                // which it was created, even if its ancestor was renamed.
                let removed =
                    unsafe { libc::unlinkat(self.directory.as_raw_fd(), self.name.as_ptr(), 0) };
                if removed != 0
                    && std::io::Error::last_os_error().kind() != std::io::ErrorKind::NotFound
                {
                    log::warn!("Conflict recovery cleanup failed: {}", error());
                }
            }
        }
    }
    fn temporary(directory: Arc<File>, path: &Path) -> Result<(Recovery, File), String> {
        for _ in 0..32 {
            let filename = format!(
                ".gitpulse-conflict-{}-{}-recovery",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            );
            let name = CString::new(filename.as_bytes()).map_err(|e| e.to_string())?;
            // SAFETY: create_new semantics and private permissions; existing
            // entries, including links placed by another process, are refused.
            let fd = unsafe {
                libc::openat(
                    directory.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_RDWR
                        | libc::O_CREAT
                        | libc::O_EXCL
                        | libc::O_NOFOLLOW
                        | libc::O_CLOEXEC,
                    0o600,
                )
            };
            if fd >= 0 {
                return Ok((
                    Recovery {
                        path: path.with_file_name(filename),
                        name,
                        directory,
                        keep: false,
                    },
                    opened(fd)?,
                ));
            }
            if std::io::Error::last_os_error().kind() != std::io::ErrorKind::AlreadyExists {
                return Err(error());
            }
        }
        Err("Cannot allocate a conflict recovery entry".into())
    }
    fn rename(directory: &File, from: &CString, to: &CString, swap: bool) -> Result<(), String> {
        if !swap {
            return crate::fs_entry::rename_noreplace_at(directory, from, directory, to)
                .map_err(|error| format!("Atomic conflict replacement failed: {error}"));
        }
        // SAFETY: both names and the shared parent descriptor are live. The
        // kernel either exchanges entries or refuses to replace the target.
        #[cfg(target_os = "macos")]
        let result = unsafe {
            libc::renameatx_np(
                directory.as_raw_fd(),
                from.as_ptr(),
                directory.as_raw_fd(),
                to.as_ptr(),
                libc::RENAME_SWAP,
            )
        };
        #[cfg(target_os = "linux")]
        let result = unsafe {
            libc::renameat2(
                directory.as_raw_fd(),
                from.as_ptr(),
                directory.as_raw_fd(),
                to.as_ptr(),
                libc::RENAME_EXCHANGE,
            )
        };
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            let _ = (directory, from, to, swap);
            return Err("Atomic conflict replacement is unavailable; resolve externally and stage the working file".into());
        }
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        if result != 0 {
            Err(format!("Atomic conflict replacement failed: {}", error()))
        } else {
            Ok(())
        }
    }
    pub(super) fn replace(
        path: &Path,
        source: &Worktree,
        next: &Worktree,
    ) -> Result<Option<Recovery>, String> {
        replace_with_limit(path, source, next, CAP)
    }
    pub(super) fn replace_with_limit(
        path: &Path,
        source: &Worktree,
        next: &Worktree,
        cap: usize,
    ) -> Result<Option<Recovery>, String> {
        if source == next {
            return Ok(None);
        }
        let directory = parent(path)?;
        let target = name(path)?;
        let (mut recovery, mut handle) = temporary(directory.clone(), path)?;
        if let Some(bytes) = &next.bytes {
            if next.mode == "120000" {
                let link =
                    CString::new(bytes.as_slice()).map_err(|_| "Git symlink contains NUL")?;
                // SAFETY: unlink our exclusive placeholder and create a link
                // without following or overwriting another entry.
                if unsafe { libc::unlinkat(directory.as_raw_fd(), recovery.name.as_ptr(), 0) } != 0
                {
                    return Err(error());
                }
                if unsafe {
                    libc::symlinkat(link.as_ptr(), directory.as_raw_fd(), recovery.name.as_ptr())
                } != 0
                {
                    recovery.keep = true;
                    return Err(error());
                }
            } else {
                handle.write_all(bytes).map_err(|e| e.to_string())?;
                // Preserve the permission bits when the selected Git mode did
                // not change, otherwise apply the chosen executable mode.
                let mut metadata = std::mem::MaybeUninit::<libc::stat>::uninit();
                // SAFETY: metadata is initialized only when the call succeeds;
                // this never follows a symlink to borrow its target's mode.
                let exists = unsafe {
                    libc::fstatat(
                        directory.as_raw_fd(),
                        target.as_ptr(),
                        metadata.as_mut_ptr(),
                        libc::AT_SYMLINK_NOFOLLOW,
                    )
                } == 0;
                let original = if exists {
                    Some(unsafe { metadata.assume_init() })
                } else {
                    None
                };
                // mode_t is u16 on macOS and u32 on Linux. Keep the lossless
                // conversion required by PermissionsExt without platform forks.
                #[allow(clippy::useless_conversion)]
                let mut mode = original
                    .as_ref()
                    .filter(|stat| stat.st_mode & libc::S_IFMT == libc::S_IFREG)
                    .map_or(0o600, |stat| u32::from(stat.st_mode) & 0o777);
                #[cfg(any(target_os = "macos", target_os = "linux"))]
                if original
                    .as_ref()
                    .is_some_and(|stat| stat.st_mode & libc::S_IFMT == libc::S_IFREG)
                {
                    // SAFETY: an O_NOFOLLOW source descriptor and our exclusive
                    // destination. Metadata copying preserves ACLs, ownership
                    // and extended attributes without copying source bytes.
                    let source_file = opened(unsafe {
                        libc::openat(
                            directory.as_raw_fd(),
                            target.as_ptr(),
                            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
                        )
                    })?;
                    #[cfg(target_os = "macos")]
                    if unsafe {
                        libc::fcopyfile(
                            source_file.as_raw_fd(),
                            handle.as_raw_fd(),
                            std::ptr::null_mut(),
                            libc::COPYFILE_METADATA,
                        )
                    } != 0
                    {
                        return Err(format!("Cannot preserve file metadata: {}", error()));
                    }
                    #[cfg(target_os = "linux")]
                    crate::fs_entry::preserve_metadata(&source_file, &handle)
                        .map_err(|e| format!("Cannot preserve file metadata: {e}"))?;
                }
                if source.mode != next.mode {
                    mode = if next.mode == "100755" {
                        mode | ((mode & 0o444) >> 2) | 0o100
                    } else {
                        mode & !0o111
                    };
                }
                // Unconditional, and `mode` is masked to 0o777, so this is
                // also what clears set-user-ID, set-group-ID and the sticky
                // bit. Those are dropped by an ordinary content write and by
                // the Linux arm above, which strips them along with
                // `security.capability`; `fcopyfile` copies the whole mode, so
                // without this a saved resolution would leave a set-id file
                // set-id on macOS alone. For every other bit this repeats what
                // the metadata copy already applied.
                handle
                    .set_permissions(std::fs::Permissions::from_mode(mode))
                    .map_err(|e| e.to_string())?;
                handle.sync_all().map_err(|e| e.to_string())?;
            }
        }
        let still_here = parent(path)?.metadata().map_err(|e| e.to_string())?;
        let anchored = directory.metadata().map_err(|e| e.to_string())?;
        if (still_here.dev(), still_here.ino()) != (anchored.dev(), anchored.ino())
            || read_at_with_limit(&directory, &target, cap)? != *source
        {
            return Err("The working file or its directory changed; reload before saving".into());
        }
        if source.mode == "missing" {
            // SAFETY: linkat without AT_SYMLINK_FOLLOW links the entry itself;
            // it fails if another writer created the destination meanwhile.
            if unsafe {
                libc::linkat(
                    directory.as_raw_fd(),
                    recovery.name.as_ptr(),
                    directory.as_raw_fd(),
                    target.as_ptr(),
                    0,
                )
            } != 0
            {
                return Err(format!(
                    "Cannot create the resolution without replacing a new entry: {}",
                    error()
                ));
            }
            directory.sync_all().map_err(|e| e.to_string())?;
            return Ok(None);
        }
        if next.bytes.is_none() {
            // SAFETY: remove our own placeholder to reserve a nonexisting
            // target for rename-without-replacement. A raced entry is refused.
            if unsafe { libc::unlinkat(directory.as_raw_fd(), recovery.name.as_ptr(), 0) } != 0 {
                return Err(error());
            }
            rename(&directory, &target, &recovery.name, false)?;
        } else {
            rename(&directory, &recovery.name, &target, true)?;
        }
        recovery.keep = true;
        if read_at_with_limit(&directory, &recovery.name, cap)? != *source {
            return Err(format!("An external edit raced the save. Review the working file and the displaced content retained at {} before staging", recovery.path.display()));
        }
        directory.sync_all().map_err(|e| {
            format!(
                "Resolution replaced but directory sync failed; original retained at {}: {e}",
                recovery.path.display()
            )
        })?;
        Ok(Some(recovery))
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn a_pinned_parent_cannot_be_redirected_through_a_replaced_ancestor() {
            let root = tempfile::TempDir::new().unwrap();
            let outside = tempfile::TempDir::new().unwrap();
            let canonical_root = root.path().canonicalize().unwrap();
            let folder = canonical_root.join("nested");
            std::fs::create_dir(&folder).unwrap();
            std::fs::write(folder.join("file"), "original").unwrap();
            std::fs::write(outside.path().join("file"), "outside").unwrap();
            let dir = parent(&folder.join("file")).unwrap();
            std::fs::rename(&folder, root.path().join("moved")).unwrap();
            std::os::unix::fs::symlink(outside.path(), &folder).unwrap();
            assert_eq!(
                read_at(&dir, &CString::new("file").unwrap())
                    .unwrap()
                    .bytes
                    .unwrap(),
                b"original"
            );
            assert!(parent(&folder.join("file")).is_err());
            assert_eq!(
                std::fs::read(outside.path().join("file")).unwrap(),
                b"outside"
            );
        }
    }
}

#[cfg(unix)]
pub(super) use unix::Recovery;
#[cfg(unix)]
pub(super) fn read(path: &Path) -> Result<Worktree, String> {
    unix::read(path)
}
#[cfg(unix)]
pub(super) fn replace(
    path: &Path,
    source: &Worktree,
    next: &Worktree,
) -> Result<Option<Recovery>, String> {
    unix::replace(path, source, next)
}

#[cfg(not(any(unix, windows)))]
pub(super) struct Recovery {
    pub path: PathBuf,
    pub keep: bool,
}
#[cfg(not(any(unix, windows)))]
pub(super) fn replace(
    _path: &Path,
    source: &Worktree,
    next: &Worktree,
) -> Result<Option<Recovery>, String> {
    if source == next {
        Ok(None)
    } else {
        Err("Atomic conflict replacement is unavailable on this platform; resolve externally and use Stage working file".into())
    }
}

/// Ordinary saves share the anchored entry transaction used by conflict saves.
/// They retain the editor's file-size budget and retain displaced content when
/// an external edit races publication.
pub(crate) fn write_regular(path: &Path, content: &[u8]) -> Result<(), String> {
    let cap = crate::engine::budget::MAX_FILE_BYTES as usize;
    if content.len() > cap {
        return Err(format!(
            "File exceeds the {} MiB save limit",
            cap / (1024 * 1024)
        ));
    }
    #[cfg(unix)]
    let source = unix::read_for_save(path, cap)?;
    #[cfg(windows)]
    let source = windows::read_for_save(path, cap)?;
    #[cfg(any(unix, windows))]
    {
        if !matches!(source.mode.as_str(), "missing" | "100644" | "100755") {
            return Err("Only regular files can be saved".into());
        }
        let next = Worktree {
            bytes: Some(content.to_vec()),
            mode: if source.mode == "100755" {
                "100755"
            } else {
                "100644"
            }
            .into(),
        };
        #[cfg(unix)]
        let saved = unix::replace_with_limit(path, &source, &next, cap)?;
        #[cfg(windows)]
        let saved = windows::replace_with_limit(path, &source, &next, cap as u64)?;
        if let Some(mut original) = saved {
            original.keep = false;
        }
        Ok(())
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        Err("Atomic file saves are unavailable on this platform".into())
    }
}
