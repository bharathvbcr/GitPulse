//! Descriptor-based access to managed files. Directory links are refused before
//! a child is opened or created; file opens never truncate before validation.
//! Unix uses openat/O_NOFOLLOW. Windows retains directory handles without delete
//! sharing and opens reparse points themselves. No guarantees are made against
//! privileged processes or another process already executing as the same user.

use std::ffi::{OsStr, OsString};
use std::fs::{File, Metadata};
use std::io::{self, Read, Seek, Write};
use std::ops::{Deref, DerefMut};
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Read,
    ReadWrite,
    Append,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Creation {
    Never,
    IfMissing,
    New,
}

fn refused(reason: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, reason.into())
}

fn check_child_name(name: &OsStr) -> io::Result<()> {
    let mut components = Path::new(name).components();
    if !matches!(components.next(), Some(Component::Normal(_))) || components.next().is_some() {
        return Err(refused("managed child must be one filename"));
    }
    check_name(name)
}

fn check_name(name: &OsStr) -> io::Result<()> {
    if name.is_empty() {
        return Err(refused("managed filename is empty"));
    }
    #[cfg(windows)]
    if name.to_string_lossy().contains(':') {
        return Err(refused("alternate data streams are not managed file paths"));
    }
    Ok(())
}

fn absolute(path: &Path) -> io::Result<PathBuf> {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    // macOS exposes these fixed system-root aliases. Resolve only this prefix,
    // never arbitrary user/repository links or a path containing one later on.
    #[cfg(target_os = "macos")]
    for name in ["var", "tmp"] {
        let alias = PathBuf::from("/").join(name);
        if let Ok(rest) = path.strip_prefix(&alias) {
            let target = PathBuf::from("/private").join(name);
            let metadata = std::fs::symlink_metadata(&alias)?;
            if metadata.file_type().is_symlink() {
                use std::os::unix::fs::MetadataExt;
                let link = std::fs::read_link(&alias)?;
                let link = if link.is_absolute() {
                    link
                } else {
                    Path::new("/").join(link)
                };
                if metadata.uid() != 0 || link != target {
                    return Err(refused("unrecognized system-root alias"));
                }
                return Ok(target.join(rest));
            }
        }
    }
    Ok(path)
}

/// Directory handles remain alive through every operation on a child file.
#[derive(Debug)]
pub struct PinnedDir {
    path: PathBuf,
    chain: Vec<File>,
}

impl PinnedDir {
    pub fn open(path: &Path, create: bool) -> io::Result<Self> {
        let path = absolute(path)?;
        let mut pinned = Self {
            path: PathBuf::new(),
            chain: Vec::new(),
        };
        for component in path.components() {
            if pinned.chain.len() >= 256 {
                return Err(refused("managed path exceeds 256 directory components"));
            }
            match component {
                Component::Prefix(prefix) => {
                    if matches!(prefix.kind(), std::path::Prefix::DeviceNS(_)) {
                        return Err(refused("device namespaces are not managed file paths"));
                    }
                    pinned.path.push(prefix.as_os_str());
                }
                Component::RootDir => {
                    pinned.path.push(component.as_os_str());
                    pinned.chain.push(open_directory_root(&pinned.path)?);
                }
                Component::CurDir => {}
                Component::Normal(_) | Component::ParentDir => {
                    let name = component.as_os_str();
                    check_name(name)?;
                    let parent = pinned
                        .chain
                        .last()
                        .ok_or_else(|| refused("managed path has no filesystem root"))?;
                    let next_path = pinned.path.join(name);
                    let child = open_directory_child(parent, &next_path, name, create)
                        .map_err(|error| io::Error::new(error.kind(), format!("cannot open managed directory {} without following links: {error}; use its real path", next_path.display())))?;
                    pinned.path = next_path;
                    pinned.chain.push(child);
                }
            }
        }
        if pinned.chain.is_empty() {
            return Err(refused("managed directory is missing"));
        }
        Ok(pinned)
    }

    pub fn open_file(
        &self,
        name: &OsStr,
        access: Access,
        creation: Creation,
    ) -> io::Result<SafeFile> {
        check_child_name(name)?;
        let file = open_regular_child(self, name, access, creation)?;
        let initial = check_regular(&file, access != Access::Read)?;
        let parent = Self {
            path: self.path.clone(),
            chain: self
                .chain
                .iter()
                .map(File::try_clone)
                .collect::<io::Result<_>>()?,
        };
        Ok(SafeFile {
            file,
            _parent: parent,
            name: name.to_os_string(),
            initial,
            access,
        })
    }

