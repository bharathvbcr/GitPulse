//! Copy-on-Write directory and build-cache cloning.
//!
//! AI coding agents parallelize across linked worktrees, but duplicating
//! multi-gigabyte dependency trees (`node_modules`) burns disk space and
//! stalls launches on repeated installations, and a fresh Cargo `target/`
//! recompiles every dependency. This module clones them:
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
//!
//! Cargo's `target/` is cloned only by a real clone (never byte-copied: a
//! byte copy of a build directory costs the disk the clone was meant to
//! save), only while no anchor build holds its `.cargo-lock`, and with the
//! anchor's absolute paths in recorded build-script output rewritten to the
//! clone's. Registry and git crates' fingerprints carry no workspace path,
//! so the worktree reuses their compiled artifacts; path crates rebuild.

use serde::{Deserialize, Serialize};
#[cfg(target_os = "macos")]
use std::ffi::CString;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use crate::sysload::{free_bytes, FreeSpace, LowDisk, LowDiskCause, Reading, Why, LOW_DISK_FLOOR};

/// Deepest directory nesting one copy accepts. pnpm and nested npm installs
/// go well past a dozen levels (`.pnpm/<pkg>/node_modules/@scope/<pkg>/...`).
pub const MAX_COW_DEPTH: usize = 64;
/// Most entries (files, directories and links) one copy accepts. A large
/// `node_modules` holds a few hundred thousand.
pub const MAX_COW_ENTRIES: usize = 1_000_000;
/// Most bytes one copy may physically write. A clone writes none; a tree this
/// filesystem can only byte-copy (ext4, NTFS, a non-APFS volume) writes its
/// whole size, which sixteen lanes of a few gigabytes each turns into a full
/// disk.
pub const MAX_COW_BYTES: u64 = 8 << 30;

/// Free space is read again after a byte-copy has written this many bytes, so
/// a volume that fills during a long copy ends it instead of being read once
/// beforehand and trusted.
pub const RECHECK_EVERY_BYTES: u64 = 64 << 20;

/// The bounds one copy runs under. A tree past any is not copied at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CowCaps {
    pub max_depth: usize,
    pub max_entries: usize,
    /// Bytes the copy may write by byte-copying (cloned files cost none).
    pub max_bytes: u64,
}

impl CowCaps {
    pub const DEFAULT: CowCaps = CowCaps {
        max_depth: MAX_COW_DEPTH,
        max_entries: MAX_COW_ENTRIES,
        max_bytes: MAX_COW_BYTES,
    };
}

/// Cache directories cloned by default, each only where the worktree's git
/// ignore rules cover it. `node_modules` and `.turbo` hold no absolute path
/// of the checkout they were made in. Cargo's `target/` reuses compiled
/// registry and git dependencies (their fingerprints name no workspace
/// path); the paths it does record — build-script output — are rewritten
/// for the clone, and it is cloned only by a real clone (see
/// [`CARGO_TARGET_DIR`]). `build/` and `dist/` are not caches: a stale
/// `dist/` would be embedded by the next bundling build. `vendor/` is often
/// tracked.
pub const STANDARD_CACHE_DIRS: &[&str] = &["node_modules", ".turbo", CARGO_TARGET_DIR];

/// Cargo's build directory. Cloned only when every file can be cloned (APFS
/// `clonefile`, or FICLONE on each file), never byte-copied; skipped, with
/// the reason reported, while an anchor build holds a `.cargo-lock` in it.
pub const CARGO_TARGET_DIR: &str = "target";

/// Profile directories (`target/debug`, `target/release`, ...) whose
/// `.cargo-lock` a target clone takes. Far above any real target directory.
const MAX_TARGET_PROFILES: usize = 256;

/// Largest recorded build-script output rewritten for a target clone. Real
/// ones are a few kilobytes of `cargo:` lines.
const MAX_BUILD_OUTPUT: u64 = 16 * 1024 * 1024;

/// Build scripts' directories under one profile's `build/` the rewrite
/// visits; past it the target clone fails rather than leave some unrewritten.
const MAX_BUILD_SCRIPTS: usize = 100_000;

/// Python virtual environments. `python -m venv` writes its own absolute path
/// into `bin/activate`, `pyvenv.cfg` and every script's shebang, so a copy
/// runs the anchor's interpreter and installs into the anchor's environment.
/// Cloned only when a caller opts in.
pub const VIRTUALENV_DIRS: &[&str] = &[".venv", "venv"];

/// Which caches [`reflink_ignored_caches`] considers, the bounds each copy
/// runs under, and how free space on the worktree's volume is read.
#[derive(Debug, Clone, Copy)]
pub struct CacheSelection {
    pub include_virtualenvs: bool,
    pub caps: CowCaps,
    pub free_space: FreeSpace,
}

impl Default for CacheSelection {
    fn default() -> Self {
        Self {
            include_virtualenvs: false,
            caps: CowCaps::DEFAULT,
            free_space: free_bytes,
        }
    }
}

/// What copying the caches produced: the copies made, the failures (each
/// named), and, apart from them, what was skipped for want of disk. A skip is
/// advice, not a failure: the worktree is as usable without its caches.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CacheReport {
    pub copied: Vec<ReflinkResult>,
    pub failures: Vec<String>,
    pub low_disk: Option<LowDisk>,
    /// The volume's free space could not be read, so nothing was skipped on
    /// its account and nothing is known to be plentiful.
    pub disk_unknown: bool,
}

impl CacheReport {
    /// The failures as one message, `None` when there were none.
    pub fn error(&self) -> Option<String> {
        if self.failures.is_empty() {
            return None;
        }
        let copied = if self.copied.is_empty() {
            String::new()
        } else {
            let names: Vec<&str> = self
                .copied
                .iter()
                .filter_map(|r| Path::new(&r.destination).file_name()?.to_str())
                .collect();
            format!(" (copied: {})", names.join(", "))
        };
        Some(format!(
            "{} of the build caches were not copied: {}{copied}",
            self.failures.len(),
            self.failures.join("; ")
        ))
    }
}

/// Why a copy stopped. The byte cap is a typed stop beside a failure, so the
/// cache loop tells them apart from the value it was handed, on whatever
/// thread the copy ran. The public entry points flatten it to the message.
#[derive(Debug, Clone, PartialEq, Eq)]
enum CopyStop {
    /// The tree would have written more than `cap` bytes by byte-copying.
    CapHit {
        cap: u64,
    },
    /// The volume's free space fell below the floor while the copy ran.
    BelowFloor {
        free: u64,
    },
    Failed(String),
}

impl From<String> for CopyStop {
    fn from(message: String) -> Self {
        Self::Failed(message)
    }
}

impl CopyStop {
    /// The message every caller of the public entry points sees.
    fn into_message(self) -> String {
        match self {
            Self::CapHit { cap } => {
                format!("the copy would write more than {cap} bytes (the byte cap), so it was not copied")
            }
            Self::BelowFloor { free } => format!(
                "free space fell to {free} bytes during the copy, below the floor, so it was not copied"
            ),
            Self::Failed(message) => message,
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

/// What a file occupies on disk: its length rounded up to the volume's block
/// size, or the blocks the filesystem reports, whichever is more. Over-counts
/// (a sparse file counts whole), never under-counts.
#[cfg(unix)]
fn allocated(meta: &fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    let block = meta.blksize().max(512);
    meta.len()
        .div_ceil(block)
        .saturating_mul(block)
        .max(meta.blocks().saturating_mul(512))
}

#[cfg(not(unix))]
fn allocated(meta: &fs::Metadata) -> u64 {
    meta.len()
}

/// Copies one file's first `limit` bytes. A file that grew after it was
/// measured is cut at the measured size, so the bytes charged are the bytes
/// written.
fn copy_bounded(src: &Path, dst: &Path, limit: u64) -> std::io::Result<()> {
    let mut input = fs::File::open(src)?;
    let permissions = input.metadata()?.permissions();
    let mut output = fs::File::create_new(dst)?;
    std::io::copy(&mut (&mut input).take(limit), &mut output)?;
    drop(output);
    fs::set_permissions(dst, permissions)
}

/// Bytes the byte-copies running in this process have each been allowed to
/// write, by volume. Concurrent lanes take their headroom from what is left
/// after these, so they cannot jointly cross the floor.
static RESERVED: Mutex<Vec<(u64, u64)>> = Mutex::new(Vec::new());

/// A share of a volume's headroom, held while one copy runs.
struct Reservation {
    volume: u64,
    bytes: u64,
}

impl Reservation {
    /// Takes up to `want` of `headroom` bytes on `volume`, less what other
    /// copies hold. Released when dropped, whether the copy finished, failed
    /// or unwound.
    fn take(volume: u64, headroom: u64, want: u64) -> Self {
        let mut held = RESERVED
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let others = held
            .iter()
            .filter(|(v, _)| *v == volume)
            .map(|(_, b)| *b)
            .fold(0u64, u64::saturating_add);
        let bytes = want.min(headroom.saturating_sub(others));
        held.push((volume, bytes));
        Self { volume, bytes }
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        let mut held = RESERVED
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(at) = held
            .iter()
            .position(|&(v, b)| v == self.volume && b == self.bytes)
        {
            held.swap_remove(at);
        }
    }
}

/// The volume a path is on, for reservations; one shared key where the
/// platform gives none.
fn volume_of(path: &Path) -> u64 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        fs::metadata(path).map_or(0, |meta| meta.dev())
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        0
    }
}

/// A free-space read that cannot take the caller down: a reader that panics
/// is a volume that could not be read.
fn read_free(reader: FreeSpace, path: &Path) -> Reading<u64> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| reader(path))).unwrap_or_else(|_| {
        log::warn!("the free-space reader panicked for '{}'", path.display());
        Reading::Unavailable(Why::Failed)
    })
}

