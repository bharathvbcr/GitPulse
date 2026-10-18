//! Worktree entries are accessed relative to a pinned, symlink-free directory.
//! A concurrent ancestor rename cannot redirect resolution I/O through a link.
use super::conflict_session::Worktree;
use std::path::Path;
#[cfg(not(windows))]
use std::path::PathBuf;

/// The three ways an entry transaction reports that another writer got there
/// first. [`concurrent_change`] recognises them so a save can answer with a
/// typed conflict rather than a string a caller would have to parse.
const CHANGED_BEFORE_PUBLISH: &str =
    "The working file or its directory changed; reload before saving";
const NEW_ENTRY_APPEARED: &str = "Cannot create the resolution without replacing a new entry";
const RACED_AFTER_PUBLISH: &str = "An external edit raced the save";
/// The Windows transaction's own wording for the same condition.
const WINDOWS_SOURCE_CHANGED: &str = "Conflict source changed";

fn concurrent_change(message: &str) -> bool {
    [
        CHANGED_BEFORE_PUBLISH,
        NEW_ENTRY_APPEARED,
        RACED_AFTER_PUBLISH,
        WINDOWS_SOURCE_CHANGED,
    ]
    .iter()
    .any(|known| message.starts_with(known))
}

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

    /// What stands at `path` for a create: the entry itself in whatever form
    /// (file, directory, link), never its target. Unlike [`read_for_save`] a
    /// directory is an answer here, not an error. The parent is created, which
    /// is what New Folder needs for its `.gitkeep`.
    ///
    /// `created` receives the directories this call made, so a create that
    /// then fails can take them away again.
    pub(super) fn read_for_create(
        path: &Path,
        created: &mut Vec<PathBuf>,
    ) -> Result<Worktree, String> {
        let directory =
            crate::fs_entry::pin_parent_recording(path, created).map_err(|e| e.to_string())?;
        read_at(&directory, &name(path)?)
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
            return Err(CHANGED_BEFORE_PUBLISH.into());
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
                return Err(format!("{NEW_ENTRY_APPEARED}: {}", error()));
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
            return Err(format!("{RACED_AFTER_PUBLISH}. Review the working file and the displaced content retained at {} before staging", recovery.path.display()));
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

/// A version of a file's content, small enough to carry in an editor draft and
/// compare on save: `"<len>:<digest hex>"` of the raw bytes, or `"absent"`
/// for a path with no file.
///
/// Content rather than mtime and size on purpose. The editor's textarea hands
/// back LF text while the save re-adds CRLF, so only the backend can say what
/// the bytes on disk are; and a timestamp misses a same-size rewrite inside
/// one tick while reporting a conflict for a bare `touch`. The digest is not a
/// security boundary: this guards against a lost update, and a 2^-64 collision
/// is the same risk as the write racing in just after the check.
pub(crate) fn content_token(bytes: Option<&[u8]>) -> String {
    match bytes {
        None => "absent".into(),
        Some(bytes) => format!("{}:{:016x}", bytes.len(), digest(bytes)),
    }
}

/// A 64-bit digest of `bytes`, fast enough to run over every file the editor
/// opens or saves (8 MiB at most).
///
/// This is an accidental-change detector, not a hash that holds against a
/// crafted input. Each block is absorbed by an invertible step, so someone who
/// knows the function can build two different files with one token. That is
/// acceptable here because the other writer is the user's own external editor,
/// an agent or `git`, none of which is trying to forge a token; the digest only
/// has to tell the edits they actually make from no edit at all. A
/// collision-resistant hash would need a dependency or a much slower pass. A
/// crafted multi-lane collision (two lanes edited in one block and repaired in
/// the next) remains possible; the tests cover the accidental shapes, not that.
///
/// FNV-1a was the first choice and cost more than reading the file: its one
/// multiply per byte is a serial dependency chain, so an 8 MiB open took
/// about 2.3x as long. This takes a word at a time on four lanes so the
/// multiplies overlap. The lanes are not independent: after every block each
/// absorbs a mix of all four, so a change confined to one lane at one offset
/// cannot be undone by a change to the same lane later. The tail bytes and the
/// length are folded in at the end. Tokens are compared within one running app
/// against bytes it just read, never stored, so the function may change.
fn digest(bytes: &[u8]) -> u64 {
    const PRIME: u64 = 0x9E37_79B9_7F4A_7C15;
    const FOLD: u64 = 0xC2B2_AE3D_27D4_EB4F;
    let mut lanes: [u64; 4] = [
        0xcbf2_9ce4_8422_2325,
        0x8422_2325_cbf2_9ce4,
        0x1000_0000_01b3_0001,
        0x0001_0000_0000_01b3,
    ];
    let (blocks, tail) = bytes.as_chunks::<32>();
    for block in blocks {
        for (lane, word) in lanes.iter_mut().zip(block.as_chunks::<8>().0) {
            let word = u64::from_le_bytes(*word);
            *lane = (*lane ^ word).wrapping_mul(PRIME).rotate_left(29);
        }
        let mix = (lanes[0]
            ^ lanes[1].rotate_left(16)
            ^ lanes[2].rotate_left(32)
            ^ lanes[3].rotate_left(48))
        .wrapping_mul(FOLD);
        for lane in &mut lanes {
            *lane = lane.wrapping_add(mix ^ (mix >> 29));
        }
    }
    let mut hash = bytes.len() as u64;
    for lane in lanes {
        hash = (hash ^ lane).wrapping_mul(PRIME).rotate_left(31);
    }
    for byte in tail {
        hash = (hash ^ u64::from(*byte)).wrapping_mul(PRIME);
    }
    hash ^ (hash >> 32)
}

/// What a write requires of the entry it replaces.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Expect<'a> {
    /// Replace whatever regular file is there, or create it.
    Any,
    /// Replace only a regular file holding exactly these bytes.
    Bytes(&'a [u8]),
    /// Replace only the entry whose [`content_token`] is this.
    Token(&'a str),
    /// Create only: nothing may stand at the path, in any form.
    Absent,
}

/// Why a write did not happen, or failed part-way.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum WriteFailure {
    /// An [`Expect::Absent`] write found an entry, including one that
    /// appeared between the check and the publish.
    AlreadyExists,
    /// The entry is not what the caller expected; `actual` is its token now.
    Changed {
        actual: String,
    },
    Other(String),
}

impl From<String> for WriteFailure {
    fn from(message: String) -> Self {
        Self::Other(message)
    }
}

impl std::fmt::Display for WriteFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyExists => f.write_str("File already exists"),
            Self::Changed { .. } => f.write_str("The file changed on disk"),
            Self::Other(message) => f.write_str(message),
        }
    }
}

