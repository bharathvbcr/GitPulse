//! Atomic publication of a filesystem entry without replacing an existing one.
//! Shared by clone publication and conflict recovery; unsupported filesystems
//! fail closed rather than falling back to a check followed by an overwrite.
use std::io;
use std::path::Path;

#[cfg(unix)]
pub(crate) fn rename_noreplace_at(
    source_parent: &std::fs::File,
    source: &std::ffi::CStr,
    target_parent: &std::fs::File,
    target: &std::ffi::CStr,
) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    #[cfg(target_os = "macos")]
    // SAFETY: descriptors and terminated names remain valid for the syscall.
    let result = unsafe {
        libc::renameatx_np(
            source_parent.as_raw_fd(),
            source.as_ptr(),
            target_parent.as_raw_fd(),
            target.as_ptr(),
            libc::RENAME_EXCL,
        )
    };
    #[cfg(target_os = "linux")]
    // SAFETY: descriptors and terminated names remain valid for the syscall.
    let result = unsafe {
        libc::renameat2(
            source_parent.as_raw_fd(),
            source.as_ptr(),
            target_parent.as_raw_fd(),
            target.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        if result == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (source_parent.as_raw_fd(), source, target_parent, target);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Atomic no-replace publication is unavailable on this platform",
        ))
    }
}

#[cfg(unix)]
pub(crate) fn rename_noreplace(source: &Path, target: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let parts = |path: &Path| -> io::Result<(std::fs::File, CString)> {
        let parent = path.parent().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "Missing parent directory")
        })?;
        let name = path
            .file_name()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Missing entry name"))?;
        Ok((std::fs::File::open(parent)?, CString::new(name.as_bytes())?))
    };
    let (source_parent, source) = parts(source)?;
    let (target_parent, target) = parts(target)?;
    rename_noreplace_at(&source_parent, &source, &target_parent, &target)
}

#[cfg(windows)]
pub(crate) fn windows_path(path: &Path) -> io::Result<Vec<u16>> {
    use std::os::windows::ffi::OsStrExt;
    // Resolve only the parent: the final entry can be absent. Canonicalization
    // restores the verbatim namespace needed for descendants beyond MAX_PATH.
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Missing parent directory"))?
        .canonicalize()?;
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Missing entry name"))?;
    let mut value: Vec<u16> = parent.join(name).as_os_str().encode_wide().collect();
    if value.contains(&0) || value.len() >= 32_767 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Path contains NUL or exceeds the Windows path limit",
        ));
    }
    value.push(0);
    Ok(value)
}

#[cfg(windows)]
pub(crate) fn rename_noreplace(source: &Path, target: &Path) -> io::Result<()> {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn MoveFileExW(source: *const u16, target: *const u16, flags: u32) -> i32;
    }
    let source = windows_path(source)?;
    let target = windows_path(target)?;
    // SAFETY: live, terminated UTF-16 paths. WRITE_THROUGH does not permit
    // replacement, cross-volume copying, or delayed publication.
    if unsafe { MoveFileExW(source.as_ptr(), target.as_ptr(), 8) } == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn rename_noreplace(_source: &Path, _target: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Atomic no-replace publication is unavailable on this platform",
    ))
}

#[cfg(test)]
mod tests {
    use super::rename_noreplace;
    #[test]
    fn publication_preserves_existing_files_and_empty_directories() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let target = root.path().join("target");
        std::fs::write(&source, "new").unwrap();
        std::fs::write(&target, "old").unwrap();
        assert!(rename_noreplace(&source, &target).is_err());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "old");
        assert_eq!(std::fs::read_to_string(&source).unwrap(), "new");
        std::fs::remove_file(&source).unwrap();
        std::fs::remove_file(&target).unwrap();
        std::fs::create_dir(&source).unwrap();
        std::fs::create_dir(&target).unwrap();
        assert!(rename_noreplace(&source, &target).is_err());
        assert!(source.is_dir() && target.is_dir());
        std::fs::remove_dir(&target).unwrap();
        rename_noreplace(&source, &target).unwrap();
        assert!(!source.exists() && target.is_dir());
    }
}
