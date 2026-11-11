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
//! is deleted. It is also per file: a file with another hard link frees nothing
//! when one name is removed yet reports its whole size as private, so callers
//! must not sum it ([`FileFacts::nlink`]). Where the volume cannot answer, the
//! answer is `None`, and the caller must say the size is logical rather than
//! call it reclaimable. Whether a volume can hold clones at all is
//! [`volume_clones`].

use super::FileFacts;
use std::path::Path;

/// `VOL_CAP_INT_CLONE` in the `VOL_CAPABILITIES_INTERFACES` set: "When set, the
/// volume supports clones" (`sys/attr.h`). `libc` does not export it.
const VOL_CAP_INT_CLONE: u32 = 0x0001_0000;
/// `VREG` in `enum vtype` (`sys/vnode.h`): `VNON` is 0, `VREG` is 1.
const VREG: u32 = 1;

/// Whether a volume can hold clones, from `ATTR_VOL_CAPABILITIES`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum VolumeClones {
    /// The volume supports clones: a file's private size can be smaller than
    /// its length.
    Yes,
    /// The volume reports it does not (HFS+, exFAT, FAT): a file's logical
    /// length is exactly what deleting it frees, hard links aside.
    No,
    /// The volume did not say. Treated as able to hold clones.
    Unknown,
}

/// The decision [`volume_clones`] makes from the interfaces capability words:
/// `capabilities` holds the supported bits and `valid` which of them the
/// volume actually answered. A bit the volume did not answer is not a "no".
pub(super) fn clones_from_capabilities(capabilities: u32, valid: u32) -> VolumeClones {
    if valid & VOL_CAP_INT_CLONE == 0 {
        VolumeClones::Unknown
    } else if capabilities & VOL_CAP_INT_CLONE != 0 {
        VolumeClones::Yes
    } else {
        VolumeClones::No
    }
}

/// Whether the volume holding `path` supports clones. A failed call is
/// [`VolumeClones::Unknown`], never "no".
pub(super) fn volume_clones(path: &Path) -> VolumeClones {
    use std::os::unix::ffi::OsStrExt;

    #[repr(C, packed(4))]
    struct Reply {
        length: u32,
        capabilities: libc::vol_capabilities_attr_t,
    }

    let Ok(c_path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return VolumeClones::Unknown;
    };
    let mut request = libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: 0,
        volattr: libc::ATTR_VOL_INFO | libc::ATTR_VOL_CAPABILITIES,
        dirattr: 0,
        fileattr: 0,
        forkattr: 0,
    };
    let mut reply = Reply {
        length: 0,
        capabilities: libc::vol_capabilities_attr_t {
            capabilities: [0; 4],
            valid: [0; 4],
        },
    };
    // SAFETY: `c_path` is a NUL-terminated string; `request` is a fully
    // initialised `attrlist`; `reply` is a writable buffer of exactly the size
    // passed.
    let status = unsafe {
        libc::getattrlist(
            c_path.as_ptr(),
            std::ptr::addr_of_mut!(request).cast(),
            std::ptr::addr_of_mut!(reply).cast(),
            std::mem::size_of::<Reply>(),
            0,
        )
    };
    let length = reply.length;
    if status != 0 || (length as usize) < std::mem::size_of::<Reply>() {
        return VolumeClones::Unknown;
    }
    let capabilities = reply.capabilities.capabilities;
    let valid = reply.capabilities.valid;
    clones_from_capabilities(
        capabilities[libc::VOL_CAPABILITIES_INTERFACES],
        valid[libc::VOL_CAPABILITIES_INTERFACES],
    )
}

/// `attrBuf` layout for [`file_facts`]. The kernel packs attributes in the
/// order `man getattrlist` describes them (common, then file, then the common
/// extended group) with 4-byte alignment, which `packed(4)` reproduces. It
/// leaves out an attribute the filesystem did not return, so the reply is only
/// read at these offsets when `returned` says all but the last were returned.
#[repr(C, packed(4))]
struct FactsReply {
    length: u32,
    returned: libc::attribute_set_t,
    dev: u32,
    obj_type: u32,
    file_id: u64,
    link_count: u32,
    data_length: libc::off_t,
    private_size: libc::off_t,
}