/// The token of what is at `path` now, for a conflict report.
fn current_token(path: &Path) -> String {
    #[cfg(unix)]
    let now = unix::read(path);
    #[cfg(windows)]
    let now = windows::read(path);
    #[cfg(not(any(unix, windows)))]
    let now: Result<Worktree, String> = Err(String::new());
    match now {
        Ok(entry) if matches!(entry.mode.as_str(), "missing" | "100644" | "100755") => {
            content_token(entry.bytes.as_deref())
        }
        Ok(_) => "other".into(),
        Err(_) => "unreadable".into(),
    }
}

/// Reads `path` for a write: a create needs the entry in whatever form it has
/// (and its parent made), a save needs a regular file it may write.
#[cfg(unix)]
fn read_source(
    path: &Path,
    expected: Expect<'_>,
    cap: usize,
    created: &mut Vec<std::path::PathBuf>,
) -> Result<Worktree, String> {
    if matches!(expected, Expect::Absent) {
        unix::read_for_create(path, created)
    } else {
        unix::read_for_save(path, cap)
    }
}

/// The Windows pin records the directories it makes in `created`, as the unix
/// walk does, so a failed create removes them again through the same cleanup.
/// Compiled for `x86_64-pc-windows-msvc` by `cargo check` only: no Windows host
/// has run it.
#[cfg(windows)]
fn read_source(
    path: &Path,
    expected: Expect<'_>,
    cap: usize,
    created: &mut Vec<std::path::PathBuf>,
) -> Result<Worktree, String> {
    if matches!(expected, Expect::Absent) && std::fs::symlink_metadata(path).is_ok() {
        // Any entry at all refuses a create; its form does not matter.
        return Ok(Worktree {
            bytes: None,
            mode: "160000".into(),
        });
    }
    windows::read_for_save(path, cap, created)
}

