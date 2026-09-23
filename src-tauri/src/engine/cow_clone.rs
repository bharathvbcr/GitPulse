//! Copy-on-Write directory and build-cache cloning.
//!
//! AI coding agents parallelize across linked worktrees, but duplicating
//! multi-gigabyte build artifacts (`node_modules`, `target`, `.venv`) burns
//! disk space and stalls launches on repeated dependency installations.
//! This module provides cross-platform Copy-on-Write directory cloning:
//! - macOS: APFS `copyfile(3)` with `COPYFILE_CLONE | COPYFILE_RECURSIVE`.
//! - Linux: `ioctl(FICLONE)` on files across directory hierarchy (btrfs, XFS).
//! - Windows / Fallback: Bounded recursive copy when CoW is unsupported.

use serde::{Deserialize, Serialize};
use std::ffi::CString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// Bounds on directory traversal to prevent pathological runaway.
pub const MAX_COW_DEPTH: usize = 12;
pub const MAX_COW_ENTRIES: usize = 100_000;

/// Standard ignored build-cache directory names candidate for CoW cloning.
pub const STANDARD_CACHE_DIRS: &[&str] = &[
    "node_modules",
    "target",
    ".venv",
    "venv",
    "vendor",
    ".turbo",
    "build",
    "dist",
];

/// Outcome of a CoW directory cloning operation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReflinkResult {
    pub source: String,
    pub destination: String,
    pub files_cloned: usize,
    pub bytes_cloned: u64,
    pub is_cow: bool,
    pub duration_ms: u64,
}

#[cfg(target_os = "macos")]
extern "C" {
    fn copyfile(
        from: *const libc::c_char,
        to: *const libc::c_char,
        state: *mut libc::c_void,
        flags: u32,
    ) -> libc::c_int;
}

#[cfg(target_os = "macos")]
const COPYFILE_DATA: u32 = 1 << 3;
#[cfg(target_os = "macos")]
const COPYFILE_METADATA: u32 = 1 << 1;
#[cfg(target_os = "macos")]
const COPYFILE_ALL: u32 = COPYFILE_METADATA | COPYFILE_DATA;
#[cfg(target_os = "macos")]
const COPYFILE_RECURSIVE: u32 = 1 << 15;
#[cfg(target_os = "macos")]
const COPYFILE_CLONE: u32 = 1 << 24;

/// Clones a directory tree from `src` to `dst`, prioritizing Copy-on-Write.
pub fn reflink_copy_dir(src: &Path, dst: &Path) -> Result<ReflinkResult, String> {
    if !src.exists() {
        return Err(format!("Source path '{}' does not exist", src.display()));
    }
    if !src.is_dir() {
        return Err(format!(
            "Source path '{}' is not a directory",
            src.display()
        ));
    }
    if dst.exists() {
        return Err(format!(
            "Destination path '{}' already exists",
            dst.display()
        ));
    }

    let start = Instant::now();

    #[cfg(target_os = "macos")]
    {
        let src_c = CString::new(src.to_string_lossy().as_bytes())
            .map_err(|e| format!("Invalid source path: {e}"))?;
        let dst_c = CString::new(dst.to_string_lossy().as_bytes())
            .map_err(|e| format!("Invalid destination path: {e}"))?;

        // Try APFS clone first: COPYFILE_ALL | COPYFILE_RECURSIVE | COPYFILE_CLONE
        let flags = COPYFILE_ALL | COPYFILE_RECURSIVE | COPYFILE_CLONE;
        let ret = unsafe { copyfile(src_c.as_ptr(), dst_c.as_ptr(), std::ptr::null_mut(), flags) };
        if ret == 0 {
            let (files, bytes) = count_dir_stats(dst, 0)?;
            return Ok(ReflinkResult {
                source: src.display().to_string(),
                destination: dst.display().to_string(),
                files_cloned: files,
                bytes_cloned: bytes,
                is_cow: true,
                duration_ms: start.elapsed().as_millis() as u64,
            });
        }
        // If APFS clone failed (e.g. cross-volume or permission), clean any partial dst
        let _ = fs::remove_dir_all(dst);
    }

    // Fallback to recursive copy with per-file CoW on Linux or regular copy
    fallback_reflink_copy(src, dst, start)
}

/// Fallback recursive cloner that attempts file-level FICLONE on Linux, or fs::copy elsewhere.
fn fallback_reflink_copy(src: &Path, dst: &Path, start: Instant) -> Result<ReflinkResult, String> {
    fs::create_dir_all(dst).map_err(|e| format!("Failed to create destination dir: {e}"))?;

    let mut files_cloned = 0;
    let mut bytes_cloned = 0;
    let mut is_cow = false;

    let mut stack: Vec<(PathBuf, PathBuf, usize)> = vec![(src.to_path_buf(), dst.to_path_buf(), 0)];

    while let Some((curr_src, curr_dst, depth)) = stack.pop() {
        if depth > MAX_COW_DEPTH {
            continue;
        }
        let read_dir = match fs::read_dir(&curr_src) {
            Ok(rd) => rd,
            Err(_) => continue,
        };

        for entry in read_dir.flatten() {
            if files_cloned >= MAX_COW_ENTRIES {
                break;
            }
            let entry_path = entry.path();
            let file_name = entry.file_name();
            let target_path = curr_dst.join(&file_name);

            let Ok(meta) = entry.metadata() else {
                continue;
            };

            if meta.is_dir() {
                if fs::create_dir_all(&target_path).is_ok() {
                    stack.push((entry_path, target_path, depth + 1));
                }
            } else if meta.is_file() {
                #[cfg(target_os = "linux")]
                let cloned = try_linux_ficlone(&entry_path, &target_path).unwrap_or(false);
                #[cfg(not(target_os = "linux"))]
                let cloned = false;

                if cloned {
                    is_cow = true;
                    files_cloned += 1;
                    bytes_cloned += meta.len();
                } else if fs::copy(&entry_path, &target_path).is_ok() {
                    files_cloned += 1;
                    bytes_cloned += meta.len();
                }
            } else if meta.is_symlink() {
                #[cfg(unix)]
                if let Ok(link_target) = fs::read_link(&entry_path) {
                    let _ = std::os::unix::fs::symlink(link_target, &target_path);
                    files_cloned += 1;
                }
            }
        }
    }

    Ok(ReflinkResult {
        source: src.display().to_string(),
        destination: dst.display().to_string(),
        files_cloned,
        bytes_cloned,
        is_cow,
        duration_ms: start.elapsed().as_millis() as u64,
    })
}