    pub fn rename_child(&self, from: &OsStr, to: &OsStr) -> io::Result<()> {
        check_child_name(from)?;
        check_child_name(to)?;
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            let parent = self
                .chain
                .last()
                .ok_or_else(|| refused("managed directory is missing"))?;
            let from = c_name(from)?;
            let to = c_name(to)?;
            // SAFETY: both names and the pinned directory descriptor are live.
            if unsafe {
                libc::renameat(
                    parent.as_raw_fd(),
                    from.as_ptr(),
                    parent.as_raw_fd(),
                    to.as_ptr(),
                )
            } != 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }
        #[cfg(windows)]
        {
            std::fs::rename(self.path.join(from), self.path.join(to))
        }
    }

    pub fn remove_child(&self, name: &OsStr) -> io::Result<()> {
        check_child_name(name)?;
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            let parent = self
                .chain
                .last()
                .ok_or_else(|| refused("managed directory is missing"))?;
            let name = c_name(name)?;
            // SAFETY: name and pinned directory descriptor are live; flags=0 removes a file.
            if unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), 0) } != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }
        #[cfg(windows)]
        {
            std::fs::remove_file(self.path.join(name))
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn metadata(&self) -> io::Result<Metadata> {
        self.chain
            .last()
            .ok_or_else(|| refused("managed directory is missing"))?
            .metadata()
    }

    /// Used only for DevMap's generated Unix runtime directory, after ownership
    /// is established and before opening the lock or endpoint beneath it.
    #[cfg(unix)]
    pub fn make_private(&self) -> io::Result<()> {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let file = self
            .chain
            .last()
            .ok_or_else(|| refused("managed directory is missing"))?;
        let metadata = file.metadata()?;
        // SAFETY: geteuid has no preconditions.
        if metadata.uid() != unsafe { libc::geteuid() } {
            return Err(refused("managed runtime directory belongs to another user"));
        }
        file.set_permissions(std::fs::Permissions::from_mode(0o700))
    }
}

/// A regular file and the directory handles used to reach it. On Windows the
/// ancestors cannot be renamed while these handles are held.
#[derive(Debug)]
pub struct SafeFile {
    file: File,
    _parent: PinnedDir,
    name: OsString,
    initial: Metadata,
    access: Access,
}

/// Read source selected by a stored repository path. Source aliases are allowed
/// only when their canonical target stays in the selected repository; the final
/// open walks that target again without following links. A missing ordinary
/// path stays NotFound so previews can distinguish new files from refused reads.
pub fn read_repo_source(root: Option<&Path>, path: &str, limit: u64) -> io::Result<String> {
    use std::path::Component;
    let raw = Path::new(path);
    if path.is_empty()
        || raw
            .components()
            .any(|part| matches!(part, Component::ParentDir))
    {
        return Err(refused("source path is empty or contains parent traversal"));
    }
    let recorded_root = root;
    let root = match root {
        Some(root) => root.canonicalize()?,
        None => std::env::current_dir()?.canonicalize()?,
    };
    let candidate = if raw.is_absolute() {
        let selected = recorded_root
            .ok_or_else(|| refused("absolute source path has no recorded repository root"))?;
        let relative = raw
            .strip_prefix(selected)
            .or_else(|_| raw.strip_prefix(&root))
            .map_err(|_| refused("source path is outside the repository"))?;
        root.join(relative)
    } else {
        root.join(raw)
    };
    let resolved = match candidate.canonicalize() {
        Ok(resolved) => resolved,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            // Do not turn a dangling alias or a linked ancestor into an absent
            // source revision. Opening without creation is nonblocking.
            return match SafeFile::open(&candidate, Access::Read, Creation::Never) {
                Err(error) => Err(error),
                Ok(_) => Err(refused("source changed during path resolution")),
            };
        }
        Err(error) => return Err(error),
    };
    if !resolved.starts_with(&root) {
        return Err(refused("source resolves outside the repository"));
    }
    let limit = limit.min(crate::MAX_SOURCE_BYTES);
    let mut file = SafeFile::open(&resolved, Access::Read, Creation::Never)?;
    if file.metadata()?.len() > limit {
        return Err(io::Error::new(
            io::ErrorKind::FileTooLarge,
            format!("source exceeds {limit} byte read limit"),
        ));
    }
    file.read_text(limit)
}

impl Deref for SafeFile {
    type Target = File;
    fn deref(&self) -> &File {
        &self.file
    }
}
impl DerefMut for SafeFile {
    fn deref_mut(&mut self) -> &mut File {
        &mut self.file
    }
}

