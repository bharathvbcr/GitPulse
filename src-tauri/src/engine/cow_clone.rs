//! Copy-on-Write directory and build-cache cloning.
//!
//! AI coding agents parallelize across linked worktrees, but duplicating
//! multi-gigabyte dependency trees (`node_modules`) burns disk space and
//! stalls launches on repeated installations. This module clones them:
//! - macOS: APFS `copyfile(3)` with `COPYFILE_CLONE | COPYFILE_RECURSIVE`.
//! - Linux: `ioctl(FICLONE)` on files across directory hierarchy (btrfs, XFS).
//! - Windows / Fallback: Bounded recursive copy when CoW is unsupported.
//!
//! A copy is whole or absent. It is made under a staging name beside the
//! destination and renamed into place only once every entry was copied; a
//! bound reached, an unreadable entry or a failed write removes the staging
//! tree and reports the failure. A half-copied `node_modules` used to be
//! reported as success, and because its directory then existed, never
//! repaired.

use serde::{Deserialize, Serialize};
#[cfg(target_os = "macos")]
use std::ffi::CString;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

/// Deepest directory nesting one copy accepts. pnpm and nested npm installs
/// go well past a dozen levels (`.pnpm/<pkg>/node_modules/@scope/<pkg>/...`).
pub const MAX_COW_DEPTH: usize = 64;
/// Most entries (files, directories and links) one copy accepts. A large
/// `node_modules` holds a few hundred thousand.
pub const MAX_COW_ENTRIES: usize = 1_000_000;

/// The bounds one copy runs under. A tree past either is not copied at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CowCaps {
    pub max_depth: usize,
    pub max_entries: usize,
}

impl CowCaps {
    pub const DEFAULT: CowCaps = CowCaps {
        max_depth: MAX_COW_DEPTH,
        max_entries: MAX_COW_ENTRIES,
    };
}

/// Cache directories cloned by default: dependency trees that hold no
/// absolute path of the checkout they were installed in, so a copy works in
/// another worktree. Rust `target/`, `build/`, `dist/` and `vendor/` are not
/// among them: build outputs record absolute paths (a copied `target/` is
/// rebuilt anyway), and `vendor/` is often tracked.
pub const STANDARD_CACHE_DIRS: &[&str] = &["node_modules", ".turbo"];

/// Python virtual environments. `python -m venv` writes its own absolute path
/// into `bin/activate`, `pyvenv.cfg` and every script's shebang, so a copy
/// runs the anchor's interpreter and installs into the anchor's environment.
/// Cloned only when a caller opts in.
pub const VIRTUALENV_DIRS: &[&str] = &[".venv", "venv"];

/// Which caches [`reflink_ignored_caches`] considers, and the bounds each
/// copy runs under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheSelection {
    pub include_virtualenvs: bool,
    pub caps: CowCaps,
}

impl Default for CacheSelection {
    fn default() -> Self {
        Self {
            include_virtualenvs: false,
            caps: CowCaps::DEFAULT,
        }
    }
}

/// Outcome of a CoW directory cloning operation. Only a complete copy has
/// one: a copy that stopped short is an error, and leaves nothing behind.
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

/// What one copy produced, before it is renamed into place.
struct Tally {
    files: usize,
    bytes: u64,
    is_cow: bool,
}

/// Clones a directory tree from `src` to `dst`, prioritizing Copy-on-Write,
/// under the default bounds.
pub fn reflink_copy_dir(src: &Path, dst: &Path) -> Result<ReflinkResult, String> {
    reflink_copy_dir_bounded(src, dst, CowCaps::DEFAULT)
}

/// [`reflink_copy_dir`] under `caps`.
fn reflink_copy_dir_bounded(
    src: &Path,
    dst: &Path,
    caps: CowCaps,
) -> Result<ReflinkResult, String> {
    // Refused, not respelled: a lossy spelling names a different file, and
    // the clone and its cleanup would act on that one.
    let src_text = utf8_path(src, "Source")?;
    let dst_text = utf8_path(dst, "Destination")?;
    if !src.exists() {
        return Err(format!("Source path '{src_text}' does not exist"));
    }
    if !src.is_dir() {
        return Err(format!("Source path '{src_text}' is not a directory"));
    }
    // `symlink_metadata`: a dangling link is still something in the way.
    if dst.symlink_metadata().is_ok() {
        return Err(format!("Destination path '{dst_text}' already exists"));
    }

    let start = Instant::now();
    let staging = staging_path(dst)?;
    let copied = clone_tree(src, &staging, caps).and_then(|tally| {
        fs::rename(&staging, dst)
            .map(|()| tally)
            .map_err(|e| format!("Could not move the finished copy into '{dst_text}': {e}"))
    });
    match copied {
        Ok(tally) => Ok(ReflinkResult {
            source: src_text.to_string(),
            destination: dst_text.to_string(),
            files_cloned: tally.files,
            bytes_cloned: tally.bytes,
            is_cow: tally.is_cow,
            duration_ms: start.elapsed().as_millis() as u64,
        }),
        Err(error) => Err(discard_staging(&staging, error)),
    }
}

