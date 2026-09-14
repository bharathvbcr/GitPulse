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

/// Pin every ancestor without following symlinks. Optional parent creation
/// happens relative to the preceding pinned descriptor, never a path re-walk.
#[cfg(unix)]
pub(crate) fn pin_parent(path: &Path, create: bool) -> io::Result<std::fs::File> {
    if !path.is_absolute() || path.file_name().is_none() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Expected an absolute file path",
        ));
    }
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Missing parent"))?;
    open_dir_chain(parent, create).map(|pinned| pinned.dir)
}

/// A pinned directory, and whether its *name* is beyond another user's reach.
#[cfg(unix)]
pub(crate) struct PinnedDir {
    pub(crate) dir: std::fs::File,
    /// True when no principal other than this user or root can rename any
    /// component of the path, so the name cannot be made to point somewhere
    /// else after it has been resolved. False whenever that cannot be
    /// established.
    ///
    /// Mode bits are the same signal Git's own `safe.directory` ownership
    /// check uses. It does not model POSIX/NFSv4 ACLs: an ACL granting another
    /// user write on an ancestor would not show up here, so this answers
    /// "provably private", never "provably shared".
    pub(crate) exclusive: bool,
    /// The first directory that cost the path its exclusivity, so a refusal can
    /// name what to change instead of only saying no.
    pub(crate) shared_holder: Option<std::path::PathBuf>,
}

/// Pin `dir` itself, walking it component by component without following a
/// symlink at any of them.
///
/// The descriptor is bound to the directory's inode, so renaming or replacing
/// any component of the path afterwards cannot redirect work done through it.
/// That is the difference between validating a path and validating the object
/// a path happened to name a moment ago.
#[cfg(unix)]
/// The identity of the repository the pinned directory belongs to, read
/// through descriptors instead of names.
///
/// [`crate::engine::git_cli::find_git_root`] walks a *path*, so each step is a
/// fresh name lookup that a concurrent rename can redirect. This walks the same
/// tree with `openat(fd, "..")`, where the kernel derives the parent from the
/// object itself, so the answer describes the directory that was actually
/// pinned and no rename running alongside can change it.
///
/// The two tests mirror `find_git_root` exactly — a `.git` entry, resolved
/// through a symlink the way `Path::exists` does, or a bare layout of a `HEAD`
/// file beside an `objects` directory — so a legitimate checkout is never
/// classified differently by the two walks.
#[cfg(unix)]
pub(crate) fn repository_identity_of_pin(dir: &std::fs::File) -> io::Result<Option<(u64, u64)>> {
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::fs::MetadataExt;

    // A duplicate, so the caller keeps the descriptor it will hand the child.
    let mut current = dir.try_clone()?;
    for _ in 0..256 {
        let metadata = current.metadata()?;
        let here = (metadata.dev(), metadata.ino());
        if holds_a_repository(&current) {
            return Ok(Some(here));
        }
        // SAFETY: live directory descriptor and a NUL-terminated literal.
        let fd = unsafe {
            libc::openat(
                current.as_raw_fd(),
                c"..".as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: successful openat returned a newly owned descriptor.
        let parent = unsafe { std::fs::File::from_raw_fd(fd) };
        let above = parent.metadata()?;
        if (above.dev(), above.ino()) == here {
            // `..` of the filesystem root is itself; there is nowhere left.
            return Ok(None);
        }
        current = parent;
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "File path exceeds the directory depth limit",
    ))
}

/// Does this directory look like the root of a repository?
#[cfg(unix)]
fn holds_a_repository(dir: &std::fs::File) -> bool {
    if entry_mode(dir, c".git").is_some() {
        return true;
    }
    let head = entry_mode(dir, c"HEAD");
    let objects = entry_mode(dir, c"objects");
    matches!(head, Some(mode) if mode & libc::S_IFMT == libc::S_IFREG)
        && matches!(objects, Some(mode) if mode & libc::S_IFMT == libc::S_IFDIR)
}

/// `st_mode` of one entry under a pinned directory, or `None` when it cannot be
/// read for any reason — the same answer `Path::exists` gives, so this walk and
/// `find_git_root` agree on dangling links and unreadable entries too.
#[cfg(unix)]
fn entry_mode(dir: &std::fs::File, name: &std::ffi::CStr) -> Option<libc::mode_t> {
    use std::os::fd::AsRawFd;
    let mut status: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: live directory descriptor and a NUL-terminated literal name.
    let rc = unsafe { libc::fstatat(dir.as_raw_fd(), name.as_ptr(), &mut status, 0) };
    (rc == 0).then_some(status.st_mode)
}

/// The no-follow pin is the unix half of this seam: [`PinnedDir`] and the
/// descriptor walk behind it are unix-only, and Windows pins a path through
/// [`pin_dir_chain`] instead. Without this gate the Windows build type-checks a
/// signature naming items that exist nowhere on that platform, so the failure
/// lands in the library rather than at the one caller that needs it — and
/// `pin_and_admit`, the only caller outside tests, is itself `#[cfg(unix)]`.
#[cfg(unix)]
pub(crate) fn pin_dir_nofollow(dir: &Path) -> io::Result<PinnedDir> {
    if !dir.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Expected an absolute directory path",
        ));
    }
    open_dir_chain(dir, false)
}