impl SafeFile {
    pub fn open(path: &Path, access: Access, creation: Creation) -> io::Result<Self> {
        let name = path
            .file_name()
            .ok_or_else(|| refused("managed file has no filename"))?;
        check_name(name)?;
        let parent = PinnedDir::open(
            path.parent().unwrap_or_else(|| Path::new(".")),
            creation != Creation::Never,
        )?;
        parent.open_file(name, access, creation)
    }

    #[cfg(unix)]
    pub fn is_owned_by_current_user(&self) -> io::Result<bool> {
        use std::os::unix::fs::MetadataExt;
        // SAFETY: geteuid has no preconditions.
        Ok(self.file.metadata()?.uid() == unsafe { libc::geteuid() })
    }

    pub fn require_owned(&self) -> io::Result<()> {
        check_regular(&self.file, true).map(|_| ())
    }

    pub fn read_text(&mut self, limit: u64) -> io::Result<String> {
        if self.initial.len() > limit {
            return Err(refused(format!("file exceeds {limit} byte read limit")));
        }
        let mut bytes = Vec::with_capacity(self.initial.len() as usize);
        Read::by_ref(&mut self.file)
            .take(limit.saturating_add(1))
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > limit {
            return Err(refused(format!("file exceeds {limit} byte read limit")));
        }
        self.check_unchanged()?;
        String::from_utf8(bytes).map_err(|error| refused(error.to_string()))
    }

    /// Read the prefix of an append-only ledger that was there when it opened.
    ///
    /// [`read_text`](Self::read_text) treats any size change as tampering,
    /// which is right for a file only one process should be writing. A session
    /// ledger is appended by every daemon at once, so growth under the reader
    /// is its normal state, not an attack: measured on this tree, one
    /// concurrent writer had 134 of 400 reads refused. Because the rotation
    /// that retires the ledger only runs after a successful read, each refusal
    /// also left the file to grow, widening the window for the next one.
    ///
    /// So this reads the bytes present at open and ignores whatever arrived
    /// after: a prefix of an append-only file is a consistent snapshot of it,
    /// and because a record is appended in a single write, that prefix always
    /// ends on a record boundary. Replacement (a different inode) and
    /// truncation (it shrank) are still refused — those are what tampering
    /// looks like, and neither is something an appender does.
    pub fn read_text_prefix(&mut self, limit: u64) -> io::Result<String> {
        let snapshot = self.initial.len();
        if snapshot > limit {
            return Err(refused(format!("file exceeds {limit} byte read limit")));
        }
        let mut bytes = Vec::with_capacity(snapshot as usize);
        Read::by_ref(&mut self.file)
            .take(snapshot)
            .read_to_end(&mut bytes)?;
        self.check_appended_only()?;
        String::from_utf8(bytes).map_err(|error| refused(error.to_string()))
    }

    /// Identity check for a file other processes are expected to append to.
    ///
    /// The same checks [`check_unchanged`](Self::check_unchanged) makes, minus
    /// the equal-length requirement: the file may have grown, and may not have
    /// been swapped or truncated.
    fn check_appended_only(&self) -> io::Result<()> {
        let current = check_regular(&self.file, self.access != Access::Read)?;
        let named = open_regular_child(&self._parent, &self.name, Access::Read, Creation::Never)?;
        let named_metadata = check_regular(&named, self.access != Access::Read)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if current.dev() != named_metadata.dev() || current.ino() != named_metadata.ino() {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "managed file was replaced while being examined",
                ));
            }
        }
        if current.len() < self.initial.len() {
            return Err(refused("managed file was truncated while being examined"));
        }
        Ok(())
    }

    pub fn matches_contents(&mut self, contents: &[u8]) -> io::Result<bool> {
        if self.initial.len() != contents.len() as u64 {
            return Ok(false);
        }
        let mut buffer = [0u8; 8192];
        for chunk in contents.chunks(buffer.len()) {
            self.file.read_exact(&mut buffer[..chunk.len()])?;
            if &buffer[..chunk.len()] != chunk {
                return Ok(false);
            }
        }
        self.check_unchanged()?;
        Ok(true)
    }

    pub fn check_unchanged(&self) -> io::Result<()> {
        let current = check_regular(&self.file, self.access != Access::Read)?;
        let named = open_regular_child(&self._parent, &self.name, Access::Read, Creation::Never)?;
        let named_metadata = check_regular(&named, self.access != Access::Read)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if current.dev() != named_metadata.dev() || current.ino() != named_metadata.ino() {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "managed file was replaced while being examined",
                ));
            }
        }
        if named_metadata.len() != current.len()
            || named_metadata.modified()? != current.modified()?
        {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "managed file was replaced while being examined",
            ));
        }
        if current.len() != self.initial.len() || current.modified()? != self.initial.modified()? {
            return Err(refused("managed file changed while being examined"));
        }
        Ok(())
    }

    pub fn rewrite(&mut self, contents: &[u8]) -> io::Result<()> {
        if self.access != Access::ReadWrite {
            return Err(refused("managed file was not opened for replacement"));
        }
        self.check_unchanged()?;
        self.file.rewind()?;
        self.file.set_len(0)?;
        self.file.write_all(contents)?;
        self.file.flush()?;
        self.initial = self.file.metadata()?;
        Ok(())
    }
}