fn utf8_path<'a>(path: &'a Path, role: &str) -> Result<&'a str, String> {
    path.to_str().ok_or_else(|| {
        format!(
            "{role} path '{}' is not valid UTF-8, so it is not copied",
            path.display()
        )
    })
}

/// A fresh name beside `dst` that nothing else in this process will pick.
fn staging_path(dst: &Path) -> Result<PathBuf, String> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let parent = dst
        .parent()
        .ok_or_else(|| format!("Destination path '{}' has no parent", dst.display()))?;
    let name = dst
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("Destination path '{}' has no name", dst.display()))?;
    let seq = NEXT.fetch_add(1, Ordering::Relaxed);
    Ok(parent.join(format!(
        ".{name}.gitpulse-partial-{}-{seq}",
        std::process::id()
    )))
}

/// Removes a staging tree after `error`, and says so when that failed too.
fn discard_staging(staging: &Path, error: String) -> String {
    match fs::remove_dir_all(staging) {
        Ok(()) => error,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => error,
        Err(e) => format!(
            "{error}; the partial copy at '{}' could not be removed: {e}",
            staging.display()
        ),
    }
}

/// Copies `src` into `staging`, which must not exist yet.
fn clone_tree(src: &Path, staging: &Path, caps: CowCaps) -> Result<Tally, String> {
    #[cfg(target_os = "macos")]
    {
        let src_c = CString::new(utf8_path(src, "Source")?)
            .map_err(|e| format!("Invalid source path: {e}"))?;
        let dst_c = CString::new(utf8_path(staging, "Destination")?)
            .map_err(|e| format!("Invalid destination path: {e}"))?;
        let flags = COPYFILE_ALL | COPYFILE_RECURSIVE | COPYFILE_CLONE;
        // SAFETY: `src_c` and `dst_c` are NUL-terminated strings that outlive
        // the call; `copyfile(3)` only reads them. A null `state` is the
        // documented way to run without a copyfile_state_t. The call touches
        // no Rust-owned memory besides those two buffers.
        let ret = unsafe { copyfile(src_c.as_ptr(), dst_c.as_ptr(), std::ptr::null_mut(), flags) };
        if ret == 0 {
            let (files, bytes) = count_dir_stats(staging, 0, caps, &mut 0)?;
            return Ok(Tally {
                files,
                bytes,
                is_cow: true,
            });
        }
        // The clone failed (cross-volume, an unreadable entry): whatever it
        // left goes before the walk starts over in the same place.
        if let Err(e) = fs::remove_dir_all(staging) {
            if e.kind() != std::io::ErrorKind::NotFound {
                return Err(format!(
                    "The clone failed and its partial copy at '{}' could not be removed: {e}",
                    staging.display()
                ));
            }
        }
    }

    fallback_reflink_copy(src, staging, caps)
}

