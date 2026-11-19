//! The life of the per-sidecar scratch directory.
//!
//! [`super::sidecar`] gives every `manvi serve` child a private state directory,
//! `<temp>/gitpulse-harness-<pid>-<16 hex>/`. Two things end one:
//!
//! - the sidecar that made it, once its child is reaped ([`ScratchDir::remove`]);
//! - a later process's sweep ([`sweep_stale`]), for a directory whose owner never got
//!   to: a one-shot `gitpulse-hook` exits through `process::exit`, which runs no
//!   destructor, and a crash runs none either.
//!
//! The sweep removes only what it can prove is a dead process's own: the name
//! has the exact shape this code writes, the entry is a real directory (never a
//! symlink), this user owns it, and the pid in its name is gone. Anything it
//! cannot establish is kept.
//!
//! Two limits of the sweep, both deliberate:
//!
//! - It is unix-only. Off unix there is no owner id to read and no liveness
//!   probe, so nothing is proved and nothing is removed; what covers Windows is
//!   self-removal: [`ScratchDir::remove`] after the child is reaped, and the
//!   hook binary's shutdown of its sidecars before it exits.
//! - A pid is only meaningful inside its own pid namespace, and a temp root can
//!   be shared across namespaces (a container bind-mounting the host's `/tmp`).
//!   A directory whose owner lives in another namespace may name a pid that is
//!   free here, and is then swept as stale. The user-ownership check bounds the
//!   damage to the same user's own scratch state, which is recreated on the
//!   next spawn, but the sweep is not safe to point at a root shared with
//!   another namespace's live sidecars.

use std::fs::Metadata;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Every scratch directory's name starts with this.
pub(super) const SCRATCH_PREFIX: &str = "gitpulse-harness-";
/// Entries one sweep looks at. A temp root is shared with everything else the
/// user runs; the sweep must not become a walk of all of it.
pub(super) const MAX_SCANNED: usize = 4096;
/// Directories one sweep removes. The first spawn of a hook process pays for
/// it inside the hook's own deadline; the rest wait for the next process.
pub(super) const MAX_REMOVED: usize = 64;

/// A scratch directory this process made, removed once by its owner.
pub(super) struct ScratchDir {
    path: Mutex<Option<PathBuf>>,
}

impl ScratchDir {
    pub(super) fn new(path: PathBuf) -> Self {
        Self {
            path: Mutex::new(Some(path)),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<PathBuf>> {
        self.path
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The directory, while it has not been removed.
    pub(super) fn path(&self) -> Option<PathBuf> {
        self.lock().clone()
    }

    /// Removes the directory, once. Only called after whatever used it has been
    /// reaped. A failure is logged with the path and left to the next sweep.
    pub(super) fn remove(&self) {
        let Some(path) = self.lock().take() else {
            return;
        };
        if let Err(why) = remove_tree(&path) {
            log::warn!(target: "harness", "could not remove harness state dir {}: {why}", path.display());
        }
    }
}

/// Removes `dir` and what is under it, refusing a symlink or anything that is
/// not a directory at its root (`remove_dir_all` itself never follows a link
/// inside the tree). An entry already gone is not a failure.
pub(super) fn remove_tree(dir: &Path) -> Result<(), String> {
    let meta = match std::fs::symlink_metadata(dir) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.to_string()),
    };
    if !meta.file_type().is_dir() {
        return Err("it is not a directory (or is a symlink)".into());
    }
    match std::fs::remove_dir_all(dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

/// The pid in `gitpulse-harness-<pid>-<16 hex>`, or `None` for any other name.
/// Strict on purpose: it is the only evidence that a directory is ours.
pub(super) fn owner_pid(name: &str) -> Option<u32> {
    let (pid, hash) = name.strip_prefix(SCRATCH_PREFIX)?.split_once('-')?;
    // Exactly what `scratch_dir` writes: the pid with `{}` (so no sign, no
    // leading zeros) and the hash with `{:016x}` (sixteen lowercase digits).
    if hash.len() != 16 || !hash.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return None;
    }
    let parsed = pid.parse::<u32>().ok().filter(|pid| *pid > 0)?;
    (parsed.to_string() == pid).then_some(parsed)
}

/// Whether `meta` says `uid` owns the entry. Where there is no owner id to
/// read, nothing is proved, so it is not owned.
pub(super) fn owned_by(meta: &Metadata, uid: u32) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        meta.uid() == uid
    }
    #[cfg(not(unix))]
    {
        let _ = (meta, uid);
        false
    }
}