/// No directory or file is created by this check. A missing component is safe
/// to create later through PinnedDir; a linked/unreadable component is refused.
pub fn preflight_write(path: &Path) -> io::Result<()> {
    match SafeFile::open(path, Access::Read, Creation::Never).and_then(|file| file.require_owned())
    {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

pub fn write(path: &Path, contents: &[u8]) -> io::Result<()> {
    SafeFile::open(path, Access::ReadWrite, Creation::IfMissing)?.rewrite(contents)
}

/// Preserve a final database-file alias while refusing linked ancestors. Both
/// the alias's parent and every target parent are checked before canonicalizing.
pub fn resolve_file_alias(path: &Path) -> io::Result<PathBuf> {
    let mut path = absolute(path)?;
    for links in 0..40 {
        match PinnedDir::open(path.parent().unwrap_or_else(|| Path::new(".")), false) {
            Ok(_parent) => {}
            Err(error) if links == 0 && error.kind() == io::ErrorKind::NotFound => return Ok(path),
            Err(error) => return Err(error),
        }
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                let target = std::fs::read_link(&path)?;
                path = if target.is_absolute() {
                    absolute(&target)?
                } else {
                    path.parent().unwrap_or_else(|| Path::new(".")).join(target)
                };
            }
            Ok(_) => {
                // On Windows non-symlink reparse points must also be refused.
                let _file = SafeFile::open(&path, Access::Read, Creation::Never)?;
                return path.canonicalize();
            }
            Err(error) if links == 0 && error.kind() == io::ErrorKind::NotFound => return Ok(path),
            Err(error) => return Err(error),
        }
    }
    Err(refused("database alias chain exceeds 40 links"))
}

pub fn file_link_count(file: &File) -> io::Result<u64> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(file.metadata()?.nlink())
    }
    #[cfg(windows)]
    {
        let links = windows_information(file)?.links;
        if links == 0 {
            Err(refused("file link count is unavailable"))
        } else {
            Ok(u64::from(links))
        }
    }
}

fn check_regular(file: &File, owned: bool) -> io::Result<Metadata> {
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(refused("managed file is not a regular file"));
    }
    #[cfg(unix)]
    if owned {
        use std::os::unix::fs::MetadataExt;
        if metadata.nlink() == 0 {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "managed file was unlinked during examination",
            ));
        }
        // SAFETY: geteuid has no preconditions.
        if metadata.uid() != unsafe { libc::geteuid() } || metadata.nlink() != 1 {
            return Err(refused(
                "managed file must have one link and belong to the current user",
            ));
        }
    }
    #[cfg(windows)]
    {
        let information = windows_information(file)?;
        if information.attributes & 0x400 != 0
            || information.links == 0
            || (owned && information.links != 1)
        {
            return Err(refused(
                "managed file is a reparse point or has an unsafe link count",
            ));
        }
    }
    Ok(metadata)
}

#[cfg(unix)]
fn open_directory_root(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
}

#[cfg(unix)]
fn c_name(name: &OsStr) -> io::Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::CString::new(name.as_bytes()).map_err(|_| refused("managed path contains NUL"))
}

#[cfg(unix)]
fn open_directory_child(
    parent: &File,
    _path: &Path,
    name: &OsStr,
    create: bool,
) -> io::Result<File> {
    use std::os::fd::{AsRawFd, FromRawFd};
    let name = c_name(name)?;
    let open = || {
        // SAFETY: the directory descriptor and NUL-terminated name remain live.
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            Err(io::Error::last_os_error())
        } else {
            // SAFETY: successful openat returned a new descriptor owned here.
            Ok(unsafe { File::from_raw_fd(fd) })
        }
    };
    match open() {
        Err(error) if create && error.kind() == io::ErrorKind::NotFound => {
            // SAFETY: parent/name are live; mode is supplied for the new directory.
            if unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) } != 0 {
                let error = io::Error::last_os_error();
                if error.kind() != io::ErrorKind::AlreadyExists {
                    return Err(error);
                }
            }
            open()
        }
        result => result,
    }
}