/// Removes a staging tree when dropped armed, so a panic or an early return
/// between creating it and renaming it never leaves it beside the destination.
struct StagingGuard<'a> {
    path: &'a Path,
    armed: bool,
}

impl Drop for StagingGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_dir_all(self.path);
        }
    }
}

/// Work done on a finished staging tree before it is renamed into place.
type Finish<'a> = &'a dyn Fn(&Path) -> Result<(), String>;

/// Reads the volume's free space again as a byte-copy proceeds.
type Recheck<'a> = &'a dyn Fn() -> Reading<u64>;

/// What one copy demands beyond its bounds.
#[derive(Clone, Copy, Default)]
struct CopyRules<'a> {
    /// Every file must be cloned; a tree this filesystem can only byte-copy
    /// is not copied at all.
    require_clone: bool,
    /// Runs on the finished staging tree; an error discards the copy.
    finish: Option<Finish<'a>>,
}

/// Clones a directory tree from `src` to `dst`, prioritizing Copy-on-Write,
/// under the default bounds.
pub fn reflink_copy_dir(src: &Path, dst: &Path) -> Result<ReflinkResult, String> {
    reflink_copy_dir_bounded(src, dst, CowCaps::DEFAULT, CopyRules::default())
}

/// [`reflink_copy_dir`] under `caps` and `rules`, as a message.
fn reflink_copy_dir_bounded(
    src: &Path,
    dst: &Path,
    caps: CowCaps,
    rules: CopyRules<'_>,
) -> Result<ReflinkResult, String> {
    copy_dir_staged(src, dst, caps, rules).map_err(CopyStop::into_message)
}

/// [`reflink_copy_dir_bounded`], keeping why the copy stopped.
fn copy_dir_staged(
    src: &Path,
    dst: &Path,
    caps: CowCaps,
    rules: CopyRules<'_>,
) -> Result<ReflinkResult, CopyStop> {
    copy_dir_rechecked(src, dst, caps, rules, None)
}

/// [`copy_dir_staged`] that reads free space again as it writes.
fn copy_dir_rechecked(
    src: &Path,
    dst: &Path,
    caps: CowCaps,
    rules: CopyRules<'_>,
    recheck: Option<Recheck<'_>>,
) -> Result<ReflinkResult, CopyStop> {
    // Refused, not respelled: a lossy spelling names a different file, and
    // the clone and its cleanup would act on that one.
    let src_text = utf8_path(src, "Source")?;
    let dst_text = utf8_path(dst, "Destination")?;
    if !src.exists() {
        return Err(CopyStop::from(format!(
            "Source path '{src_text}' does not exist"
        )));
    }
    if !src.is_dir() {
        return Err(CopyStop::from(format!(
            "Source path '{src_text}' is not a directory"
        )));
    }
    // `symlink_metadata`: a dangling link is still something in the way.
    if dst.symlink_metadata().is_ok() {
        return Err(CopyStop::from(format!(
            "Destination path '{dst_text}' already exists"
        )));
    }

    let start = Instant::now();
    let staging = staging_path(dst)?;
    let mut guard = StagingGuard {
        path: &staging,
        armed: true,
    };
    let copied = clone_tree(src, &staging, caps, rules, recheck).and_then(|tally| {
        if let Some(finish) = rules.finish {
            finish(&staging)?;
        }
        fs::rename(&staging, dst).map(|()| tally).map_err(|e| {
            CopyStop::from(format!(
                "Could not move the finished copy into '{dst_text}': {e}"
            ))
        })
    });
    // Past a panic the guard has removed the tree; from here the removal is
    // reported, not silent.
    guard.armed = false;
    match copied {
        Ok(tally) => Ok(ReflinkResult {
            source: src_text.to_string(),
            destination: dst_text.to_string(),
            files_cloned: tally.files,
            bytes_cloned: tally.bytes,
            is_cow: tally.is_cow,
            duration_ms: start.elapsed().as_millis() as u64,
        }),
        Err(CopyStop::Failed(error)) => Err(CopyStop::Failed(discard_staging(&staging, error))),
        Err(bound) => {
            // The staging tree goes for a bound exactly as for a failure; the
            // stop stays that bound unless its removal failed too.
            let left = discard_staging(&staging, String::new());
            Err(if left.is_empty() {
                bound
            } else {
                CopyStop::Failed(format!("{}{left}", bound.into_message()))
            })
        }
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

/// Why a tree that must be cloned was not copied.
const NOT_CLONED: &str = "this filesystem could not clone it, and a build directory is never byte-copied, so it was skipped";

#[cfg(test)]
thread_local! {
    static FORCE_BYTE_COPY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether a test has ruled out every clone primitive on this thread, so the
/// byte-copy path runs on a filesystem that could clone.
#[cfg(test)]
fn byte_copy_forced() -> bool {
    FORCE_BYTE_COPY.with(std::cell::Cell::get)
}

#[cfg(not(test))]
fn byte_copy_forced() -> bool {
    false
}

/// Runs `body` as if this filesystem could not clone (Windows, ext4, a
/// cross-device FICLONE).
#[cfg(test)]
pub(crate) fn with_forced_byte_copy<T>(body: impl FnOnce() -> T) -> T {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            FORCE_BYTE_COPY.with(|cell| cell.set(self.0));
        }
    }
    let _restore = Restore(FORCE_BYTE_COPY.with(|cell| cell.replace(true)));
    body()
}

/// Copies `src` into `staging`, which must not exist yet. With
/// `require_clone`, a tree that cannot be cloned is not copied.
fn clone_tree(
    src: &Path,
    staging: &Path,
    caps: CowCaps,
    rules: CopyRules<'_>,
    recheck: Option<Recheck<'_>>,
) -> Result<Tally, CopyStop> {
    let require_clone = rules.require_clone;
    #[cfg(target_os = "macos")]
    if !byte_copy_forced() {
        if !require_clone {
            // `copyfile(3)` clones where it can and writes bytes where it
            // cannot (another volume), and does not say which. What it could
            // write is counted first, against the same cap and floor.
            precount_allocated(src, caps)?;
        }
        let src_c = CString::new(utf8_path(src, "Source")?)
            .map_err(|e| format!("Invalid source path: {e}"))?;
        let dst_c = CString::new(utf8_path(staging, "Destination")?)
            .map_err(|e| format!("Invalid destination path: {e}"))?;
        let ret = if require_clone {
            // `copyfile(3)`'s `COPYFILE_CLONE` quietly copies data where it
            // cannot clone, and its `COPYFILE_CLONE_FORCE` refuses
            // directories. `clonefile(2)` clones a whole hierarchy in one
            // call, or fails (ENOTSUP off APFS, EXDEV across volumes).
            // SAFETY: `src_c` and `dst_c` are NUL-terminated strings that
            // outlive the call; `clonefile(2)` only reads them and touches no
            // other Rust-owned memory.
            unsafe { libc::clonefile(src_c.as_ptr(), dst_c.as_ptr(), 0) }
        } else {
            let flags = COPYFILE_ALL | COPYFILE_RECURSIVE | COPYFILE_CLONE;
            // SAFETY: `src_c` and `dst_c` are NUL-terminated strings that
            // outlive the call; `copyfile(3)` only reads them. A null `state`
            // is the documented way to run without a copyfile_state_t. The
            // call touches no Rust-owned memory besides those two buffers.
            unsafe { copyfile(src_c.as_ptr(), dst_c.as_ptr(), std::ptr::null_mut(), flags) }
        };
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
                return Err(CopyStop::from(format!(
                    "The clone failed and its partial copy at '{}' could not be removed: {e}",
                    staging.display()
                )));
            }
        }
        if require_clone {
            return Err(CopyStop::from(NOT_CLONED.to_string()));
        }
    }

    walk_copy(src, staging, caps, rules, recheck)
}