/// This process's effective user id.
pub(super) fn current_uid() -> u32 {
    #[cfg(unix)]
    {
        // SAFETY: `geteuid` reads this process's credentials and cannot fail.
        unsafe { libc::geteuid() }
    }
    #[cfg(not(unix))]
    {
        0
    }
}

/// Whether process `pid` exists. An answer that could not be had is "alive":
/// the cost of a wrong "alive" is a directory kept, of a wrong "gone" a live
/// sidecar's state removed.
pub(super) fn pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // A pid past i32 would reach `kill` as a negative number, which
        // addresses a process group.
        let Ok(pid) = i32::try_from(pid) else {
            return true;
        };
        // SAFETY: signal 0 sends nothing; it only asks whether `pid` is signalable.
        if unsafe { libc::kill(pid, 0) } == 0 {
            return true;
        }
        std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        true
    }
}

/// What one sweep did, with both limits' numbers so a capped sweep is never
/// read as a complete one.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Swept {
    pub scanned: usize,
    pub removed: usize,
    /// Stale-looking directories left (live pid, unproven, or removal failed).
    pub kept: usize,
    /// A cap stopped the sweep before the whole root was read.
    pub capped: bool,
}

/// Removes the stale scratch directories under `root`: names this code wrote
/// whose pid `alive` says is gone, that are real directories owned by `uid`.
/// A failure to read the root is logged and reported as an empty sweep: it
/// must never fail a spawn.
pub(super) fn sweep_stale(root: &Path, uid: u32, alive: impl Fn(u32) -> bool) -> Swept {
    let mut swept = Swept::default();
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) => {
            log::warn!(target: "harness", "could not scan {} for stale harness state dirs: {error}", root.display());
            return swept;
        }
    };
    for entry in entries {
        if swept.scanned >= MAX_SCANNED || swept.removed >= MAX_REMOVED {
            swept.capped = true;
            break;
        }
        swept.scanned += 1;
        let Ok(entry) = entry else { continue };
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(owner_pid) else {
            continue;
        };
        let path = entry.path();
        // symlink_metadata: never the target of a planted link.
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if !meta.file_type().is_dir() || !owned_by(&meta, uid) || alive(pid) {
            swept.kept += 1;
            continue;
        }
        match remove_tree(&path) {
            Ok(()) => swept.removed += 1,
            Err(why) => {
                swept.kept += 1;
                log::warn!(target: "harness", "kept stale harness state dir {}: {why}", path.display());
            }
        }
    }
    if swept.capped {
        log::warn!(target: "harness", "stale harness state sweep of {} stopped at its cap: {swept:?}", root.display());
    } else if swept.removed > 0 {
        log::info!(target: "harness", "removed {} stale harness state dir(s) from {}", swept.removed, root.display());
    }
    swept
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(tag: &str) -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix(&format!("scratch-{tag}-"))
            .tempdir()
            .expect("test root")
    }

    const DEAD: u32 = 4_000_001;
    const LIVE: u32 = 777;

    fn name(pid: u32) -> String {
        format!("{SCRATCH_PREFIX}{pid}-0123456789abcdef")
    }

    fn alive_only_live(pid: u32) -> bool {
        pid == LIVE
    }

    #[test]
    fn only_the_exact_shape_names_an_owner() {
        assert_eq!(owner_pid(&name(42)), Some(42));
        for bad in [
            "gitpulse-harness-alpha",
            "gitpulse-harness-42",
            "gitpulse-harness-42-xyz",
            "gitpulse-harness-42-0123456789abcdeg",
            "gitpulse-harness-0-0123456789abcdef",
            "gitpulse-harness--1-0123456789abcdef",
            "gitpulse-harness-42-0123456789abcdef-extra",
            "other-42-0123456789abcdef",
        ] {
            assert_eq!(owner_pid(bad), None, "{bad}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_dead_owners_directory_is_removed_and_everything_else_is_kept() {
        let root = root("sweep");
        let stale = root.path().join(name(DEAD));
        std::fs::create_dir(&stale).unwrap();
        std::fs::write(stale.join("state.db"), b"x").unwrap();
        let live = root.path().join(name(LIVE));
        std::fs::create_dir(&live).unwrap();
        let unrelated = root.path().join("gitpulse-harness-alpha");
        std::fs::create_dir(&unrelated).unwrap();
        let planted_target = root.path().join("precious");
        std::fs::create_dir(&planted_target).unwrap();
        std::fs::write(planted_target.join("keep"), b"x").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&planted_target, root.path().join(name(DEAD + 1))).unwrap();

        let swept = sweep_stale(root.path(), current_uid(), alive_only_live);

        assert!(!stale.exists(), "a dead owner's directory must go");
        assert!(live.is_dir(), "a live owner's directory must stay");
        assert!(unrelated.is_dir(), "a name that is not ours must stay");
        assert!(planted_target.join("keep").is_file());
        #[cfg(unix)]
        {
            assert!(std::fs::symlink_metadata(root.path().join(name(DEAD + 1)))
                .unwrap()
                .file_type()
                .is_symlink());
            assert_eq!(swept.kept, 2, "{swept:?}");
        }
        assert_eq!(swept.removed, 1, "{swept:?}");
        assert!(!swept.capped);
    }

    /// Foreign ownership cannot be made without root, so the sweep is given a
    /// uid that is not the directory's: it must read the owner from the entry
    /// and refuse, which is the same decision a foreign-owned directory takes.
    #[cfg(unix)]
    #[test]
    fn a_directory_owned_by_someone_else_is_kept() {
        let root = root("foreign");
        let stale = root.path().join(name(DEAD));
        std::fs::create_dir(&stale).unwrap();
        let meta = std::fs::symlink_metadata(&stale).unwrap();
        assert!(owned_by(&meta, current_uid()));
        assert!(!owned_by(&meta, current_uid().wrapping_add(1)));

        let swept = sweep_stale(root.path(), current_uid().wrapping_add(1), |_| false);
        assert!(stale.is_dir());
        assert_eq!((swept.removed, swept.kept), (0, 1));
    }

    #[cfg(unix)]
    #[test]
    fn a_sweep_is_bounded_and_says_so() {
        let root = root("cap");
        for i in 0..(MAX_REMOVED as u32 + 3) {
            std::fs::create_dir(root.path().join(name(DEAD + i))).unwrap();
        }
        let swept = sweep_stale(root.path(), current_uid(), |_| false);
        assert_eq!(swept.removed, MAX_REMOVED);
        assert!(swept.capped);
        let rest = std::fs::read_dir(root.path()).unwrap().count();
        assert_eq!(rest, 3);
    }

    #[test]
    fn an_unreadable_root_is_an_empty_sweep() {
        let swept = sweep_stale(Path::new("/definitely/not/here"), 0, |_| false);
        assert_eq!(swept, Swept::default());
    }

    #[cfg(unix)]
    #[test]
    fn pid_liveness_tells_this_process_from_a_gone_one() {
        assert!(pid_alive(std::process::id()));
        assert!(!pid_alive(DEAD));
        assert!(pid_alive(u32::MAX), "an unaddressable pid is never swept");
    }

    #[test]
    fn remove_tree_refuses_a_symlink_root_and_tolerates_absence() {
        let root = root("tree");
        let target = root.path().join("target");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("keep"), b"x").unwrap();
        #[cfg(unix)]
        {
            let link = root.path().join("link");
            std::os::unix::fs::symlink(&target, &link).unwrap();
            assert!(remove_tree(&link).is_err());
            assert!(target.join("keep").is_file());
        }
        assert!(remove_tree(&root.path().join("absent")).is_ok());
    }
}