#[cfg(target_os = "linux")]
fn try_linux_ficlone(src: &Path, dst: &Path) -> Result<bool, ()> {
    use std::fs::OpenOptions;
    use std::os::unix::io::AsRawFd;

    const FICLONE: libc::c_ulong = 0x40049409;

    let src_file = OpenOptions::new().read(true).open(src).map_err(|_| ())?;
    let dst_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dst)
        .map_err(|_| ())?;

    let res = unsafe { libc::ioctl(dst_file.as_raw_fd(), FICLONE, src_file.as_raw_fd()) };
    if res == 0 {
        Ok(true)
    } else {
        let _ = fs::remove_file(dst);
        Ok(false)
    }
}

/// Recursively counts files and byte length of a directory up to bounds.
fn count_dir_stats(dir: &Path, depth: usize) -> Result<(usize, u64), String> {
    if depth > MAX_COW_DEPTH {
        return Ok((0, 0));
    }
    let mut files = 0;
    let mut bytes = 0;
    let read_dir = fs::read_dir(dir).map_err(|e| format!("Cannot read {}: {e}", dir.display()))?;

    for entry in read_dir.flatten() {
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if meta.is_dir() {
            let (sub_files, sub_bytes) = count_dir_stats(&entry.path(), depth + 1)?;
            files += sub_files;
            bytes += sub_bytes;
        } else if meta.is_file() {
            files += 1;
            bytes += meta.len();
        }
    }
    Ok((files, bytes))
}

/// Scans standard build-cache directories in the anchor repository and
/// Copy-on-Write clones any that are missing in the target worktree.
pub fn reflink_ignored_caches(
    anchor: &Path,
    worktree: &Path,
) -> Result<Vec<ReflinkResult>, String> {
    if !anchor.exists() || !worktree.exists() {
        return Err("Both anchor and worktree paths must exist".to_string());
    }
    if anchor == worktree {
        return Ok(Vec::new());
    }

    let mut results = Vec::new();

    for dir_name in STANDARD_CACHE_DIRS {
        let anchor_dir = anchor.join(dir_name);
        let worktree_dir = worktree.join(dir_name);

        if anchor_dir.is_dir() && !worktree_dir.exists() {
            match reflink_copy_dir(&anchor_dir, &worktree_dir) {
                Ok(res) => results.push(res),
                Err(err) => {
                    log::warn!(
                        "Could not CoW-clone cache '{}' to '{}': {}",
                        anchor_dir.display(),
                        worktree_dir.display(),
                        err
                    );
                }
            }
        }
    }

    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;
    use tempfile::tempdir;

    #[test]
    fn test_reflink_copy_roundtrip() {
        let temp = tempdir().unwrap();
        let src = temp.path().join("source_cache");
        let dst = temp.path().join("dest_cache");

        fs::create_dir_all(src.join("sub")).unwrap();
        let mut file1 = File::create(src.join("file1.txt")).unwrap();
        writeln!(file1, "Hello CoW!").unwrap();
        let mut file2 = File::create(src.join("sub/file2.txt")).unwrap();
        writeln!(file2, "Nested CoW!").unwrap();

        let result = reflink_copy_dir(&src, &dst).expect("reflink_copy_dir failed");
        assert_eq!(result.files_cloned, 2);
        assert!(dst.join("file1.txt").exists());
        assert!(dst.join("sub/file2.txt").exists());
        let content = fs::read_to_string(dst.join("file1.txt")).unwrap();
        assert_eq!(content.trim(), "Hello CoW!");
    }

    #[test]
    fn test_reflink_ignored_caches_selective() {
        let temp = tempdir().unwrap();
        let anchor = temp.path().join("anchor");
        let worktree = temp.path().join("worktree");

        fs::create_dir_all(anchor.join("node_modules/pkg")).unwrap();
        let mut pkg_f = File::create(anchor.join("node_modules/pkg/index.js")).unwrap();
        writeln!(pkg_f, "module.exports = true;").unwrap();

        fs::create_dir_all(&worktree).unwrap();

        let results = reflink_ignored_caches(&anchor, &worktree).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].files_cloned, 1);
        assert!(worktree.join("node_modules/pkg/index.js").exists());

        // Second call should not overwrite existing
        let second = reflink_ignored_caches(&anchor, &worktree).unwrap();
        assert_eq!(second.len(), 0);
    }
}