/// Fails with the cap when `src` holds more allocated bytes than `caps` allow
/// a copy to write. Stops counting at the cap.
#[cfg(target_os = "macos")]
fn precount_allocated(src: &Path, caps: CowCaps) -> Result<(), CopyStop> {
    let mut total = 0u64;
    let mut entries = 0usize;
    let mut stack = vec![src.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let read =
            fs::read_dir(&dir).map_err(|e| format!("Cannot read '{}': {e}", dir.display()))?;
        for entry in read {
            let entry = entry.map_err(|e| format!("Cannot list '{}': {e}", dir.display()))?;
            entries += 1;
            if entries > caps.max_entries {
                return Err(CopyStop::from(format!(
                    "'{}' holds more than {} entries, so it was not copied",
                    src.display(),
                    caps.max_entries
                )));
            }
            let meta = entry
                .metadata()
                .map_err(|e| format!("Cannot read '{}': {e}", entry.path().display()))?;
            if meta.is_dir() {
                stack.push(entry.path());
            } else if meta.is_file() {
                total = total.saturating_add(allocated(&meta));
                if total > caps.max_bytes {
                    return Err(CopyStop::CapHit {
                        cap: caps.max_bytes,
                    });
                }
            }
        }
    }
    Ok(())
}

/// One file cloned rather than copied, where this platform can.
fn try_clone_file(src: &Path, dst: &Path) -> bool {
    if byte_copy_forced() {
        return false;
    }
    #[cfg(target_os = "linux")]
    {
        try_linux_ficlone(src, dst).unwrap_or(false)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (src, dst);
        false
    }
}

/// [`walk_copy`] as a message, for the callers and tests that read one.
#[cfg(test)]
fn fallback_reflink_copy(
    src: &Path,
    dst: &Path,
    caps: CowCaps,
    require_clone: bool,
) -> Result<Tally, String> {
    let rules = CopyRules {
        require_clone,
        ..CopyRules::default()
    };
    walk_copy(src, dst, caps, rules, None).map_err(CopyStop::into_message)
}