/// Maps a failed publish to the typed failure when another writer caused it.
fn settled(path: &Path, expected: Expect<'_>, message: String) -> WriteFailure {
    if !concurrent_change(&message) {
        WriteFailure::Other(message)
    } else if matches!(expected, Expect::Absent) {
        WriteFailure::AlreadyExists
    } else {
        WriteFailure::Changed {
            actual: current_token(path),
        }
    }
}

/// The one write owner: replaces or creates the regular file at `path` with
/// what `encode` returns, provided the entry is what `expected` says.
///
/// Ordinary saves share the anchored entry transaction used by conflict saves.
/// They retain the editor's file-size budget and retain displaced content when
/// an external edit races publication.
///
/// `encode` is handed the bytes read under the same anchored directory the
/// replacement then re-checks, so the bytes a caller plans from, the bytes the
/// expectation is judged against and the bytes the swap displaces are one
/// read, with no window between them. Returns the [`content_token`] of the
/// bytes now on disk. Nothing is written when the expectation fails.
pub(crate) fn write_regular_expecting(
    path: &Path,
    expected: Expect<'_>,
    encode: impl FnOnce(Option<&[u8]>) -> Result<Vec<u8>, String>,
) -> Result<String, WriteFailure> {
    write_regular_after_check(path, expected, encode, || {})
}

/// [`write_regular_expecting`] with a seam that runs once the expectation has
/// held and before the publish, so a test can be the other writer.
fn write_regular_after_check(
    path: &Path,
    expected: Expect<'_>,
    encode: impl FnOnce(Option<&[u8]>) -> Result<Vec<u8>, String>,
    after_check: impl FnOnce(),
) -> Result<String, WriteFailure> {
    let mut created = Vec::new();
    let result = write_regular_recording(path, expected, encode, after_check, &mut created);
    if result.is_err() {
        // A create that did not happen leaves the tree as it found it:
        // innermost first, `remove_dir` only (it refuses a directory that has
        // anything in it, so another creator's file is never lost), and only
        // the directories this call made, so one a concurrent creator got to
        // first is not touched.
        for directory in created.iter().rev() {
            let _ = std::fs::remove_dir(directory);
        }
    }
    result
}

fn write_regular_recording(
    path: &Path,
    expected: Expect<'_>,
    encode: impl FnOnce(Option<&[u8]>) -> Result<Vec<u8>, String>,
    after_check: impl FnOnce(),
    created: &mut Vec<std::path::PathBuf>,
) -> Result<String, WriteFailure> {
    let cap = crate::engine::budget::MAX_FILE_BYTES as usize;
    #[cfg(any(unix, windows))]
    {
        let source = read_source(path, expected, cap, created)?;
        if matches!(expected, Expect::Absent) {
            if source.mode != "missing" {
                return Err(WriteFailure::AlreadyExists);
            }
        } else if !matches!(source.mode.as_str(), "missing" | "100644" | "100755") {
            return Err(WriteFailure::Other(
                "Only regular files can be saved".into(),
            ));
        }
        let existing = source.bytes.as_deref();
        let changed = match expected {
            Expect::Bytes(want) => existing != Some(want),
            Expect::Token(want) => content_token(existing) != want,
            Expect::Any | Expect::Absent => false,
        };
        if changed {
            return Err(WriteFailure::Changed {
                actual: content_token(existing),
            });
        }
        let content = encode(existing)?;
        if content.len() > cap {
            return Err(WriteFailure::Other(format!(
                "File exceeds the {} MiB save limit",
                cap / (1024 * 1024)
            )));
        }
        let token = content_token(Some(&content));
        let next = Worktree {
            bytes: Some(content),
            mode: if source.mode == "100755" {
                "100755"
            } else {
                "100644"
            }
            .into(),
        };
        after_check();
        #[cfg(unix)]
        let saved = unix::replace_with_limit(path, &source, &next, cap);
        #[cfg(windows)]
        let saved = windows::replace_with_limit(path, &source, &next, cap as u64);
        let saved = saved.map_err(|message| settled(path, expected, message))?;
        if let Some(mut original) = saved {
            original.keep = false;
        }
        Ok(token)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (path, expected, encode, after_check, cap, created);
        Err(WriteFailure::Other(
            "Atomic file saves are unavailable on this platform".into(),
        ))
    }
}