/// One `getattrlist` for the regular file at `path`: `Ok(Some)` with its
/// facts, `Ok(None)` when the call succeeded but it is not a regular file or the
/// reply is not the shape this reads (the caller falls back to `stat`), `Err`
/// when the call failed, so the caller can classify the failure as it does a
/// failed `stat`. Never follows a symlink.
pub(super) fn file_facts(path: &Path) -> std::io::Result<Option<FileFacts>> {
    use std::os::unix::ffi::OsStrExt;

    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    let mut request = libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: libc::ATTR_CMN_RETURNED_ATTRS
            | libc::ATTR_CMN_DEVID
            | libc::ATTR_CMN_OBJTYPE
            | libc::ATTR_CMN_FILEID,
        volattr: 0,
        dirattr: 0,
        fileattr: libc::ATTR_FILE_LINKCOUNT | libc::ATTR_FILE_DATALENGTH,
        forkattr: libc::ATTR_CMNEXT_PRIVATESIZE,
    };
    let mut reply: FactsReply = FactsReply {
        length: 0,
        returned: libc::attribute_set_t {
            commonattr: 0,
            volattr: 0,
            dirattr: 0,
            fileattr: 0,
            forkattr: 0,
        },
        dev: 0,
        obj_type: 0,
        file_id: 0,
        link_count: 0,
        data_length: 0,
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
            std::mem::size_of::<FactsReply>(),
            libc::FSOPT_ATTR_CMN_EXTENDED | libc::FSOPT_NOFOLLOW,
        )
    };
    if status != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // Copies out of a packed struct: no reference to a field is taken.
    let length = reply.length as usize;
    let returned_common = reply.returned.commonattr;
    let returned_file = reply.returned.fileattr;
    let returned_fork = reply.returned.forkattr;
    let need_common = libc::ATTR_CMN_DEVID | libc::ATTR_CMN_OBJTYPE | libc::ATTR_CMN_FILEID;
    let need_file = libc::ATTR_FILE_LINKCOUNT | libc::ATTR_FILE_DATALENGTH;
    let without_private = std::mem::offset_of!(FactsReply, private_size);
    let obj_type = reply.obj_type;
    if returned_common & need_common != need_common
        || returned_file & need_file != need_file
        || length < without_private
        || obj_type != VREG
    {
        return Ok(None);
    }
    let private = if returned_fork & libc::ATTR_CMNEXT_PRIVATESIZE != 0
        && length >= std::mem::size_of::<FactsReply>()
    {
        u64::try_from(reply.private_size).ok()
    } else {
        None
    };
    let (Ok(len), dev, ino, nlink) = (
        u64::try_from(reply.data_length),
        u64::from(reply.dev),
        reply.file_id,
        u64::from(reply.link_count),
    ) else {
        return Ok(None);
    };
    Ok(Some(FileFacts {
        dev,
        ino,
        nlink,
        len,
        private,
    }))
}

