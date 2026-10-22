//! Free space on the volume a path lives on, and what a launch is told when
//! there is not enough of it.
//!
//! Low disk is advice, never a refusal (owner decision, 2026-10-21): it can
//! skip the optional build-cache copy of a worktree, which only saves a
//! rebuild, and it can never stop a launch, a checkout or a worktree being
//! made. [`governor::Site`](super::Site) has no disk site and
//! `Reason`/`Refused` have no disk variant; `tests::low_disk_is_advice_only`
//! holds that.

use super::sensor::{Reading, Why};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Free bytes a cache copy must leave on the volume. Below it (or when a
/// copy would take the volume below it) the caches are skipped.
pub const LOW_DISK_FLOOR: u64 = 10 << 30;

/// A free-space reader: bytes available to an unprivileged process on the
/// volume holding the path. A function pointer so a selection of it stays
/// `Copy`, and a test injects a fixed value.
pub type FreeSpace = fn(&Path) -> Reading<u64>;

/// The real reader. An unreadable volume is `Unavailable`, never a number.
#[cfg(unix)]
pub fn free_bytes(path: &Path) -> Reading<u64> {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c_path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return Reading::Unavailable(Why::Failed);
    };
    let mut fs = std::mem::MaybeUninit::<libc::statvfs>::zeroed();
    // SAFETY: a NUL-terminated path and one statvfs buffer.
    if unsafe { libc::statvfs(c_path.as_ptr(), fs.as_mut_ptr()) } != 0 {
        return Reading::Unavailable(Why::of_errno(
            std::io::Error::last_os_error().raw_os_error(),
        ));
    }
    // SAFETY: filled by the kernel.
    let fs = unsafe { fs.assume_init() };
    #[allow(clippy::useless_conversion)]
    let (avail, frsize, bsize) = (
        u64::from(fs.f_bavail),
        u64::from(fs.f_frsize),
        u64::from(fs.f_bsize),
    );
    Reading::Value(available_bytes(avail, frsize, bsize))
}

/// Blocks available times the fragment size, or the block size where the
/// filesystem reports no fragment size (`f_frsize == 0`): a zero there made
/// every volume read as having no space.
fn available_bytes(avail_blocks: u64, frsize: u64, bsize: u64) -> u64 {
    let unit = if frsize == 0 { bsize } else { frsize };
    avail_blocks.saturating_mul(unit)
}

/// Windows has no reader here: unknown, so nothing is skipped on a guess.
#[cfg(not(unix))]
pub fn free_bytes(_path: &Path) -> Reading<u64> {
    Reading::Unavailable(Why::Unsupported)
}

/// What bound the cache copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LowDiskCause {
    /// Free space was below [`LOW_DISK_FLOOR`], or a copy would have taken it
    /// below.
    BelowFloor,
    /// A copy would have written more than the byte cap.
    ByteCap,
}

/// Why caches were not copied for want of disk. Advice: the worktree itself
/// was made.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LowDisk {
    pub cause: LowDiskCause,
    /// Free bytes read on the worktree's volume, when it could be read.
    pub free_bytes: Option<u64>,
    pub floor_bytes: u64,
    /// The cap a copy ran under, named when it is what bound.
    pub cap_bytes: Option<u64>,
    /// The cache directories that were not copied.
    pub skipped: Vec<String>,
}

impl LowDisk {
    pub fn message(&self) -> String {
        let what = self.skipped.join(", ");
        match self.cause {
            LowDiskCause::BelowFloor => format!(
                "low disk: {} free, below the {} the build caches must leave, so {what} was not copied into the new worktree",
                self.free_bytes.map_or_else(|| "unknown".to_string(), gib),
                gib(self.floor_bytes),
            ),
            LowDiskCause::ByteCap => format!(
                "low disk: copying {what} would write more than the {} byte cap, so it was not copied into the new worktree",
                self.cap_bytes.map_or_else(|| "unknown".to_string(), gib),
            ),
        }
    }
}

fn gib(bytes: u64) -> String {
    format!("{:.1} GiB", bytes as f64 / (1u64 << 30) as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn a_real_volume_reads_a_value_and_a_missing_path_does_not() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(free_bytes(dir.path()), Reading::Value(v) if v > 0));
        assert!(matches!(
            free_bytes(&dir.path().join("absent")),
            Reading::Unavailable(_)
        ));
    }

    #[test]
    fn a_zero_fragment_size_falls_back_to_the_block_size() {
        assert_eq!(available_bytes(100, 4096, 512), 100 * 4096);
        assert_eq!(available_bytes(100, 0, 512), 100 * 512);
        assert_eq!(available_bytes(100, 0, 0), 0);
        assert_eq!(available_bytes(u64::MAX, 4096, 4096), u64::MAX);
    }

    #[test]
    fn the_message_names_the_floor_or_the_cap() {
        let below = LowDisk {
            cause: LowDiskCause::BelowFloor,
            free_bytes: Some(1 << 30),
            floor_bytes: LOW_DISK_FLOOR,
            cap_bytes: None,
            skipped: vec!["node_modules".into()],
        };
        assert!(
            below.message().contains("1.0 GiB free"),
            "{}",
            below.message()
        );
        let capped = LowDisk {
            cause: LowDiskCause::ByteCap,
            cap_bytes: Some(4 << 30),
            ..below
        };
        assert!(
            capped.message().contains("4.0 GiB byte cap"),
            "{}",
            capped.message()
        );
    }
}