#[cfg(all(test, unix))]
mod write_tests {
    use super::*;

    fn dir() -> (tempfile::TempDir, PathBuf) {
        let root = tempfile::TempDir::new().unwrap();
        let canonical = root.path().canonicalize().unwrap();
        (root, canonical)
    }

    #[test]
    fn create_does_not_replace_file_created_after_check() {
        let (_root, root) = dir();
        let target = root.join("new.txt");
        let planted = target.clone();
        let result = write_regular_after_check(
            &target,
            Expect::Absent,
            |_| Ok(Vec::new()),
            || std::fs::write(&planted, b"someone else's work").unwrap(),
        );
        assert_eq!(result, Err(WriteFailure::AlreadyExists));
        assert_eq!(std::fs::read(&target).unwrap(), b"someone else's work");
    }

    #[test]
    fn create_makes_a_missing_parent_and_a_zero_byte_file() {
        let (_root, root) = dir();
        let target = root.join("a/b/.gitkeep");
        let token = write_regular_expecting(&target, Expect::Absent, |_| Ok(Vec::new())).unwrap();
        assert_eq!(token, content_token(Some(b"")));
        assert_ne!(token, "absent");
        assert_eq!(std::fs::read(&target).unwrap(), b"");
    }

    /// The cleanup is one cross-platform loop over the directories the
    /// platform's pin recorded in `created`; this is its unix run (the Windows
    /// pin records through `pin_dir_chain_recording`, compiled by
    /// `cargo check --target x86_64-pc-windows-msvc` only).
    #[test]
    fn a_failed_create_removes_the_directories_it_made_and_only_those() {
        let (_root, root) = dir();
        std::fs::create_dir(root.join("kept")).unwrap();
        let target = root.join("kept/a/b/new.txt");
        let result = write_regular_expecting(&target, Expect::Absent, |_| {
            Err("encoder refused".to_string())
        });
        assert_eq!(result, Err(WriteFailure::Other("encoder refused".into())));
        assert!(!root.join("kept/a").exists(), "made by the call: removed");
        assert!(root.join("kept").is_dir(), "was there before: kept");

        // A directory another creator already filled is not this call's to
        // remove, and its file is never lost.
        std::fs::create_dir_all(root.join("kept/d")).unwrap();
        std::fs::write(root.join("kept/d/theirs.txt"), b"x").unwrap();
        let result = write_regular_expecting(&root.join("kept/d/n.txt"), Expect::Absent, |_| {
            Err("encoder refused".to_string())
        });
        assert!(result.is_err());
        assert_eq!(std::fs::read(root.join("kept/d/theirs.txt")).unwrap(), b"x");
    }

