//! The worktree family Git last authenticated, remembered across processes.
//!
//! `AdmittedPair::authenticate` proves a checkout is a registered worktree with
//! one `git worktree list`, and remembers the answer in memory for as long as
//! the repository's own metadata names the same checkouts. A hook is a fresh
//! process for every tool call an agent makes, so that memory is empty each
//! time and every Bash call and every edit in a repository that has a ledger
//! paid the spawn again (ft-da85cd55): enough, across a handful of agents, to
//! leave the shared spawn budget at zero.
//!
//! This keeps the same answer in a file, under the same rule: it is believed
//! only while the registry the caller reads from the repository's metadata
//! (no Git) equals the registry it was stored for, and only for a pair that is
//! among its members. A worktree added, removed, moved or pruned changes the
//! registry and misses. Trust and the common directory are checked by the
//! caller on every call, before this is asked.
//!
//! The file is an authentication result, so it is only read when it cannot
//! have been put there by anyone but this user: a directory this user owns
//! with no group or other access, and a regular file (never a symlink) this
//! user owns with the same. Anything else, unreadable, oversized, malformed or
//! for a different repository, is a miss and Git is asked. Every failure to
//! write is a miss on the next call, never an error. Unix only; elsewhere
//! nothing is remembered and Git is asked, as before.

use std::path::{Path, PathBuf};

/// A remembered family: the registry it holds for, and Git's listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Stored {
    pub registry: Vec<PathBuf>,
    /// Git's own listing, primary first.
    pub members: Vec<PathBuf>,
}

/// The file's layout. Bumped when it changes; any other value is a miss.
#[cfg(unix)]
const VERSION: u32 = 1;

/// Largest file read back. A real family is a few hundred bytes per worktree.
#[cfg(unix)]
const MAX_FILE_BYTES: u64 = 1 << 20;

#[cfg(unix)]
#[derive(serde::Serialize, serde::Deserialize)]
struct Record {
    version: u32,
    common: String,
    registry: Vec<String>,
    members: Vec<String>,
}

/// The directory the files live in, or `None` where nothing is remembered.
///
/// Under test it is whatever the test on this thread set, and otherwise none:
/// a test of the hook must neither read nor leave files in the developer's
/// temporary directory.
#[cfg(all(unix, not(test)))]
fn directory() -> Option<PathBuf> {
    // SAFETY: getuid has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    Some(std::env::temp_dir().join(format!("gitpulse-worktree-families-{uid}")))
}

#[cfg(all(unix, test))]
fn directory() -> Option<PathBuf> {
    test_dir::get()
}

#[cfg(not(unix))]
fn directory() -> Option<PathBuf> {
    None
}

#[cfg(all(test, unix))]
pub(crate) mod test_dir {
    use std::cell::RefCell;
    use std::path::{Path, PathBuf};

    thread_local! {
        static DIR: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
    }

    pub(super) fn get() -> Option<PathBuf> {
        DIR.with(|dir| dir.borrow().clone())
    }

    /// Ends the remembering [`set`] began, on drop.
    pub(crate) struct Reset;

    impl Drop for Reset {
        fn drop(&mut self) {
            DIR.with(|dir| *dir.borrow_mut() = None);
        }
    }

    /// Remembers families in `dir`, on this thread, until the guard drops.
    pub(crate) fn set(dir: &Path) -> Reset {
        DIR.with(|slot| *slot.borrow_mut() = Some(dir.to_path_buf()));
        Reset
    }
}

/// The remembered family for `common`, if there is a trustworthy one.
pub(super) fn load(common: &Path) -> Option<Stored> {
    load_in(&directory()?, common)
}

/// Remembers `members` for `common` and the `registry` they hold for.
pub(super) fn store(common: &Path, registry: &[PathBuf], members: &[PathBuf]) {
    if let Some(dir) = directory() {
        store_in(&dir, common, registry, members);
    }
}