#[cfg(unix)]
fn held_by_a_trusted_owner(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    let owner = metadata.uid();
    // SAFETY: `geteuid` reads this process's own credentials and cannot fail.
    owner == unsafe { libc::geteuid() } || owner == 0
}

/// Is `entry` beyond the reach of every principal but this user and root?
///
/// Answers the question the name asks, in the direction the name asks it:
/// `true` means nobody else can rename this entry away.
///
/// Renaming an entry needs write permission on the directory holding it, so
/// the holder decides — except when the holder is sticky, where only the
/// entry's own owner or root may rename it. That carve-out is what keeps a
/// world-writable `/tmp` or `/Users/Shared` usable: without it every checkout
/// under one would read as shared and be refused a terminal.
#[cfg(unix)]
fn holder_keeps_entry_private(holder: &std::fs::Metadata, entry: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    if !held_by_a_trusted_owner(holder) {
        return false;
    }
    if holder.mode() & 0o022 == 0 {
        return true;
    }
    holder.mode() & 0o1000 != 0 && held_by_a_trusted_owner(entry)
}

/// Shared walk behind [`pin_parent`] and [`pin_dir_nofollow`]: open each
/// component of `dir` relative to the previous descriptor, never re-walking a
/// path and never following a symlink.
#[cfg(unix)]
fn open_dir_chain(dir: &Path, create: bool) -> io::Result<PinnedDir> {
    use std::ffi::CString;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;
    use std::path::Component;
    let mut directory = std::fs::File::open("/")?;
    // Optional throughout: this walk also backs `pin_parent`, which every file
    // save goes through, and an unreadable stat must cost the path its
    // exclusivity — never turn a save that used to work into an error.
    let mut holder = directory.metadata().ok();
    let mut walked = std::path::PathBuf::from("/");
    let mut shared_holder =
        (!holder.as_ref().is_some_and(held_by_a_trusted_owner)).then(|| walked.clone());
    for (depth, component) in dir.components().enumerate() {
        if depth >= 256 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "File path exceeds the directory depth limit",
            ));
        }
        if component == Component::RootDir {
            continue;
        }
        let Component::Normal(part) = component else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "File path must have normalized parents",
            ));
        };
        let name = CString::new(part.as_bytes())?;
        let open = || {
            // SAFETY: live directory fd and NUL-terminated single component.
            unsafe {
                libc::openat(
                    directory.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_RDONLY
                        | libc::O_DIRECTORY
                        | libc::O_NOFOLLOW
                        | libc::O_CLOEXEC
                        | libc::O_NONBLOCK,
                )
            }
        };
        let mut fd = open();
        if fd < 0 && create && io::Error::last_os_error().kind() == io::ErrorKind::NotFound {
            // SAFETY: creates one name under the pinned parent. A raced entry
            // is inspected by the no-follow open below, never accepted blindly.
            if unsafe { libc::mkdirat(directory.as_raw_fd(), name.as_ptr(), 0o755) } != 0
                && io::Error::last_os_error().kind() != io::ErrorKind::AlreadyExists
            {
                return Err(io::Error::last_os_error());
            }
            fd = open();
        }
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: successful openat returned a newly owned descriptor.
        directory = unsafe { std::fs::File::from_raw_fd(fd) };
        let entry = directory.metadata().ok();
        // Whether THIS component can be swapped is decided by the directory
        // holding it, not by the component itself. An unknown holder or entry
        // answers "not private", which is the safe direction.
        let private = match (&holder, &entry) {
            (Some(holder), Some(entry)) => holder_keeps_entry_private(holder, entry),
            _ => false,
        };
        if shared_holder.is_none() && !private {
            shared_holder = Some(walked.clone());
        }
        walked.push(part);
        holder = entry;
    }
    Ok(PinnedDir {
        dir: directory,
        exclusive: shared_holder.is_none(),
        shared_holder,
    })
}