    #[test]
    fn create_refuses_a_file_a_directory_and_a_link_without_touching_them() {
        let (_root, root) = dir();
        std::fs::write(root.join("f"), b"keep").unwrap();
        std::fs::create_dir(root.join("d")).unwrap();
        std::os::unix::fs::symlink(root.join("f"), root.join("l")).unwrap();
        for name in ["f", "d", "l"] {
            let result =
                write_regular_expecting(&root.join(name), Expect::Absent, |_| Ok(Vec::new()));
            assert_eq!(result, Err(WriteFailure::AlreadyExists), "{name}");
        }
        assert_eq!(std::fs::read(root.join("f")).unwrap(), b"keep");
        assert!(root.join("d").is_dir());
        assert!(root.join("l").is_symlink());
    }

    #[test]
    fn a_token_save_is_refused_when_the_bytes_moved_and_succeeds_when_they_did_not() {
        let (_root, root) = dir();
        let target = root.join("t.txt");
        std::fs::write(&target, b"one\n").unwrap();
        let base = content_token(Some(b"one\n"));
        std::fs::write(&target, b"two\n").unwrap();
        let refused =
            write_regular_expecting(&target, Expect::Token(&base), |_| Ok(b"mine\n".to_vec()));
        assert_eq!(
            refused,
            Err(WriteFailure::Changed {
                actual: content_token(Some(b"two\n"))
            })
        );
        assert_eq!(std::fs::read(&target).unwrap(), b"two\n");
        let current = content_token(Some(b"two\n"));
        let saved =
            write_regular_expecting(&target, Expect::Token(&current), |_| Ok(b"mine\n".to_vec()))
                .unwrap();
        assert_eq!(saved, content_token(Some(b"mine\n")));
        assert_eq!(std::fs::read(&target).unwrap(), b"mine\n");
    }

    #[test]
    fn a_save_that_loses_the_race_is_a_typed_conflict_and_keeps_the_other_writers_bytes() {
        let (_root, root) = dir();
        let target = root.join("t.txt");
        std::fs::write(&target, b"one\n").unwrap();
        let base = content_token(Some(b"one\n"));
        let rival = target.clone();
        let result = write_regular_after_check(
            &target,
            Expect::Token(&base),
            |_| Ok(b"mine\n".to_vec()),
            || std::fs::write(&rival, b"theirs, longer\n").unwrap(),
        );
        assert_eq!(
            result,
            Err(WriteFailure::Changed {
                actual: content_token(Some(b"theirs, longer\n"))
            })
        );
        assert_eq!(std::fs::read(&target).unwrap(), b"theirs, longer\n");
    }

    #[test]
    fn the_token_tells_apart_every_single_byte_edit_swap_and_length() {
        // Sizes straddle the 32-byte block and the tail, so each path of the
        // digest is exercised; a base is only worth sending if one changed
        // byte, a swapped pair or a changed length always changes it.
        for size in [0usize, 1, 7, 31, 32, 33, 64, 100, 4096] {
            let base: Vec<u8> = (0..size).map(|i| (i * 31 % 251) as u8).collect();
            let token = content_token(Some(&base));
            assert_eq!(
                token,
                content_token(Some(&base.clone())),
                "stable at {size}"
            );
            for at in 0..size {
                let mut flipped = base.clone();
                flipped[at] ^= 1;
                assert_ne!(token, content_token(Some(&flipped)), "flip {at} of {size}");
            }
            let mut longer = base.clone();
            longer.push(0);
            assert_ne!(token, content_token(Some(&longer)), "length at {size}");
        }
        let (a, b) = (b"AAAAAAAABBBBBBBB".to_vec(), b"BBBBBBBBAAAAAAAA".to_vec());
        assert_ne!(
            content_token(Some(&a)),
            content_token(Some(&b)),
            "swapped words"
        );
        let (a, b) = (
            [&[0u8; 32][..], &[1u8; 32][..]].concat(),
            [&[1u8; 32][..], &[0u8; 32][..]].concat(),
        );
        assert_ne!(
            content_token(Some(&a)),
            content_token(Some(&b)),
            "swapped blocks"
        );
    }