/// Recursive copier that attempts file-level FICLONE on Linux, or `fs::copy`
/// elsewhere. Every entry is copied or the copy fails: an unreadable
/// directory or file, a failed write, an entry that is none of file,
/// directory or link, or a bound reached is an error naming it.
fn fallback_reflink_copy(src: &Path, dst: &Path, caps: CowCaps) -> Result<Tally, String> {
    fs::create_dir(dst).map_err(|e| format!("Failed to create destination dir: {e}"))?;

    let mut tally = Tally {
        files: 0,
        bytes: 0,
        is_cow: false,
    };
    let mut entries = 0usize;
    let mut stack: Vec<(PathBuf, PathBuf, usize)> = vec![(src.to_path_buf(), dst.to_path_buf(), 0)];

    while let Some((curr_src, curr_dst, depth)) = stack.pop() {
        let read_dir = fs::read_dir(&curr_src)
            .map_err(|e| format!("Cannot read '{}': {e}", curr_src.display()))?;
        for entry in read_dir {
            let entry = entry.map_err(|e| format!("Cannot list '{}': {e}", curr_src.display()))?;
            entries += 1;
            if entries > caps.max_entries {
                return Err(format!(
                    "'{}' holds more than {} entries, so it was not copied",
                    src.display(),
                    caps.max_entries
                ));
            }
            let entry_path = entry.path();
            let target_path = curr_dst.join(entry.file_name());
            // Not followed: a link is copied as a link, never walked into.
            let meta = entry
                .metadata()
                .map_err(|e| format!("Cannot read '{}': {e}", entry_path.display()))?;
            let kind = meta.file_type();

            if kind.is_dir() {
                if depth + 1 > caps.max_depth {
                    return Err(format!(
                        "'{}' is nested more than {} directories deep, so it was not copied",
                        src.display(),
                        caps.max_depth
                    ));
                }
                fs::create_dir(&target_path)
                    .map_err(|e| format!("Cannot create '{}': {e}", target_path.display()))?;
                stack.push((entry_path, target_path, depth + 1));
            } else if kind.is_file() {
                #[cfg(target_os = "linux")]
                let cloned = try_linux_ficlone(&entry_path, &target_path).unwrap_or(false);
                #[cfg(not(target_os = "linux"))]
                let cloned = false;

                if cloned {
                    tally.is_cow = true;
                } else {
                    fs::copy(&entry_path, &target_path)
                        .map_err(|e| format!("Could not copy '{}': {e}", entry_path.display()))?;
                }
                tally.files += 1;
                tally.bytes += meta.len();
            } else if kind.is_symlink() {
                copy_link(&entry_path, &target_path)?;
                tally.files += 1;
            } else {
                return Err(format!(
                    "'{}' is not a file, directory or link, so the tree was not copied",
                    entry_path.display()
                ));
            }
        }
    }

    Ok(tally)
}

#[cfg(unix)]
fn copy_link(link: &Path, target: &Path) -> Result<(), String> {
    let points_to =
        fs::read_link(link).map_err(|e| format!("Cannot read link '{}': {e}", link.display()))?;
    std::os::unix::fs::symlink(points_to, target)
        .map_err(|e| format!("Could not copy link '{}': {e}", link.display()))
}

#[cfg(not(unix))]
fn copy_link(link: &Path, _target: &Path) -> Result<(), String> {
    Err(format!(
        "'{}' is a link, which this platform's copy does not recreate, so the tree was not copied",
        link.display()
    ))
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

    // SAFETY: both descriptors come from `File`s that stay open for the whole
    // call (they are dropped at the end of this function), so neither can be
    // closed or reused underneath it. FICLONE takes the source descriptor as
    // its integer argument and writes no memory of ours.
    let res = unsafe { libc::ioctl(dst_file.as_raw_fd(), FICLONE, src_file.as_raw_fd()) };
    if res == 0 {
        Ok(true)
    } else {
        let _ = fs::remove_file(dst);
        Ok(false)
    }
}

/// Counts the files and bytes of a finished clone, under the same bounds as
/// the walk: a tree past them was not one this module accepts, so the clone
/// is discarded rather than reported with a count that stopped short.
#[cfg(target_os = "macos")]
fn count_dir_stats(
    dir: &Path,
    depth: usize,
    caps: CowCaps,
    entries: &mut usize,
) -> Result<(usize, u64), String> {
    let mut files = 0;
    let mut bytes = 0;
    let read_dir = fs::read_dir(dir).map_err(|e| format!("Cannot read {}: {e}", dir.display()))?;

    for entry in read_dir {
        let entry = entry.map_err(|e| format!("Cannot list {}: {e}", dir.display()))?;
        *entries += 1;
        if *entries > caps.max_entries {
            return Err(format!(
                "the tree holds more than {} entries, so it was not copied",
                caps.max_entries
            ));
        }
        let meta = entry
            .metadata()
            .map_err(|e| format!("Cannot read {}: {e}", entry.path().display()))?;
        if meta.is_dir() {
            if depth + 1 > caps.max_depth {
                return Err(format!(
                    "the tree is nested more than {} directories deep, so it was not copied",
                    caps.max_depth
                ));
            }
            let (sub_files, sub_bytes) = count_dir_stats(&entry.path(), depth + 1, caps, entries)?;
            files += sub_files;
            bytes += sub_bytes;
        } else if meta.is_file() {
            files += 1;
            bytes += meta.len();
        } else if meta.file_type().is_symlink() {
            files += 1;
        }
    }
    Ok((files, bytes))
}