#[cfg(unix)]
fn file_name(common: &str) -> String {
    // FNV-1a: stable across builds, so the app and the hook find one file.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in common.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}.json")
}

#[cfg(unix)]
fn text(path: &Path) -> Option<&str> {
    path.to_str()
}

/// Whether `meta` is a thing only this user can have made or changed.
#[cfg(unix)]
fn private_to_this_user(meta: &std::fs::Metadata, want_dir: bool) -> bool {
    use std::os::unix::fs::MetadataExt;
    // SAFETY: getuid has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    let kind = if want_dir {
        meta.is_dir()
    } else {
        meta.is_file()
    };
    kind && meta.uid() == uid && meta.mode() & 0o077 == 0
}

#[cfg(unix)]
fn load_in(dir: &Path, common: &Path) -> Option<Stored> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;

    let common = text(common)?;
    if !private_to_this_user(&std::fs::symlink_metadata(dir).ok()?, true) {
        return None;
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        // Never follow a link, and never block on a FIFO or device planted at
        // the name: the kind is read from the opened descriptor (no gap between
        // check and use) before a byte of it is read.
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(dir.join(file_name(common)))
        .ok()?;
    let meta = file.metadata().ok()?;
    if !private_to_this_user(&meta, false) || meta.len() > MAX_FILE_BYTES {
        return None;
    }
    let mut raw = Vec::new();
    file.take(MAX_FILE_BYTES + 1).read_to_end(&mut raw).ok()?;
    let record: Record = serde_json::from_slice(&raw).ok()?;
    if record.version != VERSION || record.common != common {
        return None;
    }
    Some(Stored {
        registry: record.registry.into_iter().map(PathBuf::from).collect(),
        members: record.members.into_iter().map(PathBuf::from).collect(),
    })
}

#[cfg(not(unix))]
fn load_in(_dir: &Path, _common: &Path) -> Option<Stored> {
    None
}

#[cfg(unix)]
fn store_in(dir: &Path, common: &Path, registry: &[PathBuf], members: &[PathBuf]) {
    use std::io::Write;
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};

    // A path that is not UTF-8 cannot be written back faithfully; Git is asked
    // for that family every time.
    let (Some(common), Some(registry), Some(members)) = (
        text(common),
        registry.iter().map(|p| text(p)).collect::<Option<Vec<_>>>(),
        members.iter().map(|p| text(p)).collect::<Option<Vec<_>>>(),
    ) else {
        return;
    };
    let record = Record {
        version: VERSION,
        common: common.to_owned(),
        registry: registry.into_iter().map(str::to_owned).collect(),
        members: members.into_iter().map(str::to_owned).collect(),
    };
    let Ok(body) = serde_json::to_vec(&record) else {
        return;
    };
    if body.len() as u64 > MAX_FILE_BYTES {
        return;
    }
    match std::fs::DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => return,
    }
    match std::fs::symlink_metadata(dir) {
        Ok(meta) if private_to_this_user(&meta, true) => {}
        _ => return,
    }
    // Written whole under a name of its own and renamed over the target, so a
    // reader sees the old file or the new one and never half of either.
    let name = file_name(common);
    // A staging name no other writer, thread or crashed process can hold: pid,
    // clock and a counter, created exclusively, and tried again on the rare
    // collision rather than taking over or deleting what is there.
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let mut staged = None;
    for _ in 0..STAGING_ATTEMPTS {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let count = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let staging = dir.join(format!(
            ".{name}.{}.{nanos}.{count}{STAGING_SUFFIX}",
            std::process::id()
        ));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&staging)
        {
            Ok(file) => {
                staged = Some((staging, file));
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return,
        }
    }
    let Some((staging, mut file)) = staged else {
        return;
    };
    let written = file.write_all(&body).and_then(|()| file.sync_data());
    if written.is_err() || std::fs::rename(&staging, dir.join(&name)).is_err() {
        let _ = std::fs::remove_file(&staging);
        return;
    }
    prune(dir, &name);
}

