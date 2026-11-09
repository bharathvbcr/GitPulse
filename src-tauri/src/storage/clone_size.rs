//! Bytes that deleting a file would free right now, on APFS.
//!
//! `st_size` is a file's logical length. A file made by `clonefile(2)` or
//! `cp -c` shares its blocks with its source, so deleting it frees only the
//! blocks nothing else references, and the logical length of a cloned `target/`
//! says nothing about that. `getattrlist(2)` reports it directly: per
//! `man getattrlist` and `sys/attr.h`, `ATTR_CMNEXT_PRIVATESIZE` (requested
//! with `FSOPT_ATTR_CMN_EXTENDED`) is "the number of bytes that are not
//! trapped inside a clone or snapshot, and which would be freed immediately
//! if the file were deleted".
//!
//! The reading is a point in time: it changes as a sibling clone diverges or
//! is deleted. Where the volume cannot answer, the answer is `None`, and the
//! caller must say the size is logical rather than call it reclaimable.

use std::path::Path;

/// `attrBuf` layout for a request of `ATTR_CMN_RETURNED_ATTRS` followed by one
/// extended common attribute: the reply length, the set of attributes the
/// filesystem actually returned, then the attribute. Packed to 4 bytes as the
/// header does (`#pragma pack(4)`), so the `off_t` sits at offset 24.
#[cfg(target_os = "macos")]
#[repr(C, packed(4))]
struct PrivateSizeReply {
    length: u32,
    returned: libc::attribute_set_t,
    private_size: libc::off_t,
}

/// Clone-private bytes of the regular file at `path`, or `None` when the
/// filesystem does not report them (not APFS, a path it cannot open, a reply
/// that omits the attribute). Never follows a symlink.
#[cfg(target_os = "macos")]
pub(super) fn private_size(path: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;

    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut request = libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: libc::ATTR_CMN_RETURNED_ATTRS,
        volattr: 0,
        dirattr: 0,
        fileattr: 0,
        forkattr: libc::ATTR_CMNEXT_PRIVATESIZE,
    };
    let mut reply = PrivateSizeReply {
        length: 0,
        returned: libc::attribute_set_t {
            commonattr: 0,
            volattr: 0,
            dirattr: 0,
            fileattr: 0,
            forkattr: 0,
        },
        private_size: 0,
    };
    // SAFETY: `c_path` is a NUL-terminated string; `request` is a fully
    // initialised `attrlist`; `reply` is a writable buffer of exactly the size
    // passed. The kernel writes at most that many bytes.
    let status = unsafe {
        libc::getattrlist(
            c_path.as_ptr(),
            std::ptr::addr_of_mut!(request).cast(),
            std::ptr::addr_of_mut!(reply).cast(),
            std::mem::size_of::<PrivateSizeReply>(),
            libc::FSOPT_ATTR_CMN_EXTENDED | libc::FSOPT_NOFOLLOW,
        )
    };
    if status != 0 {
        return None;
    }
    // Copies out of a packed struct: no reference to a field is taken.
    let length = reply.length;
    let returned = reply.returned.forkattr;
    let private = reply.private_size;
    if (length as usize) < std::mem::size_of::<PrivateSizeReply>()
        || returned & libc::ATTR_CMNEXT_PRIVATESIZE == 0
    {
        return None;
    }
    u64::try_from(private).ok()
}

/// Measurement support for tests outside this module: `(private, logical,
/// files, unread)` over every regular file under `root`, skipping `root/.git`.
#[cfg(all(test, target_os = "macos"))]
pub(crate) fn private_tree_bytes(root: &Path) -> (u64, u64, u64, u64) {
    let (mut private, mut logical, mut files, mut unread) = (0, 0, 0, 0);
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("read_dir").flatten() {
            let path = entry.path();
            let kind = entry.file_type().expect("file_type");
            if kind.is_dir() {
                if !(dir == root && entry.file_name() == ".git") {
                    stack.push(path);
                }
            } else if kind.is_file() {
                files += 1;
                logical += entry.metadata().expect("metadata").len();
                match private_size(&path) {
                    Some(bytes) => private += bytes,
                    None => unread += 1,
                }
            }
        }
    }
    (private, logical, files, unread)
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use std::io::{Seek, SeekFrom, Write};

    const MIB: usize = 1024 * 1024;

    fn clone_file(src: &Path, dst: &Path) {
        let src = std::ffi::CString::new(src.to_str().unwrap()).unwrap();
        let dst = std::ffi::CString::new(dst.to_str().unwrap()).unwrap();
        // SAFETY: both are valid NUL-terminated strings.
        let status = unsafe { libc::clonefile(src.as_ptr(), dst.as_ptr(), 0) };
        assert_eq!(status, 0, "clonefile: {}", std::io::Error::last_os_error());
    }

    /// Incompressible-looking, non-sparse content (a zero page would be a hole).
    fn payload() -> Vec<u8> {
        (0..MIB)
            .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8 | 1)
            .collect()
    }

    #[test]
    fn a_cloned_file_is_logically_whole_but_privately_almost_nothing() {
        let dir = tempfile::TempDir::new().unwrap();
        let original = dir.path().join("original");
        std::fs::write(&original, payload()).unwrap();
        // Flush so the blocks are allocated before they are asked about.
        std::fs::File::open(&original).unwrap().sync_all().unwrap();

        let before = private_size(&original).expect("APFS reports a private size");
        assert!(
            before >= MIB as u64 / 2,
            "an unshared file owns its blocks: {before}"
        );

        let clone = dir.path().join("clone");
        clone_file(&original, &clone);
        assert_eq!(std::fs::metadata(&clone).unwrap().len(), MIB as u64);
        let (a, b) = (
            private_size(&original).unwrap(),
            private_size(&clone).unwrap(),
        );
        assert!(
            a < MIB as u64 / 8 && b < MIB as u64 / 8,
            "blocks shared by two files are private to neither: original {a}, clone {b}"
        );
    }

    #[test]
    fn a_diverged_clone_owns_only_the_blocks_it_rewrote() {
        let dir = tempfile::TempDir::new().unwrap();
        let original = dir.path().join("original");
        std::fs::write(&original, payload()).unwrap();
        std::fs::File::open(&original).unwrap().sync_all().unwrap();
        let clone = dir.path().join("clone");
        clone_file(&original, &clone);

        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .open(&clone)
            .unwrap();
        file.seek(SeekFrom::Start(0)).unwrap();
        file.write_all(&vec![0xA5; MIB / 4]).unwrap();
        file.sync_all().unwrap();

        let diverged = private_size(&clone).unwrap();
        assert!(
            diverged >= MIB as u64 / 8 && diverged < MIB as u64 * 3 / 4,
            "a clone that rewrote a quarter of the file owns about a quarter of it: {diverged}"
        );
    }

    #[test]
    fn a_path_that_does_not_exist_has_no_private_size() {
        let dir = tempfile::TempDir::new().unwrap();
        assert_eq!(private_size(&dir.path().join("missing")), None);
    }
}
