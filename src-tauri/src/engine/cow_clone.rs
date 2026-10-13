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

/// Work done on a finished staging tree before it is renamed into place.
type Finish<'a> = &'a dyn Fn(&Path) -> Result<(), String>;

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

/// [`reflink_copy_dir`] under `caps` and `rules`.
fn reflink_copy_dir_bounded(
    src: &Path,
    dst: &Path,
    caps: CowCaps,
    rules: CopyRules<'_>,
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
    let copied = clone_tree(src, &staging, caps, rules.require_clone).and_then(|tally| {
        if let Some(finish) = rules.finish {
            finish(&staging)?;
        }
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
    require_clone: bool,
) -> Result<Tally, String> {
    #[cfg(target_os = "macos")]
    if !byte_copy_forced() {
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
                return Err(format!(
                    "The clone failed and its partial copy at '{}' could not be removed: {e}",
                    staging.display()
                ));
            }
        }
        if require_clone {
            return Err(NOT_CLONED.to_string());
        }
    }

    fallback_reflink_copy(src, staging, caps, require_clone)
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

/// Recursive copier that attempts file-level FICLONE on Linux, or `fs::copy`
/// elsewhere. Every entry is copied or the copy fails: an unreadable
/// directory or file, a failed write, an entry that is none of file,
/// directory or link, or a bound reached is an error naming it. With
/// `require_clone`, a file that could not be cloned ends the copy.
fn fallback_reflink_copy(
    src: &Path,
    dst: &Path,
    caps: CowCaps,
    require_clone: bool,
) -> Result<Tally, String> {
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
                if try_clone_file(&entry_path, &target_path) {
                    tally.is_cow = true;
                } else if require_clone {
                    return Err(NOT_CLONED.to_string());
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
        let copied = if *dir_name == CARGO_TARGET_DIR {
            clone_cargo_target(&anchor_dir, &worktree_dir, selection.caps)
        } else {
            reflink_copy_dir_bounded(
                &anchor_dir,
                &worktree_dir,
                selection.caps,
                CopyRules::default(),
            )
        };
        match copied {
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
        "{} of the build caches were not copied: {}{copied}",
        failures.len(),
        failures.join("; ")
    ))
}

/// Clones the anchor's Cargo target directory: a real directory, under its
/// build locks, by clone only, with recorded build-script paths rewritten
/// to the destination before the copy is renamed into place.
fn clone_cargo_target(
    anchor_dir: &Path,
    worktree_dir: &Path,
    caps: CowCaps,
) -> Result<ReflinkResult, String> {
    let from = utf8_path(anchor_dir, "Source")?;
    let to = utf8_path(worktree_dir, "Destination")?;
    let is_link = anchor_dir
        .symlink_metadata()
        .map_err(|e| format!("Cannot read '{from}': {e}"))?
        .file_type()
        .is_symlink();
    if is_link {
        return Err(
            "skipped: the anchor's target is a link to a shared build directory, which a clone would not follow"
                .to_string(),
        );
    }
    let _held =
        hold_cargo_build_locks(anchor_dir).map_err(|reason| format!("skipped: {reason}"))?;
    let rewrite = |staging: &Path| rewrite_build_outputs(staging, from, to);
    reflink_copy_dir_bounded(
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
}