/// Staging files are `.<name>.<pid>.<nanos>.<n>.tmp`.
#[cfg(unix)]
const STAGING_SUFFIX: &str = ".tmp";

/// Tries at a staging name before the write is given up (the next call
/// writes it).
#[cfg(unix)]
const STAGING_ATTEMPTS: usize = 8;

/// Most remembered families kept. One file per repository, a few hundred bytes
/// each; past this the least recently written go.
#[cfg(unix)]
const MAX_FILES: usize = 256;

/// A staging file older than this belongs to a writer that is gone.
#[cfg(unix)]
const STAGING_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(60);

/// Directory entries one prune looks at, so a directory something else filled
/// is bounded work.
#[cfg(unix)]
const PRUNE_SCAN: usize = 4 * MAX_FILES;

/// Keeps the directory bounded after a store: removes staging leftovers older
/// than [`STAGING_MAX_AGE`] and, over [`MAX_FILES`], the oldest records by
/// modification time, never `keep` (the one just written). Stores happen only
/// when Git was asked, so this is not on the hot path. Errors are ignored:
/// a record that stays costs bytes, and one that goes costs one Git call.
#[cfg(unix)]
fn prune(dir: &Path, keep: &str) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let now = std::time::SystemTime::now();
    let mut records = Vec::new();
    for entry in entries.take(PRUNE_SCAN).flatten() {
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let modified = meta.modified().unwrap_or(now);
        let file_name = entry.file_name();
        let file_name = file_name.to_string_lossy();
        if file_name.ends_with(STAGING_SUFFIX) {
            let age = now.duration_since(modified).unwrap_or_default();
            if age > STAGING_MAX_AGE {
                let _ = std::fs::remove_file(entry.path());
            }
        } else if file_name.ends_with(".json") && file_name != keep {
            records.push((modified, entry.path()));
        }
    }
    // `keep` is one of the MAX_FILES the directory may hold.
    if records.len() < MAX_FILES {
        return;
    }
    records.sort();
    let excess = records.len() + 1 - MAX_FILES;
    for (_, path) in records.into_iter().take(excess) {
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(not(unix))]
fn store_in(_dir: &Path, _common: &Path, _registry: &[PathBuf], _members: &[PathBuf]) {}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn paths(names: &[&str]) -> Vec<PathBuf> {
        names.iter().map(PathBuf::from).collect()
    }

    fn private_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("dir");
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        dir
    }

    #[test]
    fn a_stored_family_is_read_back_for_its_own_repository_only() {
        let dir = private_dir();
        let common = Path::new("/r/.git");
        let registry = paths(&["/r", "/w"]);
        store_in(dir.path(), common, &registry, &registry);
        let stored = load_in(dir.path(), common).expect("stored");
        assert_eq!(stored.registry, registry);
        assert_eq!(stored.members, registry);
        assert!(load_in(dir.path(), Path::new("/other/.git")).is_none());
    }

    #[test]
    fn a_file_another_user_could_have_written_is_not_believed() {
        let dir = private_dir();
        let common = Path::new("/r/.git");
        let registry = paths(&["/r"]);
        store_in(dir.path(), common, &registry, &registry);
        let file = dir.path().join(file_name("/r/.git"));
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o666)).unwrap();
        assert!(
            load_in(dir.path(), common).is_none(),
            "group/other writable"
        );
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(load_in(dir.path(), common).is_some());
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(load_in(dir.path(), common).is_none(), "open directory");
    }

    #[test]
    fn a_symlink_in_place_of_the_file_is_not_followed() {
        let dir = private_dir();
        let elsewhere = private_dir();
        let common = Path::new("/r/.git");
        let registry = paths(&["/r"]);
        store_in(elsewhere.path(), common, &registry, &registry);
        std::os::unix::fs::symlink(
            elsewhere.path().join(file_name("/r/.git")),
            dir.path().join(file_name("/r/.git")),
        )
        .unwrap();
        assert!(load_in(dir.path(), common).is_none());
    }

    #[test]
    fn garbage_an_oversized_file_and_another_version_are_misses() {
        let dir = private_dir();
        let common = Path::new("/r/.git");
        let file = dir.path().join(file_name("/r/.git"));
        for body in [
            b"not json".to_vec(),
            vec![b' '; (MAX_FILE_BYTES + 1) as usize],
            br#"{"version":99,"common":"/r/.git","registry":[],"members":[]}"#.to_vec(),
            br#"{"version":1,"common":"/elsewhere/.git","registry":[],"members":[]}"#.to_vec(),
        ] {
            std::fs::write(&file, body).unwrap();
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
            assert!(load_in(dir.path(), common).is_none());
        }
    }

    #[test]
    fn a_missing_or_foreign_directory_is_a_miss_and_never_an_error() {
        let dir = private_dir();
        let common = Path::new("/r/.git");
        assert!(load_in(&dir.path().join("absent"), common).is_none());
        let open = tempfile::tempdir().unwrap();
        std::fs::set_permissions(open.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        store_in(open.path(), common, &paths(&["/r"]), &paths(&["/r"]));
        assert!(
            std::fs::read_dir(open.path()).unwrap().next().is_none(),
            "wrote into a directory others can write"
        );
    }

    /// The guard path. The anchor `resolve_worktree_family` returns is where the
    /// ledger and the task store are read, so a file that could move it would
    /// let a same-user writer make a guard judge this checkout against another
    /// repository's task. Whatever the file says, the anchor is the primary
    /// checkout the repository's own metadata proves, and the family is only
    /// what the registry names.
    #[test]
    fn a_forged_file_cannot_point_the_guard_at_another_repositorys_anchor() {
        use crate::engine::worktree::{
            family_disk_dir, forget_in_process, resolve_worktree_family,
        };
        use crate::test_support::{git_in, repo_with_worktrees, trust_repo};

        let (main, _parent, linked) = repo_with_worktrees(2);
        trust_repo(main.path());
        let memory = private_dir();
        let _remembering = family_disk_dir::set(memory.path());
        let other = tempfile::tempdir().expect("another repository");
        git_in(other.path(), &["init", "-b", "main"]);
        trust_repo(other.path());
        let other = other.path().canonicalize().unwrap();

        let repo = main.path().canonicalize().unwrap();
        let common = repo.join(".git");
        let (repo_s, worktree_s) = (repo.to_str().unwrap(), linked[0].to_str().unwrap());
        let honest = resolve_worktree_family(repo_s, worktree_s).expect("registered");
        let stored = load_in(memory.path(), &common).expect("stored");

        let mut widened = vec![other.clone()];
        widened.extend(stored.members.clone());
        let swapped: Vec<PathBuf> = stored
            .members
            .iter()
            .map(|m| {
                if *m == honest.anchor {
                    other.clone()
                } else {
                    m.clone()
                }
            })
            .collect();
        let mut reversed = stored.members.clone();
        reversed.reverse();
        for forged in [widened, swapped, reversed] {
            store_in(memory.path(), &common, &stored.registry, &forged);
            forget_in_process(&common);
            let family = resolve_worktree_family(repo_s, worktree_s).expect("still answers");
            assert_eq!(family.anchor, honest.anchor, "forged {forged:?}");
            assert!(!family.members.contains(&other), "{:?}", family.members);
            let mut got = family.members.clone();
            got.sort();
            let mut want = honest.members.clone();
            want.sort();
            assert_eq!(got, want);
        }
    }

    #[test]
    fn a_non_utf8_family_is_not_stored() {
        use std::os::unix::ffi::OsStrExt;
        let dir = private_dir();
        let odd = PathBuf::from(std::ffi::OsStr::from_bytes(b"/r/\xff"));
        let family = std::slice::from_ref(&odd);
        store_in(dir.path(), Path::new("/r/.git"), family, family);
        assert!(std::fs::read_dir(dir.path()).unwrap().next().is_none());
    }
}