#[cfg(windows)]
pub(crate) fn windows_file_identity(file: &std::fs::File) -> io::Result<(u64, [u8; 16])> {
    use std::ffi::c_void;
    use std::os::windows::io::AsRawHandle;
    #[repr(C)]
    #[derive(Default)]
    struct FileIdInfo {
        volume: u64,
        id: [u8; 16],
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetFileInformationByHandleEx(
            handle: *mut c_void,
            class: i32,
            info: *mut c_void,
            size: u32,
        ) -> i32;
    }
    let mut info = FileIdInfo::default();
    // SAFETY: File owns the live handle; class 18 is FILE_ID_INFO with the
    // documented repr(C) layout and exact output-buffer size. No fallback to
    // timestamps: callers must fail closed if the filesystem lacks file IDs.
    if unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            18,
            (&mut info as *mut FileIdInfo).cast(),
            std::mem::size_of::<FileIdInfo>() as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok((info.volume, info.id))
}

#[cfg(windows)]
pub(crate) fn pin_directory(path: &Path) -> io::Result<std::fs::File> {
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
    let file = std::fs::OpenOptions::new()
        // LIST_DIRECTORY | READ_ATTRIBUTES participates in share checks;
        // omitting SHARE_DELETE pins this directory against rename/removal.
        .access_mode(0x81)
        .share_mode(1 | 2)
        .custom_flags(0x0020_0000 | 0x0200_0000)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_dir() || metadata.file_attributes() & 0x400 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Parent must be a real directory, not a reparse point",
        ));
    }
    Ok(file)
}

#[cfg(windows)]
pub(crate) fn pin_parents(path: &Path, create: bool) -> io::Result<Vec<std::fs::File>> {
    if !path.is_absolute() || path.file_name().is_none() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Expected an absolute file path",
        ));
    }
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Missing parent"))?;
    pin_dir_chain(parent, create)
}

/// Pin `dir` and every ancestor. Windows has no `O_NOFOLLOW` walk, so the
/// guarantee comes from the handles themselves: [`pin_directory`] omits
/// `FILE_SHARE_DELETE`, so while these are held no component of the path can
/// be renamed or removed, and none of them may be a reparse point.
#[cfg(windows)]
pub(crate) fn pin_dir_chain(dir: &Path, create: bool) -> io::Result<Vec<std::fs::File>> {
    use std::path::{Component, PathBuf};
    if !dir.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Expected an absolute directory path",
        ));
    }
    let mut current = PathBuf::new();
    let mut handles = Vec::new();
    for component in dir.components() {
        if handles.len() >= 256 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "File path exceeds the directory depth limit",
            ));
        }
        match component {
            Component::Prefix(_) => current.push(component),
            Component::RootDir | Component::Normal(_) => {
                current.push(component);
                let handle = match pin_directory(&current) {
                    Err(error) if create && error.kind() == io::ErrorKind::NotFound => {
                        match std::fs::create_dir(&current) {
                            Ok(()) => {}
                            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                            Err(error) => return Err(error),
                        }
                        pin_directory(&current)?
                    }
                    other => other?,
                };
                handles.push(handle);
            }
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "File path must have normalized parents",
                ))
            }
        }
    }
    Ok(handles)
}