/// Recursive copier that attempts file-level FICLONE on Linux, or `fs::copy`
/// elsewhere. Every entry is copied or the copy fails: an unreadable
/// directory or file, a failed write, an entry that is none of file,
/// directory or link, or a bound reached is an error naming it. With
/// `require_clone`, a file that could not be cloned ends the copy.
fn walk_copy(
    src: &Path,
    dst: &Path,
    caps: CowCaps,
    rules: CopyRules<'_>,
    recheck: Option<Recheck<'_>>,
) -> Result<Tally, CopyStop> {
    let require_clone = rules.require_clone;
    fs::create_dir(dst).map_err(|e| format!("Failed to create destination dir: {e}"))?;

    let mut tally = Tally {
        files: 0,
        bytes: 0,
        is_cow: false,
    };
    let mut entries = 0usize;
    let mut written = 0u64;
    let mut since_check = 0u64;
    let mut stack: Vec<(PathBuf, PathBuf, usize)> = vec![(src.to_path_buf(), dst.to_path_buf(), 0)];

    while let Some((curr_src, curr_dst, depth)) = stack.pop() {
        let read_dir = fs::read_dir(&curr_src)
            .map_err(|e| format!("Cannot read '{}': {e}", curr_src.display()))?;
        for entry in read_dir {
            let entry = entry.map_err(|e| format!("Cannot list '{}': {e}", curr_src.display()))?;
            entries += 1;
            if entries > caps.max_entries {
                return Err(CopyStop::from(format!(
                    "'{}' holds more than {} entries, so it was not copied",
                    src.display(),
                    caps.max_entries
                )));
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
                    return Err(CopyStop::from(format!(
                        "'{}' is nested more than {} directories deep, so it was not copied",
                        src.display(),
                        caps.max_depth
                    )));
                }
                fs::create_dir(&target_path)
                    .map_err(|e| format!("Cannot create '{}': {e}", target_path.display()))?;
                stack.push((entry_path, target_path, depth + 1));
            } else if kind.is_file() {
                if try_clone_file(&entry_path, &target_path) {
                    tally.is_cow = true;
                } else if require_clone {
                    return Err(CopyStop::from(NOT_CLONED.to_string()));
                } else {
                    let cost = allocated(&meta);
                    written = written.saturating_add(cost);
                    if written > caps.max_bytes {
                        return Err(CopyStop::CapHit {
                            cap: caps.max_bytes,
                        });
                    }
                    copy_bounded(&entry_path, &target_path, meta.len())
                        .map_err(|e| format!("Could not copy '{}': {e}", entry_path.display()))?;
                    since_check = since_check.saturating_add(cost);
                    if let (Some(free), true) = (recheck, since_check >= RECHECK_EVERY_BYTES) {
                        since_check = 0;
                        if let Reading::Value(left) = free() {
                            if left < LOW_DISK_FLOOR {
                                return Err(CopyStop::BelowFloor { free: left });
                            }
                        }
                    }
                }
                tally.files += 1;
                tally.bytes += meta.len();
            } else if kind.is_symlink() {
                copy_link(&entry_path, &target_path)?;
                tally.files += 1;
            } else {
                return Err(CopyStop::from(format!(
                    "'{}' is not a file, directory or link, so the tree was not copied",
                    entry_path.display()
                )));
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

/// Takes, without waiting, every `<target>/<profile>/.cargo-lock` the anchor
/// has — the lock cargo holds for a whole build of that profile — so no
/// anchor build writes the tree while it is cloned. Released when the files
/// drop. A profile with no lock file has no build to wait for.
fn hold_cargo_build_locks(target: &Path) -> Result<Vec<fs::File>, String> {
    let profiles =
        fs::read_dir(target).map_err(|e| format!("Cannot read '{}': {e}", target.display()))?;
    let mut held = Vec::new();
    for (index, profile) in profiles.enumerate() {
        if index >= MAX_TARGET_PROFILES {
            return Err(format!(
                "'{}' holds more than {MAX_TARGET_PROFILES} entries, so its build locks were not all checked",
                target.display()
            ));
        }
        let profile = profile.map_err(|e| format!("Cannot list '{}': {e}", target.display()))?;
        let lock_path = profile.path().join(".cargo-lock");
        let file = match fs::File::open(&lock_path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            // A profile entry that is a file, not a directory.
            Err(_) if !profile.path().is_dir() => continue,
            Err(e) => return Err(format!("Cannot open '{}': {e}", lock_path.display())),
        };
        match file.try_lock() {
            Ok(()) => held.push(file),
            Err(fs::TryLockError::WouldBlock) => {
                return Err(format!(
                    "anchor build in progress ('{}' is locked)",
                    lock_path.display()
                ))
            }
            Err(fs::TryLockError::Error(e)) => {
                return Err(format!("Cannot lock '{}': {e}", lock_path.display()))
            }
        }
    }
    Ok(held)
}

/// Points a cloned target directory's recorded build-script output at the
/// clone. Cargo never reruns a registry crate's build script, so its
/// `output` (`cargo:rustc-link-search=native=<out dir>`) and `root-output`
/// keep naming the anchor's out directories — and the worktree silently
/// links against the anchor's target until a `cargo clean` there.
///
/// Visits `<target>/<profile>/build/<script>/{output,root-output}`; replaces
/// `from` (the anchor's target directory) with `to` wherever it is a whole
/// path prefix. A file that cannot be read as UTF-8, or is oversized, fails
/// the clone.
fn rewrite_build_outputs(staging: &Path, from: &str, to: &str) -> Result<(), String> {
    let profiles =
        fs::read_dir(staging).map_err(|e| format!("Cannot read '{}': {e}", staging.display()))?;
    for (index, profile) in profiles.enumerate() {
        if index >= MAX_TARGET_PROFILES {
            return Err(format!(
                "the target directory holds more than {MAX_TARGET_PROFILES} entries"
            ));
        }
        let profile = profile.map_err(|e| format!("Cannot list '{}': {e}", staging.display()))?;
        let build = profile.path().join("build");
        if !build.is_dir() {
            continue;
        }
        let scripts =
            fs::read_dir(&build).map_err(|e| format!("Cannot read '{}': {e}", build.display()))?;
        for (index, script) in scripts.enumerate() {
            if index >= MAX_BUILD_SCRIPTS {
                return Err(format!(
                    "'{}' holds more than {MAX_BUILD_SCRIPTS} entries",
                    build.display()
                ));
            }
            let script = script.map_err(|e| format!("Cannot list '{}': {e}", build.display()))?;
            for name in ["output", "root-output"] {
                rewrite_one_output(&script.path().join(name), from, to)?;
            }
        }
    }
    Ok(())
}

fn rewrite_one_output(path: &Path, from: &str, to: &str) -> Result<(), String> {
    let meta = match path.symlink_metadata() {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(format!("Cannot read '{}': {e}", path.display())),
    };
    if !meta.is_file() {
        return Err(format!("'{}' is not a file", path.display()));
    }
    if meta.len() > MAX_BUILD_OUTPUT {
        return Err(format!(
            "'{}' is larger than {MAX_BUILD_OUTPUT} bytes",
            path.display()
        ));
    }
    let bytes = fs::read(path).map_err(|e| format!("Cannot read '{}': {e}", path.display()))?;
    let text = String::from_utf8(bytes).map_err(|_| {
        format!(
            "'{}' is not UTF-8, so its paths cannot be rewritten",
            path.display()
        )
    })?;
    let rewritten = replace_path_prefix(&text, from, to);
    if rewritten != text {
        fs::write(path, rewritten)
            .map_err(|e| format!("Cannot rewrite '{}': {e}", path.display()))?;
    }
    Ok(())
}

/// `text` with every occurrence of the path `from` that is a whole path or a
/// path prefix (followed by a separator, a line end or the end) replaced by
/// `to`. `/a/target2` is not inside `/a/target`.
fn replace_path_prefix(text: &str, from: &str, to: &str) -> String {
    if from.is_empty() {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(from) {
        let after = &rest[at + from.len()..];
        let whole = after.is_empty() || after.starts_with(['/', '\\', '\n', '\r']);
        out.push_str(&rest[..at]);
        out.push_str(if whole { to } else { from });
        rest = after;
    }
    out.push_str(rest);
    out
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
    let report = clone_ignored_caches(anchor, worktree, selection)?;
    if let Some(error) = report.error() {
        return Err(error);
    }
    match report.low_disk {
        Some(low) => Err(low.message()),
        None => Ok(report.copied),
    }
}

/// [`reflink_ignored_caches`], reporting a skip for want of disk apart from a
/// failure. Free space is read on the worktree's volume before each cache: a
/// volume below [`LOW_DISK_FLOOR`] gets no copy, and a byte-copied tree may
/// write no more than the cap or what would leave the floor. A volume that
/// cannot be read is unknown, never plentiful, and forces no skip.
pub fn clone_ignored_caches(
    anchor: &Path,
    worktree: &Path,
    selection: &CacheSelection,
) -> Result<CacheReport, String> {
    if !anchor.exists() || !worktree.exists() {
        return Err("Both anchor and worktree paths must exist".to_string());
    }
    let mut report = CacheReport::default();
    if anchor == worktree {
        return Ok(report);
    }

    let virtualenvs: &[&str] = if selection.include_virtualenvs {
        VIRTUALENV_DIRS
    } else {
        &[]
    };
    let mut skipped: Vec<String> = Vec::new();
    let mut cause = LowDiskCause::BelowFloor;
    let mut free_seen: Option<u64> = None;
    let mut cap_seen: Option<u64> = None;

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
                report.failures.push(format!("{dir_name}: {err}"));
                continue;
            }
        }
        let mut caps = selection.caps;
        let mut reservation = None;
        match read_free(selection.free_space, worktree) {
            Reading::Value(free) => {
                free_seen = Some(free);
                if free < LOW_DISK_FLOOR {
                    skipped.push((*dir_name).to_string());
                    continue;
                }
                let held =
                    Reservation::take(volume_of(worktree), free - LOW_DISK_FLOOR, caps.max_bytes);
                caps.max_bytes = held.bytes;
                reservation = Some(held);
            }
            Reading::Unavailable(_) => report.disk_unknown = true,
        }
        let recheck = || read_free(selection.free_space, worktree);
        let copied = if *dir_name == CARGO_TARGET_DIR {
            clone_cargo_target(&anchor_dir, &worktree_dir, caps)
        } else {
            copy_dir_rechecked(
                &anchor_dir,
                &worktree_dir,
                caps,
                CopyRules::default(),
                Some(&recheck),
            )
        };
        drop(reservation);
        match copied {
            Ok(res) => report.copied.push(res),
            Err(CopyStop::BelowFloor { free }) => {
                cause = LowDiskCause::BelowFloor;
                free_seen = Some(free);
                skipped.push((*dir_name).to_string());
            }
            Err(CopyStop::CapHit { .. }) => {
                // The cap, or the floor when the volume's headroom was the
                // smaller bound: nothing was copied, and that is advice.
                if caps.max_bytes < selection.caps.max_bytes {
                    cause = LowDiskCause::BelowFloor;
                } else {
                    cause = LowDiskCause::ByteCap;
                    cap_seen = Some(caps.max_bytes);
                }
                skipped.push((*dir_name).to_string());
            }
            Err(stop) => report
                .failures
                .push(format!("{dir_name}: {}", stop.into_message())),
        }
    }

    if !skipped.is_empty() {
        report.low_disk = Some(LowDisk {
            cause,
            free_bytes: free_seen,
            floor_bytes: LOW_DISK_FLOOR,
            cap_bytes: cap_seen,
            skipped,
        });
    }
    Ok(report)
}

/// Clones the anchor's Cargo target directory: a real directory, under its
/// build locks, by clone only, with recorded build-script paths rewritten
/// to the destination before the copy is renamed into place.
fn clone_cargo_target(
    anchor_dir: &Path,
    worktree_dir: &Path,
    caps: CowCaps,
) -> Result<ReflinkResult, CopyStop> {
    let from = utf8_path(anchor_dir, "Source")?;
    let to = utf8_path(worktree_dir, "Destination")?;
    let is_link = anchor_dir
        .symlink_metadata()
        .map_err(|e| format!("Cannot read '{from}': {e}"))?
        .file_type()
        .is_symlink();
    if is_link {
        return Err(CopyStop::from(
            "skipped: the anchor's target is a link to a shared build directory, which a clone would not follow"
                .to_string(),
        ));
    }
    let _held =
        hold_cargo_build_locks(anchor_dir).map_err(|reason| format!("skipped: {reason}"))?;
    let rewrite = |staging: &Path| rewrite_build_outputs(staging, from, to);
    copy_dir_staged(
        anchor_dir,
        worktree_dir,
        caps,
        CopyRules {
            require_clone: true,
            finish: Some(&rewrite),
        },
    )
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
        let refused = reflink_copy_dir_bounded(&src, &dst, shallow, CopyRules::default());
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
        let error = fallback_reflink_copy(&src, &dst, shallow, false)
            .map(|tally| tally.files)
            .expect_err("past the bound");
        assert!(error.contains("12 directories deep"), "{error}");
        let whole = temp.path().join("whole");
        let tally = fallback_reflink_copy(&src, &whole, CowCaps::DEFAULT, false).expect("copied");
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
        let error = reflink_copy_dir_bounded(&src, &dst, few, CopyRules::default())
            .expect_err("past the cap");
        assert!(error.contains("more than 4 entries"), "{error}");
        assert_eq!(entries(temp.path()), ["src"]);

        let walk = temp.path().join("walk");
        let error = fallback_reflink_copy(&src, &walk, few, false)
            .map(|tally| tally.files)
            .expect_err("past the cap");
        assert!(error.contains("more than 4 entries"), "{error}");

        let roomy = CowCaps {
            max_entries: 6,
            ..CowCaps::DEFAULT
        };
        let result = reflink_copy_dir_bounded(&src, &dst, roomy, CopyRules::default())
            .expect("within the cap");
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

    /// An anchor whose ignored `target/` holds one build script's recorded
    /// output, written the way cargo writes it: absolute paths into the
    /// anchor's own target directory. Returns the anchor (canonical), the
    /// worktree, and the script's directory relative to `target/`.
    fn anchor_with_cargo_target() -> (
        tempfile::TempDir,
        tempfile::TempDir,
        PathBuf,
        PathBuf,
        PathBuf,
    ) {
        let (main, parent, worktree) = anchor_and_worktree(&["target", "node_modules"]);
        let anchor = main.path().canonicalize().unwrap();
        let script = PathBuf::from("debug/build/ring-0f3a9c");
        let out = anchor.join("target").join(&script).join("out");
        fs::create_dir_all(&out).unwrap();
        fs::write(out.join("libring_core.a"), "archive").unwrap();
        let out_text = out.to_str().unwrap();
        fs::write(
            anchor.join("target").join(&script).join("output"),
            format!(
                "cargo:rustc-link-lib=static=ring_core\ncargo:rustc-link-search=native={out_text}\n"
            ),
        )
        .unwrap();
        fs::write(
            anchor.join("target").join(&script).join("root-output"),
            out_text,
        )
        .unwrap();
        fs::create_dir_all(anchor.join("target/debug/deps")).unwrap();
        fs::write(anchor.join("target/debug/deps/libserde-1a2b.rlib"), "rlib").unwrap();
        fs::create_dir_all(anchor.join("node_modules/pkg")).unwrap();
        fs::write(anchor.join("node_modules/pkg/index.js"), "i").unwrap();
        (main, parent, anchor, worktree, script)
    }

    /// A cloned `target/` reuses the anchor's compiled dependencies, but
    /// cargo never reruns a registry build script, so the paths it recorded
    /// must name the clone's own out directories — not the anchor's, which
    /// a `cargo clean` there would pull out from under the worktree.
    #[test]
    fn a_cloned_cargo_target_records_its_own_paths_not_the_anchors() {
        let (_main, _parent, anchor, worktree, script) = anchor_with_cargo_target();
        let copied =
            reflink_ignored_caches(&anchor, &worktree, &CacheSelection::default()).expect("synced");
        assert!(
            copied
                .iter()
                .any(|r| Path::new(&r.destination).file_name() == Some("target".as_ref())),
            "{copied:?}"
        );
        assert!(worktree
            .join("target/debug/deps/libserde-1a2b.rlib")
            .is_file());
        let anchor_target = anchor.join("target");
        let worktree_target = worktree.join("target");
        for name in ["output", "root-output"] {
            let cloned = fs::read_to_string(worktree_target.join(&script).join(name)).unwrap();
            assert_eq!(
                cloned.matches(anchor_target.to_str().unwrap()).count(),
                0,
                "{name} still names the anchor: {cloned}"
            );
            assert!(
                cloned.contains(worktree_target.join(&script).join("out").to_str().unwrap()),
                "{name}: {cloned}"
            );
            let original = fs::read_to_string(anchor_target.join(&script).join(name)).unwrap();
            assert!(
                original.contains(anchor_target.to_str().unwrap()),
                "the anchor's own was rewritten"
            );
        }
    }

    /// A build in progress in the anchor holds `target/debug/.cargo-lock`.
    /// The sync neither waits for it nor clones a half-written tree: target
    /// is skipped and the reason reported, and the other caches still land.
    #[test]
    fn a_cargo_target_mid_build_is_skipped_with_the_reason_and_without_waiting() {
        let (_main, _parent, anchor, worktree, _script) = anchor_with_cargo_target();
        let lock_path = anchor.join("target/debug/.cargo-lock");
        fs::write(&lock_path, "").unwrap();
        let held = File::open(&lock_path).unwrap();
        held.lock().unwrap();
        let started = Instant::now();
        let synced = reflink_ignored_caches(&anchor, &worktree, &CacheSelection::default());
        let waited = started.elapsed();
        held.unlock().unwrap();
        let error = synced.expect_err("a skipped target/ must be reported");
        assert!(error.contains("target"), "{error}");
        assert!(error.contains("anchor build in progress"), "{error}");
        assert!(
            waited < std::time::Duration::from_secs(10),
            "waited {waited:?}"
        );
        assert!(!worktree.join("target").exists());
        assert!(worktree.join("node_modules/pkg/index.js").is_file());

        // Released, the next sync clones it.
        reflink_ignored_caches(&anchor, &worktree, &CacheSelection::default()).expect("synced");
        assert!(worktree
            .join("target/debug/deps/libserde-1a2b.rlib")
            .is_file());
    }

    /// A recorded build output that is not UTF-8 cannot be rewritten
    /// byte-exactly, so the target/ clone fails whole and nothing is left.
    #[test]
    fn an_unrewritable_build_output_fails_the_target_clone_whole() {
        let (_main, _parent, anchor, worktree, script) = anchor_with_cargo_target();
        fs::write(
            anchor.join("target").join(&script).join("output"),
            b"cargo:x=\xff\n",
        )
        .unwrap();
        let error = reflink_ignored_caches(&anchor, &worktree, &CacheSelection::default())
            .expect_err("an output that could not be rewritten was cloned");
        assert!(error.contains("target"), "{error}");
        assert!(!worktree.join("target").exists());
        assert!(worktree.join("node_modules/pkg/index.js").is_file());
        assert_eq!(
            entries(&worktree)
                .iter()
                .filter(|name| name.contains("gitpulse-partial"))
                .count(),
            0,
            "a staging tree was left"
        );
    }

    /// Where nothing can be cloned (Windows, ext4, a cross-device FICLONE),
    /// target/ is skipped with the reason rather than byte-copied, and the
    /// other caches keep their byte-copy fallback.
    #[test]
    fn a_cargo_target_that_could_only_be_byte_copied_is_skipped_with_the_reason() {
        let (_main, _parent, anchor, worktree, _script) = anchor_with_cargo_target();
        let synced = with_forced_byte_copy(|| {
            reflink_ignored_caches(&anchor, &worktree, &CacheSelection::default())
        });
        let error = synced.expect_err("a skipped target/ must be reported");
        assert!(error.contains("target: "), "{error}");
        assert!(error.contains("never byte-copied"), "{error}");
        assert!(error.contains("copied: node_modules"), "{error}");
        assert!(!worktree.join("target").exists());
        assert!(worktree.join("node_modules/pkg/index.js").is_file());
        assert_eq!(
            entries(&worktree)
                .iter()
                .filter(|name| name.contains("gitpulse-partial"))
                .count(),
            0,
            "a staging tree was left"
        );
    }

    /// A target directory that is a link (a shared `CARGO_TARGET_DIR`) is not
    /// cloned: the clone would be the link, and the worktree would build in
    /// the anchor's directory.
    #[cfg(unix)]
    #[test]
    fn a_linked_cargo_target_is_skipped_with_the_reason() {
        let (main, _parent, worktree) = anchor_and_worktree(&["target"]);
        let anchor = main.path().canonicalize().unwrap();
        let shared = tempdir().unwrap();
        fs::create_dir_all(shared.path().join("debug")).unwrap();
        std::os::unix::fs::symlink(shared.path(), anchor.join("target")).unwrap();
        let error = reflink_ignored_caches(&anchor, &worktree, &CacheSelection::default())
            .expect_err("a skipped target/ must be reported");
        assert!(error.contains("target: skipped"), "{error}");
        assert!(worktree.join("target").symlink_metadata().is_err());
    }

    #[test]
    fn only_whole_path_prefixes_are_rewritten() {
        let from = "/w/a/target";
        let to = "/w/b/target";
        assert_eq!(
            replace_path_prefix(
                "x=/w/a/target/debug/out\ny=/w/a/target2/z\nroot=/w/a/target\n/w/a/target",
                from,
                to
            ),
            "x=/w/b/target/debug/out\ny=/w/a/target2/z\nroot=/w/b/target\n/w/b/target"
        );
        assert_eq!(
            replace_path_prefix("nothing here", from, to),
            "nothing here"
        );
    }

    /// target/ passes the same ignore check as every other cache.
    #[test]
    fn a_cargo_target_git_does_not_ignore_is_not_cloned() {
        let (main, _parent, worktree) = anchor_and_worktree(&["node_modules"]);
        let anchor = main.path().canonicalize().unwrap();
        fs::create_dir_all(anchor.join("target/debug/deps")).unwrap();
        fs::write(anchor.join("target/debug/deps/liba.rlib"), "rlib").unwrap();
        let copied =
            reflink_ignored_caches(&anchor, &worktree, &CacheSelection::default()).expect("synced");
        assert!(copied.is_empty(), "{copied:?}");
        assert!(!worktree.join("target").exists());
    }

    fn roomy(_: &Path) -> Reading<u64> {
        Reading::Value(LOW_DISK_FLOOR + (400 << 30))
    }
    fn full(_: &Path) -> Reading<u64> {
        Reading::Value(LOW_DISK_FLOOR - 1)
    }
    fn tight(_: &Path) -> Reading<u64> {
        // Room for 4 bytes above the floor.
        Reading::Value(LOW_DISK_FLOOR + 4)
    }
    fn unreadable(_: &Path) -> Reading<u64> {
        Reading::Unavailable(crate::sysload::Why::Failed)
    }

    fn cache_with_file(anchor: &Path, bytes: usize) {
        fs::create_dir_all(anchor.join("node_modules/pkg")).unwrap();
        fs::write(anchor.join("node_modules/pkg/index.js"), vec![b'x'; bytes]).unwrap();
    }

    fn selection(free_space: FreeSpace) -> CacheSelection {
        CacheSelection {
            free_space,
            ..CacheSelection::default()
        }
    }

    /// Below the floor nothing is copied and nothing is left behind; the skip
    /// is typed and is not a failure.
    #[test]
    fn a_volume_below_the_floor_gets_no_copy_and_a_typed_low_disk() {
        let (main, _parent, worktree) = anchor_and_worktree(&["node_modules"]);
        let anchor = main.path().canonicalize().unwrap();
        cache_with_file(&anchor, 8);
        let report = clone_ignored_caches(&anchor, &worktree, &selection(full)).unwrap();
        assert!(
            report.copied.is_empty() && report.failures.is_empty(),
            "{report:?}"
        );
        let low = report.low_disk.expect("a typed low_disk");
        assert_eq!(low.cause, LowDiskCause::BelowFloor);
        assert_eq!(low.free_bytes, Some(LOW_DISK_FLOOR - 1));
        assert_eq!(low.skipped, vec!["node_modules".to_string()]);
        assert!(!worktree.join("node_modules").exists());
        assert!(entries(&worktree).iter().all(|n| !n.contains("partial")));
    }

    #[test]
    fn plenty_of_space_copies_and_reports_no_low_disk() {
        let (main, _parent, worktree) = anchor_and_worktree(&["node_modules"]);
        let anchor = main.path().canonicalize().unwrap();
        cache_with_file(&anchor, 8);
        let report = clone_ignored_caches(&anchor, &worktree, &selection(roomy)).unwrap();
        assert_eq!(report.copied.len(), 1);
        assert_eq!((report.low_disk, report.disk_unknown), (None, false));
        assert!(worktree.join("node_modules/pkg/index.js").is_file());
    }

    /// Unknown is its own state: the copy proceeds, and the report says the
    /// volume was not read rather than that it was fine.
    #[test]
    fn an_unreadable_volume_is_unknown_and_forces_no_skip() {
        let (main, _parent, worktree) = anchor_and_worktree(&["node_modules"]);
        let anchor = main.path().canonicalize().unwrap();
        cache_with_file(&anchor, 8);
        let report = clone_ignored_caches(&anchor, &worktree, &selection(unreadable)).unwrap();
        assert_eq!(report.copied.len(), 1, "{report:?}");
        assert!(report.low_disk.is_none());
        assert!(report.disk_unknown);
    }

    /// A byte-copied tree past the cap is not copied at all, the cap is
    /// named, and nothing is left behind.
    #[test]
    fn the_byte_cap_is_honoured_whole_or_nothing() {
        let (main, _parent, worktree) = anchor_and_worktree(&["node_modules"]);
        let anchor = main.path().canonicalize().unwrap();
        cache_with_file(&anchor, 100);
        let capped = CacheSelection {
            caps: CowCaps {
                max_bytes: 10,
                ..CowCaps::DEFAULT
            },
            free_space: roomy,
            ..CacheSelection::default()
        };
        let report =
            with_forced_byte_copy(|| clone_ignored_caches(&anchor, &worktree, &capped)).unwrap();
        assert!(
            report.copied.is_empty() && report.failures.is_empty(),
            "{report:?}"
        );
        let low = report.low_disk.expect("a typed low_disk");
        assert_eq!(low.cause, LowDiskCause::ByteCap);
        assert_eq!(low.cap_bytes, Some(10));
        assert!(low.message().contains("byte cap"));
        assert!(!worktree.join("node_modules").exists());
        assert!(entries(&worktree).iter().all(|n| !n.contains("partial")));
        // Under the cap the same tree copies.
        let ok =
            with_forced_byte_copy(|| clone_ignored_caches(&anchor, &worktree, &selection(roomy)))
                .unwrap();
        assert_eq!(ok.copied.len(), 1);
    }

    /// The cap is a typed stop, not state beside the copy: the same result
    /// comes back when the copy runs on a spawned thread.
    #[test]
    fn the_byte_cap_is_detected_when_the_copy_runs_on_a_spawned_thread() {
        let (main, _parent, worktree) = anchor_and_worktree(&["node_modules"]);
        let anchor = main.path().canonicalize().unwrap();
        cache_with_file(&anchor, 100);
        let capped = CacheSelection {
            caps: CowCaps {
                max_bytes: 10,
                ..CowCaps::DEFAULT
            },
            free_space: roomy,
            ..CacheSelection::default()
        };
        let report = std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    with_forced_byte_copy(|| clone_ignored_caches(&anchor, &worktree, &capped))
                })
                .join()
                .unwrap()
        })
        .unwrap();
        assert!(report.failures.is_empty(), "{report:?}");
        let low = report.low_disk.expect("a typed low_disk");
        assert_eq!(low.cause, LowDiskCause::ByteCap);
    }

    /// A copy that would leave the volume below the floor is skipped too,
    /// and says the floor, not the cap, bound it.
    #[test]
    fn a_copy_that_would_cross_the_floor_is_skipped() {
        let (main, _parent, worktree) = anchor_and_worktree(&["node_modules"]);
        let anchor = main.path().canonicalize().unwrap();
        cache_with_file(&anchor, 100);
        let report =
            with_forced_byte_copy(|| clone_ignored_caches(&anchor, &worktree, &selection(tight)))
                .unwrap();
        let low = report.low_disk.expect("a typed low_disk");
        assert_eq!(low.cause, LowDiskCause::BelowFloor);
        assert!(!worktree.join("node_modules").exists());
    }

    /// The legacy entry point reports a skip as an error, so a caller that
    /// only reads `Result` still hears it.
    #[test]
    fn the_result_entry_point_does_not_hide_a_low_disk_skip() {
        let (main, _parent, worktree) = anchor_and_worktree(&["node_modules"]);
        let anchor = main.path().canonicalize().unwrap();
        cache_with_file(&anchor, 8);
        let error = reflink_ignored_caches(&anchor, &worktree, &selection(full)).unwrap_err();
        assert!(error.contains("low disk"), "{error}");
    }

    // ---- adversarial W4-L3 (adv-w4l3) ----

    fn staging_leftovers(dir: &Path) -> Vec<String> {
        entries(dir)
            .into_iter()
            .filter(|n| n.contains("gitpulse-partial"))
            .collect()
    }

    /// Attack 2: a panic mid-copy (here in the finish step) must not leave a
    /// staging directory beside the destination.
    #[test]
    fn adv_a_panic_mid_copy_leaves_no_staging_dir() {
        let parent = tempdir().unwrap();
        let src = parent.path().join("src");
        fs::create_dir_all(src.join("pkg")).unwrap();
        fs::write(src.join("pkg/a.js"), "a").unwrap();
        let dst = parent.path().join("dst");
        let boom = |_: &Path| -> Result<(), String> { panic!("crash mid-copy") };
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            copy_dir_staged(
                &src,
                &dst,
                CowCaps::DEFAULT,
                CopyRules {
                    require_clone: false,
                    finish: Some(&boom),
                },
            )
        }));
        assert!(outcome.is_err());
        assert!(!dst.exists());
        assert_eq!(staging_leftovers(parent.path()), Vec::<String>::new());
    }

    /// Attack 2: a cap hit after many files were already written leaves
    /// neither destination nor staging.
    #[test]
    fn adv_a_cap_hit_after_many_files_leaves_nothing() {
        let parent = tempdir().unwrap();
        let src = parent.path().join("src");
        fs::create_dir_all(src.join("d")).unwrap();
        for i in 0..40 {
            fs::write(src.join("d").join(format!("f{i}")), [b'x'; 10]).unwrap();
        }
        let dst = parent.path().join("dst");
        let caps = CowCaps {
            max_bytes: 250,
            ..CowCaps::DEFAULT
        };
        let r = with_forced_byte_copy(|| copy_dir_staged(&src, &dst, caps, CopyRules::default()));
        assert!(matches!(r, Err(CopyStop::CapHit { .. })), "{r:?}");
        assert!(!dst.exists());
        assert_eq!(staging_leftovers(parent.path()), Vec::<String>::new());
    }

    /// Attack 2: an entry that is neither file, dir nor link (a FIFO) fails
    /// the copy after earlier files were written; nothing stays.
    #[cfg(unix)]
    #[test]
    fn adv_an_error_after_partial_progress_leaves_nothing() {
        let parent = tempdir().unwrap();
        let src = parent.path().join("src");
        fs::create_dir_all(&src).unwrap();
        for i in 0..5 {
            fs::write(src.join(format!("f{i}")), "x").unwrap();
        }
        let fifo = std::ffi::CString::new(src.join("zz-fifo").to_str().unwrap()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        let dst = parent.path().join("dst");
        let r = with_forced_byte_copy(|| {
            copy_dir_staged(&src, &dst, CowCaps::DEFAULT, CopyRules::default())
        });
        assert!(r.is_err());
        assert!(!dst.exists());
        assert_eq!(staging_leftovers(parent.path()), Vec::<String>::new());
    }

    /// Attack 2: eight provisions race on the SAME destination (so the same
    /// parent): at least one wins, the winner is complete, nothing is left.
    #[test]
    fn adv_racing_copies_to_one_destination_leave_one_whole_copy_and_no_staging() {
        let parent = tempdir().unwrap();
        let src = parent.path().join("src");
        fs::create_dir_all(src.join("d")).unwrap();
        for i in 0..200 {
            fs::write(src.join("d").join(format!("f{i}")), [b'x'; 64]).unwrap();
        }
        let dst = parent.path().join("dst");
        let results: Vec<_> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..8)
                .map(|_| {
                    scope.spawn(|| {
                        with_forced_byte_copy(|| {
                            copy_dir_staged(&src, &dst, CowCaps::DEFAULT, CopyRules::default())
                        })
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        assert!(
            results.iter().filter(|r| r.is_ok()).count() >= 1,
            "{results:?}"
        );
        assert_eq!(fs::read_dir(dst.join("d")).unwrap().count(), 200);
        assert_eq!(staging_leftovers(parent.path()), Vec::<String>::new());
    }

    /// Attack 3, re-pointed when the cap became allocated-size accounting:
    /// 2000 one-byte files occupy a block each (megabytes on disk), so a
    /// 2000-byte cap REFUSES them, whole-or-nothing, rather than admitting a
    /// copy that is far over it.
    #[cfg(unix)]
    #[test]
    fn adv_the_cap_counts_physical_bytes_not_logical_length() {
        let parent = tempdir().unwrap();
        let src = parent.path().join("src");
        fs::create_dir_all(&src).unwrap();
        for i in 0..2000 {
            fs::write(src.join(format!("f{i}")), "x").unwrap();
        }
        let dst = parent.path().join("dst");
        let caps = CowCaps {
            max_bytes: 2000,
            ..CowCaps::DEFAULT
        };
        let r = with_forced_byte_copy(|| copy_dir_staged(&src, &dst, caps, CopyRules::default()));
        assert!(matches!(r, Err(CopyStop::CapHit { cap: 2000 })), "{r:?}");
        assert!(!dst.exists());
        assert_eq!(staging_leftovers(parent.path()), Vec::<String>::new());
    }

    /// Attack 3, re-pointed to allocated units: hard links are each written,
    /// so each name is counted. A cap just above two blocks admits the two
    /// small names; a cap of one block refuses them.
    #[cfg(unix)]
    #[test]
    fn adv_hard_links_are_counted_per_name() {
        use std::os::unix::fs::MetadataExt;
        let parent = tempdir().unwrap();
        let src = parent.path().join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("a"), [b'x'; 100]).unwrap();
        fs::hard_link(src.join("a"), src.join("b")).unwrap();
        let one_name = allocated(&fs::metadata(src.join("a")).unwrap());
        let tight = CowCaps {
            max_bytes: one_name,
            ..CowCaps::DEFAULT
        };
        let d1 = parent.path().join("d1");
        let r = with_forced_byte_copy(|| copy_dir_staged(&src, &d1, tight, CopyRules::default()));
        assert!(matches!(r, Err(CopyStop::CapHit { .. })), "{r:?}");
        assert!(!d1.exists());
        let roomy = CowCaps {
            max_bytes: 2 * one_name,
            ..CowCaps::DEFAULT
        };
        let d2 = parent.path().join("d2");
        with_forced_byte_copy(|| copy_dir_staged(&src, &d2, roomy, CopyRules::default())).unwrap();
        assert_ne!(
            fs::metadata(d2.join("a")).unwrap().ino(),
            fs::metadata(d2.join("b")).unwrap().ino()
        );
    }

    /// Attack 3: a sparse file is counted by logical length (over-counts, the
    /// safe direction), so a sparse 64 MiB file trips a 10 MiB cap.
    #[cfg(unix)]
    #[test]
    fn adv_a_sparse_file_is_counted_by_logical_length() {
        let parent = tempdir().unwrap();
        let src = parent.path().join("src");
        fs::create_dir_all(&src).unwrap();
        fs::File::create(src.join("s"))
            .unwrap()
            .set_len(64 << 20)
            .unwrap();
        let caps = CowCaps {
            max_bytes: 10 << 20,
            ..CowCaps::DEFAULT
        };
        let r = with_forced_byte_copy(|| {
            copy_dir_staged(&src, &parent.path().join("d"), caps, CopyRules::default())
        });
        assert!(matches!(r, Err(CopyStop::CapHit { .. })), "{r:?}");
    }

    /// Attack 3: symlinks (absolute, relative-escaping, to a directory) are
    /// copied as links: nothing outside the destination is written or read
    /// into the copy, and the link adds no bytes.
    #[cfg(unix)]
    #[test]
    fn adv_symlinks_in_a_cache_are_not_followed_and_nothing_escapes() {
        let parent = tempdir().unwrap();
        let outside = parent.path().join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("secret"), [b'x'; 4096]).unwrap();
        let src = parent.path().join("src");
        fs::create_dir_all(src.join("sub")).unwrap();
        std::os::unix::fs::symlink(&outside, src.join("abs")).unwrap();
        std::os::unix::fs::symlink("../outside", src.join("sub/rel")).unwrap();
        std::os::unix::fs::symlink("sub", src.join("selfdir")).unwrap();
        let dst = parent.path().join("dst");
        let caps = CowCaps {
            max_bytes: 10,
            ..CowCaps::DEFAULT
        };
        with_forced_byte_copy(|| copy_dir_staged(&src, &dst, caps, CopyRules::default())).unwrap();
        assert!(dst
            .join("abs")
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(entries(&outside), vec!["secret".to_string()]);
        assert_eq!(fs::read_dir(&dst).unwrap().count(), 3);
    }

    /// Attack 3: a cache directory that is itself a symlink to an outside
    /// tree is walked through (followed) as the cache root, still capped.
    #[cfg(unix)]
    #[test]
    fn adv_a_symlinked_cache_root_is_followed_and_still_capped() {
        let parent = tempdir().unwrap();
        let outside = parent.path().join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("big"), [b'x'; 100]).unwrap();
        let src = parent.path().join("node_modules");
        std::os::unix::fs::symlink(&outside, &src).unwrap();
        let caps = CowCaps {
            max_bytes: 10,
            ..CowCaps::DEFAULT
        };
        let r = with_forced_byte_copy(|| {
            copy_dir_staged(&src, &parent.path().join("d"), caps, CopyRules::default())
        });
        assert!(matches!(r, Err(CopyStop::CapHit { .. })), "{r:?}");
    }

    /// Attack 4: the free-space read is one snapshot per cache. A volume
    /// that fills after that read is never re-checked: the reader answers
    /// roomy once, then full, and the copy still runs to the end.
    #[test]
    fn adv_free_space_is_rechecked_during_a_copy() {
        use std::sync::atomic::AtomicUsize;
        static CALLS: AtomicUsize = AtomicUsize::new(0);
        fn fills_after_first_read(_: &Path) -> Reading<u64> {
            if CALLS.fetch_add(1, Ordering::SeqCst) == 0 {
                Reading::Value(LOW_DISK_FLOOR + 10_000)
            } else {
                Reading::Value(0)
            }
        }
        let (main, _parent, worktree) = anchor_and_worktree(&["node_modules"]);
        let anchor = main.path().canonicalize().unwrap();
        fs::create_dir_all(anchor.join("node_modules/p")).unwrap();
        for i in 0..50 {
            fs::write(anchor.join(format!("node_modules/p/f{i}")), [b'x'; 100]).unwrap();
        }
        let report = with_forced_byte_copy(|| {
            clone_ignored_caches(&anchor, &worktree, &selection(fills_after_first_read))
        })
        .unwrap();
        assert!(
            report.copied.is_empty(),
            "the copy ran to the end after the volume filled: free space is read once, not re-checked"
        );
    }

    /// Attack 4: lanes provisioning at once each read the same headroom and
    /// each spend all of it: 3 lanes x 800 B against 1000 B above the floor.
    #[test]
    fn adv_concurrent_lanes_cannot_jointly_cross_the_floor() {
        use std::sync::atomic::AtomicUsize;
        static ARRIVED: AtomicUsize = AtomicUsize::new(0);
        fn rendezvous(_: &Path) -> Reading<u64> {
            ARRIVED.fetch_add(1, Ordering::SeqCst);
            for _ in 0..50_000_000u64 {
                if ARRIVED.load(Ordering::SeqCst) >= 3 {
                    break;
                }
                std::thread::yield_now();
            }
            Reading::Value(LOW_DISK_FLOOR + 1000)
        }
        let lanes: Vec<_> = (0..3)
            .map(|_| anchor_and_worktree(&["node_modules"]))
            .collect();
        let written: u64 = std::thread::scope(|scope| {
            let handles: Vec<_> = lanes
                .iter()
                .map(|(main, _p, worktree)| {
                    scope.spawn(move || {
                        let anchor = main.path().canonicalize().unwrap();
                        cache_with_file(&anchor, 800);
                        with_forced_byte_copy(|| {
                            clone_ignored_caches(&anchor, worktree, &selection(rendezvous))
                        })
                        .unwrap()
                        .copied
                        .iter()
                        .map(|r| r.bytes_cloned)
                        .sum::<u64>()
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).sum()
        });
        assert!(
            written <= 1000,
            "{written} B written against 1000 B of headroom"
        );
    }

    // ---- robustness and accounting (W4-L3 adversarial follow-up) ----

    /// A file that grew after it was measured is cut at the measured size.
    #[test]
    fn a_file_is_copied_only_up_to_the_size_it_was_measured_at() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("grown"), [b'x'; 100]).unwrap();
        copy_bounded(&dir.path().join("grown"), &dir.path().join("out"), 10).unwrap();
        assert_eq!(fs::metadata(dir.path().join("out")).unwrap().len(), 10);
    }

    /// Small files are charged what they occupy, so a cap in bytes is a cap
    /// on disk: two 1-byte files do not fit a cap of 2 bytes.
    #[cfg(unix)]
    #[test]
    fn small_files_are_charged_their_allocated_size() {
        let parent = tempdir().unwrap();
        let src = parent.path().join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("a"), "x").unwrap();
        fs::write(src.join("b"), "x").unwrap();
        let caps = CowCaps {
            max_bytes: 2,
            ..CowCaps::DEFAULT
        };
        let r = with_forced_byte_copy(|| {
            copy_dir_staged(&src, &parent.path().join("d"), caps, CopyRules::default())
        });
        assert!(matches!(r, Err(CopyStop::CapHit { .. })), "{r:?}");
    }

    /// Free space is read again as a long copy proceeds: a volume that fills
    /// after the first read ends the copy, whole-or-nothing.
    #[test]
    fn a_volume_that_fills_during_a_long_copy_ends_it_with_nothing_left() {
        use std::sync::atomic::AtomicUsize;
        static CALLS: AtomicUsize = AtomicUsize::new(0);
        fn fills_after_first_read(_: &Path) -> Reading<u64> {
            if CALLS.fetch_add(1, Ordering::SeqCst) == 0 {
                Reading::Value(LOW_DISK_FLOOR + (4 << 30))
            } else {
                Reading::Value(0)
            }
        }
        let (main, _parent, worktree) = anchor_and_worktree(&["node_modules"]);
        let anchor = main.path().canonicalize().unwrap();
        fs::create_dir_all(anchor.join("node_modules/p")).unwrap();
        let mib = vec![b'x'; 1 << 20];
        for i in 0..70 {
            fs::write(anchor.join(format!("node_modules/p/f{i}")), &mib).unwrap();
        }
        let report = with_forced_byte_copy(|| {
            clone_ignored_caches(&anchor, &worktree, &selection(fills_after_first_read))
        })
        .unwrap();
        assert!(
            report.copied.is_empty() && report.failures.is_empty(),
            "{report:?}"
        );
        let low = report.low_disk.expect("a typed low_disk");
        assert_eq!(low.cause, LowDiskCause::BelowFloor);
        assert_eq!(low.free_bytes, Some(0));
        assert!(!worktree.join("node_modules").exists());
        assert!(staging_leftovers(&worktree).is_empty());
    }

    /// A reader that panics is an unreadable volume: the copy proceeds and
    /// the report says unknown.
    #[test]
    fn a_panicking_reader_is_an_unknown_volume() {
        fn boom(_: &Path) -> Reading<u64> {
            panic!("reader blew up")
        }
        let (main, _parent, worktree) = anchor_and_worktree(&["node_modules"]);
        let anchor = main.path().canonicalize().unwrap();
        cache_with_file(&anchor, 8);
        let report = clone_ignored_caches(&anchor, &worktree, &selection(boom)).unwrap();
        assert!(
            report.disk_unknown && report.low_disk.is_none(),
            "{report:?}"
        );
        assert_eq!(report.copied.len(), 1);
    }

    /// `copyfile(3)` can byte-copy across volumes without saying so; what it
    /// could write is counted first, against the cap, with no destination
    /// made.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_macos_copyfile_path_is_held_to_the_cap_before_it_runs() {
        let parent = tempdir().unwrap();
        let src = parent.path().join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("big"), [b'x'; 100]).unwrap();
        let caps = CowCaps {
            max_bytes: 10,
            ..CowCaps::DEFAULT
        };
        let dst = parent.path().join("dst");
        let r = copy_dir_staged(&src, &dst, caps, CopyRules::default());
        assert!(matches!(r, Err(CopyStop::CapHit { .. })), "{r:?}");
        assert!(!dst.exists());
        assert!(staging_leftovers(parent.path()).is_empty());
    }
}