/// Clone-private bytes of the regular file at `path` alone, or `None` when the
/// filesystem does not report them. The single-attribute call a walk made
/// beside its own `stat` before [`file_facts`] folded the two together. Kept
/// for tests, which compare the two and time the old shape against the new.
#[cfg(test)]
pub(crate) fn private_size(path: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;

    #[repr(C, packed(4))]
    struct Reply {
        length: u32,
        returned: libc::attribute_set_t,
        private_size: libc::off_t,
    }

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
    let mut reply = Reply {
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
    // SAFETY: as in [`file_facts`].
    let status = unsafe {
        libc::getattrlist(
            c_path.as_ptr(),
            std::ptr::addr_of_mut!(request).cast(),
            std::ptr::addr_of_mut!(reply).cast(),
            std::mem::size_of::<Reply>(),
            libc::FSOPT_ATTR_CMN_EXTENDED | libc::FSOPT_NOFOLLOW,
        )
    };
    if status != 0 {
        return None;
    }
    let length = reply.length;
    let returned = reply.returned.forkattr;
    let private = reply.private_size;
    if (length as usize) < std::mem::size_of::<Reply>()
        || returned & libc::ATTR_CMNEXT_PRIVATESIZE == 0
    {
        return None;
    }
    u64::try_from(private).ok()
}

/// Measurement support for tests outside this module: `(private, logical,
/// files, unread)` over every regular file under `root`, skipping `root/.git`.
#[cfg(test)]
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Seek, SeekFrom, Write};
    use std::os::unix::fs::MetadataExt;

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
        assert!(file_facts(&dir.path().join("missing")).is_err());
    }

    #[test]
    fn one_call_returns_what_stat_does_and_the_private_size() {
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("f");
        std::fs::write(&file, payload()).unwrap();
        std::fs::File::open(&file).unwrap().sync_all().unwrap();
        std::fs::hard_link(&file, dir.path().join("g")).unwrap();

        let facts = file_facts(&file).unwrap().expect("a regular file");
        let meta = std::fs::symlink_metadata(&file).unwrap();
        assert_eq!(facts.len, meta.len());
        assert_eq!(facts.nlink, meta.nlink());
        assert_eq!(facts.nlink, 2);
        assert_eq!(facts.ino, meta.ino());
        assert_eq!(facts.dev, u64::from(meta.dev() as u32));
        assert_eq!(facts.private, private_size(&file));
        assert!(facts.private.is_some_and(|p| p >= MIB as u64 / 2));
    }

    #[test]
    fn a_directory_or_a_symlink_is_not_a_regular_file_to_the_one_call() {
        let dir = tempfile::TempDir::new().unwrap();
        assert_eq!(file_facts(dir.path()).unwrap(), None);
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(dir.path(), &link).unwrap();
        assert_eq!(file_facts(&link).unwrap(), None);
    }

    /// Measurement, not a check. Visits every regular file under
    /// `GITPULSE_WALK_MEASURE_ROOT` (read only) `GITPULSE_WALK_MEASURE_N`
    /// (default 7) times per arm, arms interleaved and rotated, and prints the
    /// per-arm min / median / max in milliseconds with the load average:
    /// `stat` alone (the walk before clone sizing), `stat` plus a separate
    /// `getattrlist` (the walk as first committed), and the one `getattrlist`
    /// that returns both.
    #[test]
    #[ignore = "measurement: set GITPULSE_WALK_MEASURE_ROOT to a tree to read"]
    fn walk_cost_per_file_by_metadata_strategy() {
        let root = std::path::PathBuf::from(
            std::env::var("GITPULSE_WALK_MEASURE_ROOT").expect("tree to read"),
        );
        let runs: usize = std::env::var("GITPULSE_WALK_MEASURE_N")
            .ok()
            .and_then(|n| n.parse().ok())
            .unwrap_or(7);
        let mut files = Vec::new();
        let mut stack = vec![root];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).expect("read_dir").flatten() {
                match entry.file_type() {
                    Ok(kind) if kind.is_dir() => stack.push(entry.path()),
                    Ok(kind) if kind.is_file() => files.push(entry.path()),
                    _ => {}
                }
            }
        }
        type Arm = (&'static str, fn(&Path) -> u64);
        let arms: [Arm; 3] = [
            ("stat", |p| std::fs::metadata(p).map_or(0, |m| m.len())),
            ("stat+getattrlist", |p| {
                let len = std::fs::metadata(p).map_or(0, |m| m.len());
                len + private_size(p).unwrap_or(0)
            }),
            ("one getattrlist", |p| {
                file_facts(p)
                    .ok()
                    .flatten()
                    .map_or(0, |f| f.len + f.private.unwrap_or(0))
            }),
        ];
        let mut times: Vec<Vec<f64>> = vec![Vec::new(); arms.len()];
        let mut load = [0f64; 3];
        for run in 0..runs {
            for k in 0..arms.len() {
                let index = (run + k) % arms.len();
                let started = std::time::Instant::now();
                let sum: u64 = files.iter().map(|p| (arms[index].1)(p)).sum();
                times[index].push(started.elapsed().as_secs_f64() * 1000.0);
                std::hint::black_box(sum);
            }
        }
        // SAFETY: `getloadavg` writes at most the 3 doubles it is given.
        unsafe { libc::getloadavg(load.as_mut_ptr(), 3) };
        for (index, (name, _)) in arms.iter().enumerate() {
            let mut t = times[index].clone();
            t.sort_by(f64::total_cmp);
            println!(
                "MEASURE arm={name} files={} n={} min_ms={:.1} median_ms={:.1} max_ms={:.1} load1={:.1}",
                files.len(),
                t.len(),
                t[0],
                t[t.len() / 2],
                t[t.len() - 1],
                load[0]
            );
        }
    }

    #[test]
    fn the_temp_volume_is_apfs_and_holds_clones() {
        let dir = tempfile::TempDir::new().unwrap();
        assert_eq!(volume_clones(dir.path()), VolumeClones::Yes);
        assert_eq!(
            volume_clones(&dir.path().join("missing")),
            VolumeClones::Unknown
        );
    }

    #[test]
    fn a_volume_that_did_not_answer_the_clone_bit_is_not_a_volume_without_clones() {
        assert_eq!(
            clones_from_capabilities(VOL_CAP_INT_CLONE, VOL_CAP_INT_CLONE),
            VolumeClones::Yes
        );
        assert_eq!(
            clones_from_capabilities(0, VOL_CAP_INT_CLONE),
            VolumeClones::No
        );
        assert_eq!(clones_from_capabilities(0, 0), VolumeClones::Unknown);
        assert_eq!(
            clones_from_capabilities(VOL_CAP_INT_CLONE, 0),
            VolumeClones::Unknown
        );
    }
}