/// Preserve Linux ownership and extended metadata before atomic publication.
/// Capability and set-id privileges are cleared, as they are by an ordinary
/// content write. Any unavailable metadata operation leaves the original intact.
#[cfg(target_os = "linux")]
pub(crate) fn preserve_metadata(source: &std::fs::File, target: &std::fs::File) -> io::Result<()> {
    use std::collections::BTreeMap;
    use std::ffi::CString;
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    fn attributes(file: &std::fs::File) -> io::Result<BTreeMap<CString, Vec<u8>>> {
        let mut names = vec![0u8; 64 * 1024];
        // SAFETY: writable bounded name buffer and a live file descriptor.
        let count =
            unsafe { libc::flistxattr(file.as_raw_fd(), names.as_mut_ptr().cast(), names.len()) };
        if count < 0 {
            let error = io::Error::last_os_error();
            return if error.raw_os_error() == Some(libc::ENOTSUP) {
                Ok(BTreeMap::new())
            } else {
                Err(error)
            };
        }
        names.truncate(count as usize);
        let mut entries = BTreeMap::new();
        let mut total = 0usize;
        for name in names
            .split(|byte| *byte == 0)
            .filter(|name| !name.is_empty())
        {
            if entries.len() >= 512 {
                return Err(io::Error::other("Too many file attributes to preserve"));
            }
            let name = CString::new(name)?;
            let mut value = vec![0u8; 64 * 1024];
            // SAFETY: terminated name, live descriptor and bounded value buffer.
            let count = unsafe {
                libc::fgetxattr(
                    file.as_raw_fd(),
                    name.as_ptr(),
                    value.as_mut_ptr().cast(),
                    value.len(),
                )
            };
            if count < 0 {
                return Err(io::Error::last_os_error());
            }
            value.truncate(count as usize);
            total += value.len();
            if total > 1024 * 1024 {
                return Err(io::Error::other(
                    "File metadata exceeds the 1 MiB preservation limit",
                ));
            }
            entries.insert(name, value);
        }
        Ok(entries)
    }

    let before = source.metadata()?;
    let saved = attributes(source)?;
    let target_meta = target.metadata()?;
    if (before.uid(), before.gid()) != (target_meta.uid(), target_meta.gid()) {
        // SAFETY: target is our private temporary file; IDs come from its source.
        if unsafe { libc::fchown(target.as_raw_fd(), before.uid(), before.gid()) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    target.set_permissions(std::fs::Permissions::from_mode(before.mode() & 0o777))?;
    // A new file may inherit a directory ACL absent from the original. Remove
    // extra attributes instead of silently widening access through inheritance.
    let inherited = attributes(target)?;
    for name in inherited.keys() {
        if !saved.contains_key(name) || name.as_bytes() == b"security.capability" {
            // SAFETY: live descriptor and a terminated name read from this file.
            if unsafe { libc::fremovexattr(target.as_raw_fd(), name.as_ptr()) } != 0 {
                return Err(io::Error::last_os_error());
            }
        }
    }
    for (name, value) in &saved {
        if name.as_bytes() == b"security.capability" || inherited.get(name) == Some(value) {
            continue;
        }
        // SAFETY: live descriptors, terminated attribute name, exact byte length.
        if unsafe {
            libc::fsetxattr(
                target.as_raw_fd(),
                name.as_ptr(),
                value.as_ptr().cast(),
                value.len(),
                0,
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
    }
    let after = source.metadata()?;
    if (
        before.uid(),
        before.gid(),
        before.mode(),
        before.ctime(),
        before.ctime_nsec(),
    ) != (
        after.uid(),
        after.gid(),
        after.mode(),
        after.ctime(),
        after.ctime_nsec(),
    ) || attributes(source)? != saved
    {
        return Err(io::Error::other(
            "File metadata changed while preparing the save",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::rename_noreplace;

    #[cfg(windows)]
    #[test]
    fn windows_identity_survives_rename_and_distinguishes_replacements() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("directory");
        let moved = root.path().join("moved");
        std::fs::create_dir(&path).unwrap();
        let before = super::windows_file_identity(&super::pin_directory(&path).unwrap()).unwrap();
        std::fs::rename(&path, &moved).unwrap();
        assert_eq!(
            before,
            super::windows_file_identity(&super::pin_directory(&moved).unwrap()).unwrap()
        );
        std::fs::create_dir(&path).unwrap();
        assert_ne!(
            before,
            super::windows_file_identity(&super::pin_directory(&path).unwrap()).unwrap()
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn ordinary_saves_preserve_linux_attributes_and_permissions() {
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("file");
        std::fs::write(&path, b"old").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        let file = std::fs::File::open(&path).unwrap();
        let before = file.metadata().unwrap();
        let value = b"preserve this attribute";
        // SAFETY: live fixture fd, terminated key and exact byte length.
        assert_eq!(
            unsafe {
                libc::fsetxattr(
                    file.as_raw_fd(),
                    c"user.gitpulse-test".as_ptr(),
                    value.as_ptr().cast(),
                    value.len(),
                    0,
                )
            },
            0
        );
        crate::diff::write_regular(&path, b"new").unwrap();
        let saved = std::fs::File::open(&path).unwrap();
        let after = saved.metadata().unwrap();
        let mut actual = [0u8; 64];
        // SAFETY: live fixture fd, terminated key and writable bounded buffer.
        let count = unsafe {
            libc::fgetxattr(
                saved.as_raw_fd(),
                c"user.gitpulse-test".as_ptr(),
                actual.as_mut_ptr().cast(),
                actual.len(),
            )
        };
        assert_eq!(count, value.len() as isize);
        assert_eq!(&actual[..value.len()], value);
        assert_eq!(
            (before.uid(), before.gid(), before.mode() & 0o777),
            (after.uid(), after.gid(), after.mode() & 0o777)
        );
        assert_eq!(std::fs::read(path).unwrap(), b"new");
    }
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