/// Whether the worktree's own ignore rules cover `dir_name/`. Exit 0 is
/// "ignored", 1 is "not ignored"; anything else — including a spawn deferred
/// under load — is no answer, and the caller copies nothing on no answer.
fn ignored_in(worktree: &Path, dir_name: &str) -> Result<bool, String> {
    // The trailing slash: patterns like `node_modules/` match directories
    // only, and check-ignore judges the pathname exactly as given.
    let pathname = format!("{dir_name}/");
    let run = crate::engine::git_cli::git_captured(
        worktree,
        &["check-ignore", "-q", "--", pathname.as_str()],
    )?;
    match run.status_code {
        0 => Ok(true),
        1 => Ok(false),
        code => Err(format!(
            "git check-ignore could not say whether {pathname} is ignored (exit {code}): {}",
            String::from_utf8_lossy(&run.stderr).trim()
        )),
    }
}

/// Scans the cache directories in the anchor checkout and clones any the
/// worktree lacks and its git ignore rules cover.
///
/// Every candidate is attempted; any that could not be copied, or could not
/// be checked, makes the whole call an error naming each one (and naming
/// what was copied), so a caller never reads "done" over a missing cache.
/// A candidate git does not ignore is skipped, not an error: copying it
/// would put untracked files in the worktree.
pub fn reflink_ignored_caches(
    anchor: &Path,
    worktree: &Path,
    selection: &CacheSelection,
) -> Result<Vec<ReflinkResult>, String> {
    if !anchor.exists() || !worktree.exists() {
        return Err("Both anchor and worktree paths must exist".to_string());
    }
    if anchor == worktree {
        return Ok(Vec::new());
    }

    let virtualenvs: &[&str] = if selection.include_virtualenvs {
        VIRTUALENV_DIRS
    } else {
        &[]
    };
    let mut results = Vec::new();
    let mut failures = Vec::new();

    for dir_name in STANDARD_CACHE_DIRS.iter().chain(virtualenvs) {
        let anchor_dir = anchor.join(dir_name);
        let worktree_dir = worktree.join(dir_name);
        if !anchor_dir.is_dir() || worktree_dir.symlink_metadata().is_ok() {
            continue;
        }
        match ignored_in(worktree, dir_name) {
            Ok(true) => {}
            Ok(false) => {
                log::info!(
                    "Not copying '{dir_name}' into '{}': git does not ignore it there",
                    worktree.display()
                );
                continue;
            }
            Err(err) => {
                failures.push(format!("{dir_name}: {err}"));
                continue;
            }
        }
        match reflink_copy_dir_bounded(&anchor_dir, &worktree_dir, selection.caps) {
            Ok(res) => results.push(res),
            Err(err) => failures.push(format!("{dir_name}: {err}")),
        }
    }

    if failures.is_empty() {
        return Ok(results);
    }
    let copied = if results.is_empty() {
        String::new()
    } else {
        let names: Vec<&str> = results
            .iter()
            .filter_map(|r| Path::new(&r.destination).file_name()?.to_str())
            .collect();
        format!(" (copied: {})", names.join(", "))
    };
    Err(format!(
        "{} of the build caches could not be copied: {}{copied}",
        failures.len(),
        failures.join("; ")
    ))
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
        let (main, _parent, worktree) = anchor_and_worktree(&["node_modules"]);
        let anchor = main.path().canonicalize().unwrap();

        fs::create_dir_all(anchor.join("node_modules/pkg")).unwrap();
        let mut pkg_f = File::create(anchor.join("node_modules/pkg/index.js")).unwrap();
        writeln!(pkg_f, "module.exports = true;").unwrap();

        let results =
            reflink_ignored_caches(&anchor, &worktree, &CacheSelection::default()).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].files_cloned, 1);
        assert!(worktree.join("node_modules/pkg/index.js").exists());

        // Second call should not overwrite existing
        let second =
            reflink_ignored_caches(&anchor, &worktree, &CacheSelection::default()).unwrap();
        assert_eq!(second.len(), 0);
    }

    /// Entries directly under `dir`, so a test can say "nothing was left".
    fn entries(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// `levels` nested directories under `root`, with a file at the bottom.
    fn deep_tree(root: &Path, levels: usize) -> PathBuf {
        let mut dir = root.to_path_buf();
        for level in 1..=levels {
            dir = dir.join(format!("d{level}"));
        }
        fs::create_dir_all(&dir).unwrap();
        let leaf = dir.join("leaf.txt");
        fs::write(&leaf, "bottom").unwrap();
        leaf
    }

    /// A tree deeper than the walk's depth bound is copied whole or not at
    /// all: the walk used to skip the deeper directories, leave them empty,
    /// and report success.
    #[test]
    fn a_tree_past_the_depth_bound_is_copied_whole_or_not_at_all() {
        let temp = tempdir().unwrap();
        let src = temp.path().join("src");
        let leaf = deep_tree(&src, 13);
        let relative = leaf.strip_prefix(&src).unwrap().to_path_buf();

        // Under the default bounds a 13-deep tree is an ordinary one.
        let dst = temp.path().join("dst");
        let result = reflink_copy_dir(&src, &dst).expect("copied");
        assert!(dst.join(&relative).is_file(), "{result:?}");
        assert_eq!(result.files_cloned, 1);
        fs::remove_dir_all(&dst).unwrap();

        // Past the bound it is not copied at all, on either path (an APFS
        // clone that succeeded is discarded when it exceeds the bounds).
        let shallow = CowCaps {
            max_depth: 12,
            ..CowCaps::DEFAULT
        };
        let refused = reflink_copy_dir_bounded(&src, &dst, shallow);
        let error = refused.expect_err("a tree past the depth bound was reported copied");
        assert!(error.contains("12 directories deep"), "{error}");
        assert_eq!(
            entries(temp.path()),
            ["src"],
            "a refused copy left something"
        );
    }

    /// The walk's own bound, without the APFS clone in front of it.
    #[test]
    fn the_fallback_walk_refuses_a_tree_past_its_depth_bound() {
        let temp = tempdir().unwrap();
        let src = temp.path().join("src");
        deep_tree(&src, 13);
        let dst = temp.path().join("dst");
        let shallow = CowCaps {
            max_depth: 12,
            ..CowCaps::DEFAULT
        };
        let error = fallback_reflink_copy(&src, &dst, shallow)
            .map(|tally| tally.files)
            .expect_err("past the bound");
        assert!(error.contains("12 directories deep"), "{error}");
        let whole = temp.path().join("whole");
        let tally = fallback_reflink_copy(&src, &whole, CowCaps::DEFAULT).expect("copied");
        assert_eq!(tally.files, 1);
    }

    /// A tree with more entries than the cap yields an error and no
    /// destination, on the clone path and the walk alike.
    #[test]
    fn a_tree_past_the_entry_cap_leaves_no_destination() {
        let temp = tempdir().unwrap();
        let src = temp.path().join("src");
        fs::create_dir_all(src.join("pkg")).unwrap();
        for n in 0..5 {
            fs::write(src.join(format!("pkg/f{n}.js")), "x").unwrap();
        }
        let few = CowCaps {
            max_entries: 4,
            ..CowCaps::DEFAULT
        };
        let dst = temp.path().join("dst");
        let error = reflink_copy_dir_bounded(&src, &dst, few).expect_err("past the cap");
        assert!(error.contains("more than 4 entries"), "{error}");
        assert_eq!(entries(temp.path()), ["src"]);

        let walk = temp.path().join("walk");
        let error = fallback_reflink_copy(&src, &walk, few)
            .map(|tally| tally.files)
            .expect_err("past the cap");
        assert!(error.contains("more than 4 entries"), "{error}");

        let roomy = CowCaps {
            max_entries: 6,
            ..CowCaps::DEFAULT
        };
        let result = reflink_copy_dir_bounded(&src, &dst, roomy).expect("within the cap");
        assert_eq!(result.files_cloned, 5);
    }

    /// One unreadable file fails the copy, and the partial destination is
    /// removed rather than handed back as a cache that looks complete.
    #[cfg(unix)]
    #[test]
    fn an_unreadable_file_fails_the_copy_and_leaves_nothing() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempdir().unwrap();
        let src = temp.path().join("src");
        fs::create_dir_all(src.join("pkg")).unwrap();
        fs::write(src.join("pkg/a.js"), "a").unwrap();
        fs::write(src.join("pkg/locked.js"), "b").unwrap();
        fs::set_permissions(src.join("pkg/locked.js"), fs::Permissions::from_mode(0o000)).unwrap();
        let dst = temp.path().join("dst");
        let copied = reflink_copy_dir(&src, &dst);
        fs::set_permissions(src.join("pkg/locked.js"), fs::Permissions::from_mode(0o644)).unwrap();
        let error = copied.expect_err("a file that could not be read was reported copied");
        assert!(error.contains("locked.js"), "{error}");
        assert_eq!(
            entries(temp.path()),
            ["src"],
            "the partial copy was left behind"
        );
    }

    /// A destination that is not valid UTF-8 is refused before anything is
    /// written: its lossy spelling names a different directory.
    #[cfg(unix)]
    #[test]
    fn a_path_that_is_not_utf8_is_refused_not_respelled() {
        use std::os::unix::ffi::OsStrExt;
        let temp = tempdir().unwrap();
        let src = temp.path().join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("index.js"), "x").unwrap();
        let dst = temp.path().join(std::ffi::OsStr::from_bytes(b"cache-\xff"));
        let refused = reflink_copy_dir(&src, &dst);
        assert!(refused.is_err(), "{refused:?}");
        assert_eq!(entries(temp.path()), ["src"]);
    }

    /// A linked worktree of a fresh repository whose shared `info/exclude`
    /// ignores `ignored` (each a directory name).
    fn anchor_and_worktree(ignored: &[&str]) -> (tempfile::TempDir, tempfile::TempDir, PathBuf) {
        let (main, parent, linked) = crate::test_support::repo_with_worktrees(1);
        let rules: String = ignored.iter().map(|dir| format!("{dir}/\n")).collect();
        fs::create_dir_all(main.path().join(".git/info")).unwrap();
        fs::write(main.path().join(".git/info/exclude"), rules).unwrap();
        let worktree = linked.into_iter().next().unwrap();
        (main, parent, worktree)
    }

    /// A copy that failed is retried by the next sync instead of being
    /// skipped because its half-made destination now exists.
    #[cfg(unix)]
    #[test]
    fn a_failed_cache_copy_is_retried_by_the_next_sync() {
        use std::os::unix::fs::PermissionsExt;
        let (main, _parent, worktree) = anchor_and_worktree(&["node_modules"]);
        let anchor = main.path().canonicalize().unwrap();
        let locked = anchor.join("node_modules/pkg/locked.js");
        fs::create_dir_all(locked.parent().unwrap()).unwrap();
        fs::write(anchor.join("node_modules/pkg/index.js"), "i").unwrap();
        fs::write(&locked, "l").unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        let first = reflink_ignored_caches(&anchor, &worktree, &CacheSelection::default());
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(first.is_err(), "an incomplete copy was reported: {first:?}");
        assert!(!worktree.join("node_modules").exists());

        let second = reflink_ignored_caches(&anchor, &worktree, &CacheSelection::default())
            .expect("retried");
        assert_eq!(second.len(), 1, "{second:?}");
        assert!(worktree.join("node_modules/pkg/locked.js").is_file());
        assert!(worktree.join("node_modules/pkg/index.js").is_file());
    }

    /// The directory names are candidates, not a licence: one the worktree's
    /// own ignore rules do not cover is never copied into it.
    #[test]
    fn a_cache_directory_git_does_not_ignore_is_not_copied() {
        let (main, _parent, worktree) = anchor_and_worktree(&[]);
        let anchor = main.path().canonicalize().unwrap();
        fs::create_dir_all(anchor.join("node_modules/pkg")).unwrap();
        fs::write(anchor.join("node_modules/pkg/index.js"), "i").unwrap();
        let copied =
            reflink_ignored_caches(&anchor, &worktree, &CacheSelection::default()).expect("sync");
        assert!(copied.is_empty(), "{copied:?}");
        assert!(!worktree.join("node_modules").exists());
    }

    /// An ignore check that produced no answer is neither "ignored" nor
    /// "not ignored": nothing is copied, and the sync fails naming the cache.
    #[test]
    fn an_unanswered_ignore_check_fails_the_sync_and_copies_nothing() {
        let (main, _parent, worktree) = anchor_and_worktree(&["node_modules"]);
        let anchor = main.path().canonicalize().unwrap();
        fs::create_dir_all(anchor.join("node_modules/pkg")).unwrap();
        fs::write(anchor.join("node_modules/pkg/index.js"), "i").unwrap();
        let synced = crate::engine::git_cli::with_forced_spawn_failure_of("check-ignore", || {
            reflink_ignored_caches(&anchor, &worktree, &CacheSelection::default())
        });
        let error = synced.expect_err("an unchecked cache was copied or skipped silently");
        assert!(error.contains("node_modules"), "{error}");
        assert!(!worktree.join("node_modules").exists());
    }

    /// A virtualenv embeds its own absolute path (`bin/activate`,
    /// `pyvenv.cfg`, script shebangs), so a copy runs the anchor's
    /// interpreter and installs into the anchor's environment.
    #[test]
    fn a_virtualenv_is_not_copied_by_default() {
        let (main, _parent, worktree) = anchor_and_worktree(&[".venv", "venv"]);
        let anchor = main.path().canonicalize().unwrap();
        simulated_venv(&anchor.join(".venv"));
        let copied =
            reflink_ignored_caches(&anchor, &worktree, &CacheSelection::default()).expect("sync");
        assert!(copied.is_empty(), "{copied:?}");
        assert!(!worktree.join(".venv").exists());

        // A caller that asks for virtualenvs gets them.
        let opted_in = CacheSelection {
            include_virtualenvs: true,
            ..CacheSelection::default()
        };
        let copied = reflink_ignored_caches(&anchor, &worktree, &opted_in).expect("sync");
        assert_eq!(copied.len(), 1, "{copied:?}");
        assert!(worktree.join(".venv/pyvenv.cfg").is_file());
    }

    /// The layout `python -m venv` writes that matters here: the anchor's
    /// absolute path baked into the activation script and the config.
    /// Written by hand so the test does not depend on a Python on the host.
    fn simulated_venv(venv: &Path) {
        fs::create_dir_all(venv.join("bin")).unwrap();
        fs::write(
            venv.join("pyvenv.cfg"),
            format!(
                "home = /usr/bin\ncommand = /usr/bin/python3 -m venv {}\n",
                venv.display()
            ),
        )
        .unwrap();
        fs::write(
            venv.join("bin/activate"),
            format!("VIRTUAL_ENV=\"{}\"\nexport VIRTUAL_ENV\n", venv.display()),
        )
        .unwrap();
    }

    /// A worktree made on a branch without the anchor's tracked `vendor/`,
    /// beside an ignored virtualenv, starts with nothing git reports.
    #[test]
    fn a_new_worktree_gains_no_untracked_directories_from_the_anchor() {
        let (main, _parent, _linked) = crate::test_support::repo_with_worktrees(0);
        let anchor = main.path().canonicalize().unwrap();
        let base = crate::engine::git_cli::git_text(&anchor, &["rev-parse", "HEAD"]).unwrap();
        fs::create_dir_all(anchor.join("vendor/lib")).unwrap();
        fs::write(anchor.join("vendor/lib/mod.rs"), "pub fn f() {}\n").unwrap();
        crate::test_support::git_in(&anchor, &["add", "vendor"]);
        crate::test_support::git_in(&anchor, &["commit", "-q", "-m", "vendor"]);
        fs::write(anchor.join(".git/info/exclude"), ".venv/\nnode_modules/\n").unwrap();
        simulated_venv(&anchor.join(".venv"));
        fs::create_dir_all(anchor.join("node_modules/pkg")).unwrap();
        fs::write(anchor.join("node_modules/pkg/index.js"), "i").unwrap();

        let holder = tempdir().unwrap();
        let target = holder.path().join("lane");
        let created = crate::engine::worktree::add_worktree_extended(
            anchor.to_str().unwrap(),
            target.to_str().unwrap(),
            Some("before-vendor"),
            Some(base.trim()),
            false,
            true,
            false,
        )
        .expect("created");
        assert_eq!(created.cache_error, None);
        let status = crate::engine::git_cli::git_text(&target, &["status", "--porcelain"]).unwrap();
        assert_eq!(status.trim(), "", "the worktree starts with changes");
        assert!(target.join("node_modules/pkg/index.js").is_file());
        assert!(!target.join(".venv").exists());
        assert!(!target.join("vendor").exists());
    }
}