    /// The digest's lane step and seeds as they were when the lanes were
    /// independent: the model a same-lane compensating edit is built on.
    const SEEDS: [u64; 4] = [
        0xcbf2_9ce4_8422_2325,
        0x8422_2325_cbf2_9ce4,
        0x1000_0000_01b3_0001,
        0x0001_0000_0000_01b3,
    ];
    fn step(lane: u64, word: u64) -> u64 {
        (lane ^ word)
            .wrapping_mul(0x9E37_79B9_7F4A_7C15)
            .rotate_left(29)
    }

    #[test]
    fn a_compensating_edit_to_one_lane_at_two_offsets_changes_the_token() {
        // 4 blocks of 32 bytes. For every lane and every pair of blocks, edit
        // that lane's word in the earlier block and solve the later block's
        // word in the same lane so that, were the lanes independent, the lane
        // would return to its original state. The token must still differ.
        for (lane, seed) in SEEDS.iter().copied().enumerate() {
            for first in 0..4 {
                for second in first + 1..4 {
                    for edit in [1u64, 0xdead_beef_cafe_f00d, u64::MAX] {
                        let base = vec![0u8; 128];
                        let (mut u, mut v) = (step(seed, edit), step(seed, 0));
                        for _ in first + 1..second {
                            u = step(u, 0);
                            v = step(v, 0);
                        }
                        let mut changed = base.clone();
                        let at = |block: usize| block * 32 + lane * 8;
                        changed[at(first)..at(first) + 8].copy_from_slice(&edit.to_le_bytes());
                        changed[at(second)..at(second) + 8].copy_from_slice(&(u ^ v).to_le_bytes());
                        assert_ne!(
                            content_token(Some(&base)),
                            content_token(Some(&changed)),
                            "lane {lane} blocks {first}/{second} edit {edit:x}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn edits_that_cross_lanes_never_cancel() {
        // Two words, anywhere in 16 positions, each set to a spread of values:
        // every pair of distinct two-word edits of a zero file gets its own
        // token, so no pair of positions cancels another.
        let mut seen = std::collections::HashMap::new();
        let values = [1u64, 2, 0xff, 0x0100, 0x8000_0000_0000_0000, u64::MAX];
        for p in 0..16usize {
            for q in p + 1..16 {
                for x in values {
                    for y in values {
                        let mut bytes = vec![0u8; 128];
                        bytes[p * 8..p * 8 + 8].copy_from_slice(&x.to_le_bytes());
                        bytes[q * 8..q * 8 + 8].copy_from_slice(&y.to_le_bytes());
                        let token = content_token(Some(&bytes));
                        assert!(
                            seen.insert(token, (p, q, x, y)).is_none(),
                            "{p} {q} {x:x} {y:x}"
                        );
                    }
                }
            }
        }
        assert!(!seen.contains_key(&content_token(Some(&[0u8; 128]))));
    }

    #[test]
    fn every_two_byte_tail_has_its_own_token() {
        // 32 bytes of block, then a 2-byte tail: all 65536 tails distinct, so
        // no pair of tail bytes cancels another.
        let mut seen = std::collections::HashSet::new();
        for a in 0..=255u8 {
            for b in 0..=255u8 {
                let mut bytes = vec![7u8; 32];
                bytes.extend([a, b]);
                assert!(seen.insert(content_token(Some(&bytes))), "tail {a} {b}");
            }
        }
    }

    #[test]
    fn an_absent_token_means_the_file_was_not_there() {
        assert_eq!(content_token(None), "absent");
        assert_ne!(content_token(Some(b"")), "absent");
        let (_root, root) = dir();
        let target = root.join("gone.txt");
        let refused = write_regular_expecting(&target, Expect::Token("1:00"), |_| Ok(Vec::new()));
        assert_eq!(
            refused,
            Err(WriteFailure::Changed {
                actual: "absent".into()
            })
        );
        assert!(!target.exists());
    }
}

#[cfg(all(test, unix))]
#[path = "adversarial_tests.rs"]
mod adversarial_tests;