#[cfg(unix)]
fn open_regular_child(
    parent: &PinnedDir,
    name: &OsStr,
    access: Access,
    creation: Creation,
) -> io::Result<File> {
    use std::os::fd::{AsRawFd, FromRawFd};
    let name = c_name(name)?;
    let mut flags = libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC;
    flags |= match access {
        Access::Read => libc::O_RDONLY,
        Access::ReadWrite => libc::O_RDWR,
        Access::Append => libc::O_WRONLY | libc::O_APPEND,
    };
    flags |= match creation {
        Creation::Never => 0,
        Creation::IfMissing => libc::O_CREAT,
        Creation::New => libc::O_CREAT | libc::O_EXCL,
    };
    let directory = parent
        .chain
        .last()
        .ok_or_else(|| refused("managed directory is missing"))?;
    let mut retries = 0;
    loop {
        // SAFETY: directory/name are live and a creation mode is always provided.
        let fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags, 0o600) };
        if fd >= 0 {
            // SAFETY: successful openat returned a new descriptor owned here.
            return Ok(unsafe { File::from_raw_fd(fd) });
        }
        let error = io::Error::last_os_error();
        // Concurrent O_CREAT | O_NOFOLLOW opens can report ENOENT on macOS
        // while a peer creates the same name. Retry only that creation race,
        // at most 16 opens, retaining the pinned parent and every safety flag.
        if creation != Creation::IfMissing
            || error.kind() != io::ErrorKind::NotFound
            || retries == 15
        {
            return Err(error);
        }
        retries += 1;
    }
}

#[cfg(windows)]
fn windows_options() -> std::fs::OpenOptions {
    use std::os::windows::fs::OpenOptionsExt;
    let mut options = std::fs::OpenOptions::new();
    // FILE_SHARE_READ | FILE_SHARE_WRITE; no FILE_SHARE_DELETE. The held parent
    // handles deny rename/delete while the leaf operation is in progress.
    options
        .share_mode(0x1 | 0x2)
        .custom_flags(0x00200000 | 0x02000000);
    options
}

#[cfg(windows)]
fn open_directory_root(path: &Path) -> io::Result<File> {
    let file = windows_options().read(true).open(path)?;
    if !file.metadata()?.is_dir() || windows_information(&file)?.attributes & 0x400 != 0 {
        return Err(refused("managed directory is not an ordinary directory"));
    }
    Ok(file)
}

#[cfg(windows)]
fn open_directory_child(
    _parent: &File,
    path: &Path,
    _name: &OsStr,
    create: bool,
) -> io::Result<File> {
    match open_directory_root(path) {
        Err(error) if create && error.kind() == io::ErrorKind::NotFound => {
            match std::fs::create_dir(path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
            open_directory_root(path)
        }
        result => result,
    }
}

#[cfg(windows)]
fn open_regular_child(
    parent: &PinnedDir,
    name: &OsStr,
    access: Access,
    creation: Creation,
) -> io::Result<File> {
    let mut options = windows_options();
    match access {
        Access::Read => {
            options.read(true);
        }
        Access::ReadWrite => {
            options.read(true).write(true);
        }
        Access::Append => {
            options.append(true);
        }
    }
    match creation {
        Creation::Never => {}
        Creation::IfMissing => {
            options.create(true);
        }
        Creation::New => {
            options.create_new(true);
        }
    }
    options.open(parent.path.join(name))
}

#[cfg(windows)]
#[repr(C)]
struct WindowsInformation {
    attributes: u32,
    created: [u32; 2],
    accessed: [u32; 2],
    written: [u32; 2],
    volume: u32,
    size_high: u32,
    size_low: u32,
    links: u32,
    index_high: u32,
    index_low: u32,
}

#[cfg(windows)]
fn windows_information(file: &File) -> io::Result<WindowsInformation> {
    use std::os::windows::io::AsRawHandle;
    const _: [(); 52] = [(); std::mem::size_of::<WindowsInformation>()];
    #[link(name = "kernel32")]
    extern "system" {
        fn GetFileInformationByHandle(
            handle: *mut std::ffi::c_void,
            information: *mut WindowsInformation,
        ) -> i32;
    }
    let mut information = std::mem::MaybeUninit::uninit();
    // SAFETY: File owns a live handle and information is aligned ABI-sized storage.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), information.as_mut_ptr()) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the successful call initialized every field.
    Ok(unsafe { information.assume_init() })
}
