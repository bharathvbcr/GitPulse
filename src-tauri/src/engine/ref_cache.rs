//! Ref listings answered from the files they are a function of.
//!
//! Every watcher event used to re-run `for-each-ref`, `tag -l` and `stash list`
//! for a refresh, even when the event was a working-tree edit no listing can
//! see. Those answers depend only on the ref store, HEAD and config, so each
//! one is kept against a stamp of exactly those files and served again while
//! the stamp still matches.
//!
//! The rules that keep a stale answer from ever being served:
//!
//! - The stamp is read *before* git runs and the answer is stored under it. A
//!   write that races the child leaves the files different from the stamp, so
//!   the next caller misses.
//! - A stamp whose newest timestamp is within [`RACY_WINDOW`] of the moment it
//!   was read is never stored. A filesystem with coarse timestamps can give two
//!   writes inside one tick the same time, and git replaces most of these files
//!   by rename (a new inode) but appends to reflogs in place.
//! - A walk that meets a symlink, an unreadable entry, an entry with no
//!   modification time, or more than [`MAX_STAMP_ENTRIES`] entries yields no
//!   stamp, and no stamp means git runs. It never yields a partial stamp.
//! - Errors, refusals, deferrals and capped output are never stored.
//! - Trust is checked before the lookup, so a revoked repository is refused
//!   here exactly as it would be at the spawn.
//! - An entry expires after [`TTL`] whatever the stamp says. That bounds the
//!   inputs no stamp sees: files pulled in by `include.path`, and objects that
//!   a partial clone fetches later.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use crate::engine::git_cli::{self, Incomplete};

/// How close to "now" a stamped timestamp may be before the stamp is not
/// trusted to tell two writes apart. Generous for 1–2 s filesystems.
const RACY_WINDOW: Duration = Duration::from_secs(3);

/// Upper bound on how long any answer is served, whatever its stamp says.
const TTL: Duration = Duration::from_secs(60);

/// Files and directories one stamp may cover. A repository with more loose
/// refs than this is answered by git every time, as it was before.
const MAX_STAMP_ENTRIES: usize = 4096;

/// Answers kept at once, across every repository and listing.
const MAX_ENTRIES: usize = 64;

/// Bytes kept across every answer. A history page is far larger than a ref
/// listing, and an entry count alone would let 64 of them hold hundreds of
/// megabytes.
const MAX_KEPT_BYTES: usize = 32 * 1024 * 1024;

/// Largest single answer kept. A bigger one is answered by git every time.
const MAX_ENTRY_BYTES: usize = 8 * 1024 * 1024;

/// The namespace that means the whole ref store, every worktree's included.
pub(crate) const ALL_REFS: &str = "refs";

/// Identity of one file or directory: git replaces config, HEAD, packed-refs
/// and loose refs by lockfile rename, which gives each write a new inode, and
/// adding or removing an entry moves its directory's mtime.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct FileIdentity {
    inode: u64,
    len: u64,
    modified: Option<SystemTime>,
}

impl FileIdentity {
    pub(crate) fn of(meta: &std::fs::Metadata) -> Self {
        Self {
            inode: file_inode(meta),
            len: meta.len(),
            modified: meta.modified().ok(),
        }
    }

    /// Identity of `path`, following symlinks; `None` when it cannot be read,
    /// which callers stamp as "absent".
    pub(crate) fn at(path: &Path) -> Option<Self> {
        std::fs::metadata(path).ok().map(|meta| Self::of(&meta))
    }
}

#[cfg(unix)]
fn file_inode(meta: &std::fs::Metadata) -> u64 {
    std::os::unix::fs::MetadataExt::ino(meta)
}

/// Windows has no stable inode through `std`; size and mtime still move on
/// every lockfile rename git performs.
#[cfg(not(unix))]
fn file_inode(_meta: &std::fs::Metadata) -> u64 {
    0
}

/// Config files outside the repository that git reads for every command.
pub(crate) fn user_config_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        paths.push(home.join(".gitconfig"));
        paths.push(home.join(".config").join("git").join("config"));
    }
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from) {
        paths.push(xdg.join("git").join("config"));
    }
    for var in ["GIT_CONFIG_GLOBAL", "GIT_CONFIG_SYSTEM"] {
        if let Some(path) = std::env::var_os(var).filter(|value| !value.is_empty()) {
            paths.push(PathBuf::from(path));
        }
    }
    for system in [
        "/etc/gitconfig",
        "/opt/homebrew/etc/gitconfig",
        "/usr/local/etc/gitconfig",
    ] {
        paths.push(PathBuf::from(system));
    }
    paths
}

/// The state one listing was computed against. Equality is over every entry;
/// `newest` only decides whether the stamp may be stored at all.
#[derive(Clone, Debug)]
struct RefStamp {
    entries: Vec<(PathBuf, Option<FileIdentity>)>,
    newest: SystemTime,
}

impl PartialEq for RefStamp {
    fn eq(&self, other: &Self) -> bool {
        self.entries == other.entries
    }
}

impl RefStamp {
    /// Stamps HEAD, config, the packed and reftable stores, replace refs, and
    /// every entry under each of `namespaces` (paths relative to the git
    /// directory, such as `refs/heads` or `logs/refs/stash`), in both the
    /// worktree's own git directory and the common one.
    fn read(repo: &Path, namespaces: &[&str]) -> Option<Self> {
        let (private, common) = crate::repository_trust::git_directories(repo).ok()?;
        let mut walk = Walk::default();
        for file in [
            private.join("HEAD"),
            private.join("config.worktree"),
            common.join("config"),
            common.join("packed-refs"),
            common.join("shallow"),
            common.join("info").join("grafts"),
        ] {
            walk.leaf(file)?;
        }
        for path in user_config_paths() {
            walk.leaf(path)?;
        }
        let mut roots = vec![common.join("reftable"), common.join("refs").join("replace")];
        for namespace in namespaces {
            let relative = namespace.trim_end_matches('/');
            roots.push(common.join(relative));
            if private != common {
                roots.push(private.join(relative));
            }
        }
        // The whole ref store means every worktree's: `--all` walks each
        // linked worktree's HEAD and its own refs (bisect, worktree/*), which
        // live under `worktrees/<name>/`, not under `refs/`. A commit on a
        // detached HEAD in an agent's worktree moves only that file.
        if namespaces
            .iter()
            .any(|ns| ns.trim_end_matches('/') == ALL_REFS)
        {
            let linked = common.join("worktrees");
            match std::fs::read_dir(&linked) {
                Ok(entries) => {
                    walk.leaf(linked.clone())?;
                    for entry in entries {
                        let dir = entry.ok()?.path();
                        walk.leaf(dir.join("HEAD"))?;
                        roots.push(dir.join("refs"));
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    walk.absent(linked)?;
                }
                Err(_) => return None,
            }
        }
        for root in roots {
            walk.tree(root)?;
        }
        walk.entries.sort_by(|a, b| a.0.cmp(&b.0));
        walk.entries.dedup_by(|a, b| a.0 == b.0);
        Some(Self {
            entries: walk.entries,
            newest: walk.newest.unwrap_or(SystemTime::UNIX_EPOCH),
        })
    }

    /// A hash of exactly what equality compares, to tell two stamps apart in a
    /// map key. Not an identity: callers only coalesce on it.
    fn fingerprint(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.entries.hash(&mut hasher);
        hasher.finish()
    }

    /// False while a write inside the same timestamp tick could still be
    /// indistinguishable from the state stamped. A timestamp from the future
    /// (a skewed clock or a restored backup) is never trusted either.
    fn settled(&self, now: SystemTime) -> bool {
        let window = racy_window();
        match now.duration_since(self.newest) {
            Ok(age) => age >= window,
            Err(_) => false,
        }
    }
}

/// The state of the files a ref listing over some namespaces is a function of,
/// for a caller that coalesces concurrent asks itself and needs to know
/// whether anything moved between two of them. Opaque: the only operation is
/// equality, and a stamp is returned whether or not it has settled, because a
/// caller comparing two reads is not storing an answer under it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Stamp(RefStamp);

/// [`Stamp`] of `namespaces`; `None` when the walk cannot describe the state
/// (a symlink, an unreadable entry, too many refs), which is never equal to
/// anything.
pub(crate) fn stamp(repo: &Path, namespaces: &[&str]) -> Option<Stamp> {
    RefStamp::read(repo, namespaces).map(Stamp)
}

#[derive(Default)]
struct Walk {
    entries: Vec<(PathBuf, Option<FileIdentity>)>,
    newest: Option<SystemTime>,
}

impl Walk {
    fn note(&mut self, path: PathBuf, meta: &std::fs::Metadata) -> Option<()> {
        if self.entries.len() >= MAX_STAMP_ENTRIES {
            return None;
        }
        let identity = FileIdentity::of(meta);
        let modified = identity.modified?;
        self.newest = Some(self.newest.map_or(modified, |seen| seen.max(modified)));
        self.entries.push((path, Some(identity)));
        Some(())
    }

    fn absent(&mut self, path: PathBuf) -> Option<()> {
        if self.entries.len() >= MAX_STAMP_ENTRIES {
            return None;
        }
        self.entries.push((path, None));
        Some(())
    }

    /// A single file, following symlinks (config may be a dotfile link).
    fn leaf(&mut self, path: PathBuf) -> Option<()> {
        match std::fs::metadata(&path) {
            Ok(meta) => self.note(path, &meta),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => self.absent(path),
            Err(_) => None,
        }
    }

    /// Every entry under `root`, which may itself be a file (`refs/stash`).
    /// A symlink anywhere below it ends the walk: what it points at is not
    /// stamped, so nothing read through it can be cached.
    fn tree(&mut self, root: PathBuf) -> Option<()> {
        let mut pending = vec![root];
        let mut first = true;
        while let Some(path) = pending.pop() {
            let meta = match std::fs::symlink_metadata(&path) {
                Ok(meta) => meta,
                Err(error) if first && error.kind() == std::io::ErrorKind::NotFound => {
                    return self.absent(path);
                }
                // A child listed a moment ago and gone now is a write in
                // flight; the stamp would describe neither state.
                Err(_) => return None,
            };
            first = false;
            if meta.file_type().is_symlink() {
                return None;
            }
            self.note(path.clone(), &meta)?;
            if meta.is_dir() {
                for entry in std::fs::read_dir(&path).ok()? {
                    pending.push(entry.ok()?.path());
                    if pending.len() + self.entries.len() > MAX_STAMP_ENTRIES {
                        return None;
                    }
                }
            }
        }
        Some(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Key {
    repo: PathBuf,
    argv: Vec<String>,
}

struct Kept {
    stamp: RefStamp,
    stored_at: Instant,
    value: String,
}

fn kept() -> &'static Mutex<HashMap<Key, Kept>> {
    static KEPT: OnceLock<Mutex<HashMap<Key, Kept>>> = OnceLock::new();
    KEPT.get_or_init(|| Mutex::new(HashMap::new()))
}

/// [`git_cli::git_text_shared`] for a ref listing over `namespaces`.
pub(crate) fn git_text(repo: &Path, namespaces: &[&str], args: &[&str]) -> Result<String, String> {
    through(repo, namespaces, args, || {
        git_cli::git_text_shared(repo, args).map(|text| (text, true))
    })
}

/// [`git_cli::git_text_capped_shared`] for a ref listing over `namespaces`.
/// Only a complete answer is kept: a stream cut at the cap may also have been
/// cut by a timeout, and the two are not told apart here.
pub(crate) fn git_text_capped(
    repo: &Path,
    namespaces: &[&str],
    args: &[&str],
    cap: usize,
) -> Result<(String, Option<Incomplete>), String> {
    let mut incomplete = None;
    let text = through(repo, namespaces, args, || {
        let (text, cut) = git_cli::git_text_capped_shared(repo, args, cap)?;
        let complete = cut.is_none();
        incomplete = cut;
        Ok((text, complete))
    })?;
    Ok((text, incomplete))
}

/// `run` answers with the text and whether it may be kept.
///
/// Called directly by a listing whose non-zero exit can be an answer rather
/// than a failure (`git notes list` on a ref this git cannot load), which the
/// single `Err` of [`git_text`] cannot tell apart. `run` still owns the spawn,
/// so it still goes through [`git_cli`] and its gate.
pub(crate) fn through(
    repo: &Path,
    namespaces: &[&str],
    args: &[&str],
    run: impl FnOnce() -> Result<(String, bool), String>,
) -> Result<String, String> {
    through_stamped(repo, args, stamp(repo, namespaces).as_ref(), run)
}

/// [`through`] with the stamp the caller already read for `namespaces`, so a
/// caller that compares stamps itself (single-flight coalescing) reads the
/// files once per call, not twice. `None` is a stamp that could not be read,
/// as in [`stamp`]: the answer is computed and not kept. Settledness is
/// judged here, so the clock is still read in one place.
pub(crate) fn through_stamped(
    repo: &Path,
    args: &[&str],
    stamp: Option<&Stamp>,
    run: impl FnOnce() -> Result<(String, bool), String>,
) -> Result<String, String> {
    crate::repository_trust::require(repo)?;
    let key = Key {
        repo: repo.to_path_buf(),
        argv: args.iter().map(|arg| (*arg).to_string()).collect(),
    };
    let stamp = stamp
        .map(|stamp| &stamp.0)
        .filter(|stamp| stamp.settled(wall_now()))
        .cloned();
    if let Some(stamp) = &stamp {
        let table = kept().lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(entry) = table.get(&key) {
            // An entry stamped after `now` (the clock stepped back) has no
            // age, and is not trusted rather than treated as brand new.
            let fresh = mono_now()
                .checked_duration_since(entry.stored_at)
                .is_some_and(|age| age < TTL);
            if &entry.stamp == stamp && fresh {
                note_hit();
                return Ok(entry.value.clone());
            }
        }
    }
    let (value, keep) = run()?;
    if let (Some(stamp), true) = (stamp, keep && value.len() <= MAX_ENTRY_BYTES) {
        let mut table = kept().lock().unwrap_or_else(PoisonError::into_inner);
        table.remove(&key);
        // Oldest first, until both bounds hold with this answer added.
        loop {
            let bytes: usize = table.values().map(|entry| entry.value.len()).sum();
            if table.len() < MAX_ENTRIES && bytes + value.len() <= MAX_KEPT_BYTES {
                break;
            }
            let Some(oldest) = table
                .iter()
                .min_by_key(|(_, entry)| entry.stored_at)
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            table.remove(&oldest);
        }
        table.insert(
            key,
            Kept {
                stamp,
                stored_at: mono_now(),
                value: value.clone(),
            },
        );
    }
    Ok(value)
}

/// [`through_stamped`] for an answer that costs far more than a listing
/// (a blame over a sample of files), where two callers asking at once must not
/// both compute it. Callers join one run only when the repository, the
/// question, the admission class and the stamp are all the same, so a caller
/// that wrote to the repository after the run began never inherits its answer.
/// A caller with no stamp cannot be told apart from a later state and runs on
/// its own.
pub(crate) fn through_stamped_coalesced(
    repo: &Path,
    args: &[&str],
    stamp: Option<&Stamp>,
    run: impl FnOnce() -> Result<(String, bool), String>,
) -> Result<String, String> {
    type Key = (PathBuf, Vec<String>, u64, git_cli::Admission);
    // The answer travels with what the leader's producers wrote on its thread
    // (`ipc_error::snapshot`), so a waiter's command types the leader's refusal
    // the way the leader's does: the text alone says nothing about its kind.
    type Flights =
        crate::singleflight::SingleFlight<Key, (Result<String, String>, crate::ipc_error::Records)>;
    static FLIGHTS: OnceLock<Flights> = OnceLock::new();
    let Some(stamped) = stamp else {
        return through_stamped(repo, args, None, run);
    };
    let key = (
        repo.to_path_buf(),
        args.iter().map(|arg| (*arg).to_string()).collect(),
        stamped.0.fingerprint(),
        git_cli::current_admission(),
    );
    let (answer, records) = FLIGHTS.get_or_init(Flights::default).run(key, || {
        let answer = through_stamped(repo, args, stamp, run);
        (answer, crate::ipc_error::snapshot())
    });
    crate::ipc_error::absorb(records);
    answer
}

/// The monotonic "now" the TTL is measured on. Production reads the real
/// clock; tests install a [`ManualClock`] so the TTL boundary needs no sleep.
#[cfg(not(test))]
fn mono_now() -> Instant {
    Instant::now()
}

/// The wall-clock "now" a stamp's file mtimes are compared against. A second
/// reading beside [`mono_now`] because the two clocks are different types; a
/// [`ManualClock`] moves both together.
#[cfg(not(test))]
fn wall_now() -> SystemTime {
    SystemTime::now()
}

#[cfg(not(test))]
fn racy_window() -> Duration {
    RACY_WINDOW
}

#[cfg(not(test))]
fn note_hit() {}

#[cfg(test)]
thread_local! {
    static RACY_OVERRIDE: std::cell::Cell<Option<Duration>> = const { std::cell::Cell::new(None) };
    static HITS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static CLOCK: std::cell::Cell<Option<(Instant, SystemTime)>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
fn mono_now() -> Instant {
    CLOCK
        .with(std::cell::Cell::get)
        .map_or_else(Instant::now, |(mono, _)| mono)
}

#[cfg(test)]
fn wall_now() -> SystemTime {
    CLOCK
        .with(std::cell::Cell::get)
        .map_or_else(SystemTime::now, |(_, wall)| wall)
}

/// A clock a test moves by hand, on this thread only, until dropped. Moves
/// the monotonic and wall readings together, in either direction.
#[cfg(test)]
pub(crate) struct ManualClock;

#[cfg(test)]
impl ManualClock {
    pub(crate) fn install() -> Self {
        // An hour of headroom so `rewind` never underflows the platform epoch.
        let start = (
            Instant::now() + Duration::from_secs(3600),
            SystemTime::now() + Duration::from_secs(3600),
        );
        CLOCK.with(|cell| cell.set(Some(start)));
        Self
    }

    pub(crate) fn now(&self) -> SystemTime {
        wall_now()
    }

    pub(crate) fn advance(&self, by: Duration) {
        CLOCK.with(|cell| {
            let (mono, wall) = cell.get().expect("clock installed");
            cell.set(Some((mono + by, wall + by)));
        });
    }

    pub(crate) fn rewind(&self, by: Duration) {
        CLOCK.with(|cell| {
            let (mono, wall) = cell.get().expect("clock installed");
            cell.set(Some((mono.checked_sub(by).expect("headroom"), wall - by)));
        });
    }
}

#[cfg(test)]
impl Drop for ManualClock {
    fn drop(&mut self) {
        CLOCK.with(|cell| cell.set(None));
    }
}

#[cfg(test)]
fn racy_window() -> Duration {
    RACY_OVERRIDE.with(|cell| cell.get()).unwrap_or(RACY_WINDOW)
}

#[cfg(test)]
fn note_hit() {
    HITS.with(|cell| cell.set(cell.get() + 1));
}

/// Held by every test that counts on an answer staying kept, and by the one
/// that fills the budget: that one evicts whatever else is in the
/// process-wide table. Shared with the callers' own tests (provenance), whose
/// kept answers live in the same table.
#[cfg(test)]
pub(crate) fn retention() -> std::sync::MutexGuard<'static, ()> {
    static SERIAL: Mutex<()> = Mutex::new(());
    SERIAL.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Runs `f` with no racy window, so every stamp is stored: what the oracle
/// then proves is that the stamp itself sees every change, with no help from
/// the window.
#[cfg(test)]
pub(crate) fn eager<T>(f: impl FnOnce() -> T) -> T {
    RACY_OVERRIDE.with(|cell| cell.set(Some(Duration::ZERO)));
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            RACY_OVERRIDE.with(|cell| cell.set(None));
        }
    }
    let _reset = Reset;
    f()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::procguard::LockedSpawn;
    use crate::test_support::git_in;

    const BRANCHES: &[&str] = &["refs/heads", "refs/remotes"];
    const TAGS: &[&str] = &["refs/tags"];
    const STASH: &[&str] = &["refs/stash", "logs/refs/stash"];
    const ALL: &[&str] = &["refs"];

    const BRANCH_ARGS: &[&str] = &[
        "for-each-ref",
        "--format=%(HEAD)%00%(refname)%00%(objectname)%00%(upstream:track)%00%(upstream:short)%00%(contents:subject)",
        "refs/heads/",
        "refs/remotes/",
    ];
    const TAG_ARGS: &[&str] = &[
        "tag",
        "-l",
        "--sort=-creatordate",
        "--format=%(refname:short)%00%(objectname)%00%(contents:subject)%00%(*objectname)",
    ];
    const STASH_ARGS: &[&str] = &["stash", "list", "-z", "--format=%gd%x00%H%x00%ct%x00%gs"];
    const ALL_ARGS: &[&str] = &[
        "for-each-ref",
        "--format=%(objectname)%00%(refname)%00%(objecttype)",
        "refs/",
    ];
    const ALL_LOG_ARGS: &[&str] = &["log", "--topo-order", "--format=%H %P %s", "--all"];
    const REV_LIST_ALL_ARGS: &[&str] = &["rev-list", "--all"];
    const HEAD_ONLY: &[&str] = &["refs/heads"];
    const HEAD_ARGS: &[&str] = &["rev-parse", "HEAD"];

    fn hits() -> u64 {
        HITS.with(std::cell::Cell::get)
    }

    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self, bound: usize) -> usize {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            ((self.0 >> 33) % bound as u64) as usize
        }
    }

    fn write(path: &Path, text: &str) {
        std::fs::write(path, text).expect("write fixture file");
    }

    /// One repository, a linked worktree, and a bare remote to push and fetch.
    struct Fixture {
        _dirs: Vec<tempfile::TempDir>,
        main: PathBuf,
        linked: PathBuf,
    }

    fn fixture(ref_format: Option<&str>) -> Option<Fixture> {
        let holder = tempfile::tempdir().unwrap();
        let base = crate::engine::git_cli::canonicalize_plain(holder.path()).unwrap();
        let main = base.join("main");
        std::fs::create_dir(&main).unwrap();
        let mut init = vec!["init", "-q", "-b", "main"];
        let format_arg;
        if let Some(format) = ref_format {
            format_arg = format!("--ref-format={format}");
            init.push(&format_arg);
        }
        let probe = std::process::Command::new("git")
            .args(&init)
            .current_dir(&main)
            .output_locked()
            .expect("spawn git init");
        if !probe.status.success() {
            return None;
        }
        crate::test_support::trust_repo(&main);
        write(&main.join("a.txt"), "one\n");
        git_in(&main, &["add", "a.txt"]);
        git_in(&main, &["commit", "-q", "-m", "first"]);
        let remote = base.join("remote.git");
        let mut bare = vec!["init", "-q", "--bare"];
        if let Some(format) = ref_format {
            bare.push(if format == "reftable" {
                "--ref-format=reftable"
            } else {
                "--ref-format=files"
            });
        }
        bare.push(remote.to_str().unwrap());
        // Not `git_in`: that trusts the directory an `init` ran in, and the
        // remote is never opened as a repository.
        let created = std::process::Command::new("git")
            .args(&bare)
            .current_dir(&base)
            .output_locked()
            .expect("spawn git init --bare");
        assert!(created.status.success(), "{created:?}");
        git_in(
            &main,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        git_in(&main, &["push", "-q", "-u", "origin", "main"]);
        let linked = base.join("linked");
        git_in(
            &main,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "side",
                linked.to_str().unwrap(),
            ],
        );
        Some(Fixture {
            _dirs: vec![holder],
            main,
            linked,
        })
    }

    /// The operations a person or an agent runs between two refreshes. Each
    /// changes something one of the listings shows.
    fn mutate(fx: &Fixture, rng: &mut Lcg, step: usize) -> &'static str {
        let main = &fx.main;
        let linked = &fx.linked;
        let name = format!("b{step}");
        match rng.next(18) {
            0 => {
                write(&main.join("a.txt"), &format!("{step}\n"));
                git_in(main, &["commit", "-q", "-am", &format!("edit {step}")]);
                "commit"
            }
            1 => {
                // A ref that lives only in packed-refs is deleted by
                // rewriting that one file: no loose file, no directory moves.
                let packed = std::fs::read_to_string(main.join(".git/packed-refs"))
                    .unwrap_or_default()
                    .lines()
                    .filter_map(|line| line.split_once(" refs/heads/b"))
                    .map(|(_, rest)| format!("b{rest}"))
                    .find(|branch| !main.join(".git/refs/heads").join(branch).exists());
                match packed {
                    Some(branch) if step.is_multiple_of(2) => {
                        git_in(main, &["branch", "-q", "-D", &branch]);
                        "packed-only branch deleted"
                    }
                    _ => {
                        git_in(main, &["branch", &name]);
                        "branch"
                    }
                }
            }
            2 => {
                git_in(main, &["branch", "-q", "-D", "side-gone"]);
                git_in(main, &["branch", "side-gone"]);
                "branch delete and recreate"
            }
            3 => {
                git_in(main, &["tag", &format!("t{step}")]);
                "lightweight tag"
            }
            4 => {
                git_in(
                    main,
                    &[
                        "tag",
                        "-a",
                        "-m",
                        &format!("annotated {step}"),
                        &format!("a{step}"),
                    ],
                );
                "annotated tag"
            }
            5 => {
                git_in(main, &["tag", "-f", "moving", "HEAD"]);
                "tag moved in place"
            }
            6 => {
                write(&main.join("a.txt"), &format!("dirty {step}\n"));
                git_in(main, &["stash", "push", "-q", "-m", &format!("s{step}")]);
                "stash push"
            }
            7 => {
                let listed = std::process::Command::new("git")
                    .args(["stash", "list"])
                    .current_dir(main)
                    .output_locked()
                    .unwrap();
                match listed.stdout.iter().filter(|&&byte| byte == b'\n').count() {
                    0 => {
                        git_in(main, &["pack-refs", "--all"]);
                        "pack-refs"
                    }
                    1 => {
                        git_in(main, &["stash", "drop", "-q"]);
                        "stash drop"
                    }
                    _ if step.is_multiple_of(2) => {
                        git_in(main, &["stash", "drop", "-q", "stash@{1}"]);
                        "stash drop below the top"
                    }
                    // Rewrites only the reflog: `refs/stash` does not move,
                    // and the listing loses an entry.
                    _ => {
                        git_in(main, &["reflog", "delete", "refs/stash@{1}"]);
                        "stash reflog entry deleted"
                    }
                }
            }
            8 => {
                git_in(main, &["pack-refs", "--all"]);
                "pack-refs"
            }
            9 => {
                git_in(main, &["push", "-q", "origin", "HEAD:main", "--force"]);
                git_in(main, &["fetch", "-q", "origin"]);
                "push and fetch"
            }
            10 => {
                git_in(main, &["config", "branch.side-gone.remote", "origin"]);
                git_in(
                    main,
                    &["config", "branch.side-gone.merge", "refs/heads/main"],
                );
                "upstream configured"
            }
            11 => {
                let target = if step.is_multiple_of(2) {
                    "main"
                } else {
                    "parked"
                };
                git_in(main, &["checkout", "-q", target]);
                "checkout"
            }
            12 => {
                write(&linked.join("l.txt"), &format!("{step}\n"));
                git_in(linked, &["add", "l.txt"]);
                git_in(linked, &["commit", "-q", "-m", &format!("linked {step}")]);
                "commit in linked worktree"
            }
            13 => {
                git_in(
                    main,
                    &[
                        "update-ref",
                        &format!("refs/heads/nested/deep/{name}"),
                        "HEAD",
                    ],
                );
                "nested loose ref"
            }
            14 => {
                git_in(main, &["update-ref", "-d", "refs/remotes/origin/main"]);
                git_in(main, &["fetch", "-q", "origin"]);
                "remote ref deleted and refetched"
            }
            16 => {
                // Moves only `worktrees/linked/HEAD`: no ref under `refs/`.
                write(&linked.join("l.txt"), &format!("detached {step}\n"));
                git_in(linked, &["checkout", "-q", "--detach"]);
                git_in(linked, &["add", "l.txt"]);
                git_in(linked, &["commit", "-q", "-m", &format!("detached {step}")]);
                "commit on a detached HEAD in the linked worktree"
            }
            17 => {
                let tip = |name: &str| {
                    let out = std::process::Command::new("git")
                        .args(["rev-parse", name])
                        .current_dir(main)
                        .output_locked()
                        .unwrap();
                    String::from_utf8(out.stdout).unwrap().trim().to_string()
                };
                let (from, to) = (tip("main"), tip("parked"));
                let replaced = main.join(".git/refs/replace").join(&from);
                if replaced.exists()
                    || main
                        .join(".git/refs/replace")
                        .read_dir()
                        .is_ok_and(|mut d| d.next().is_some())
                {
                    let names: Vec<String> = std::fs::read_dir(main.join(".git/refs/replace"))
                        .unwrap()
                        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                        .collect();
                    for name in names {
                        git_in(main, &["replace", "-d", &name]);
                    }
                    "replace refs removed"
                } else if from != to {
                    git_in(main, &["replace", "-f", &from, &to]);
                    "main's tip replaced"
                } else {
                    git_in(main, &["pack-refs", "--all"]);
                    "pack-refs"
                }
            }
            _ => {
                git_in(main, &["tag", "-d", "moving"]);
                git_in(main, &["tag", "moving", "HEAD~0"]);
                "tag deleted and recreated"
            }
        }
    }

    /// Every listing, through the cache, against git asked directly, after
    /// every step of a seeded sequence of real git operations, from both
    /// worktrees. Each listing is asked twice per step, so a stamp that misses
    /// a change is caught serving the stale answer on the second ask at the
    /// latest.
    fn run_oracle(fx: &Fixture, seed: u64, steps: usize) -> u64 {
        let _held = retention();
        git_in(&fx.main, &["branch", "side-gone"]);
        git_in(&fx.main, &["branch", "parked"]);
        git_in(&fx.main, &["tag", "moving"]);
        let mut rng = Lcg(seed);
        let before = hits();
        let named_log: Vec<&str> = ["log", "--topo-order", "--format=%H %P %s"]
            .into_iter()
            .chain(
                crate::graph::history_rev_args(crate::graph::RefScope::Named)
                    .iter()
                    .copied(),
            )
            .collect();
        let reads: Vec<(&[&str], &[&str])> = vec![
            (BRANCHES, BRANCH_ARGS),
            (TAGS, TAG_ARGS),
            (STASH, STASH_ARGS),
            (ALL, ALL_ARGS),
            (ALL, ALL_LOG_ARGS),
            (ALL, REV_LIST_ALL_ARGS),
            (&crate::graph::ref_scope::NAMED_REF_PATTERNS, &named_log),
            (HEAD_ONLY, HEAD_ARGS),
        ];
        for step in 0..steps {
            let op = mutate(fx, &mut rng, step);
            for checkout in [&fx.main, &fx.linked] {
                for &(namespaces, args) in &reads {
                    let truth = git_cli::git_text(checkout, args).expect("direct listing");
                    for ask in 0..2 {
                        let served =
                            eager(|| git_text(checkout, namespaces, args)).expect("cached listing");
                        assert_eq!(
                            served,
                            truth,
                            "seed {seed} step {step} after {op}: {} served a stale {:?} (ask {ask})",
                            checkout.display(),
                            &args[..2.min(args.len())]
                        );
                    }
                }
            }
        }
        let hits = hits() - before;
        // Every second ask follows an unchanged state, so the cache has to
        // have answered at least those, or the oracle proved nothing.
        let floor = (steps * 2 * reads.len()) as u64;
        assert!(
            hits >= floor,
            "seed {seed}: only {hits} of at least {floor} cache hits"
        );
        hits
    }

    #[test]
    fn cached_listings_match_git_under_a_random_sequence_of_writes() {
        for seed in [1, 7, 42] {
            let fx = fixture(None).expect("files-backend repository");
            run_oracle(&fx, seed, 40);
        }
    }

    /// Reftable keeps every ref in `reftable/`; a stamp that only knew loose
    /// refs would serve a stale answer here first.
    #[test]
    fn cached_listings_match_git_on_a_reftable_repository() {
        let Some(fx) = fixture(Some("reftable")) else {
            // Older than git 2.45. CI pins a git that has it, so a skip there
            // is a failure, not a pass.
            assert!(
                std::env::var_os("CI").is_none(),
                "this git cannot create a reftable repository"
            );
            eprintln!("SKIPPED: this git cannot create a reftable repository");
            return;
        };
        run_oracle(&fx, 99, 40);
    }

    /// An unchanged repository is answered without a process.
    #[test]
    fn a_repeated_listing_of_an_idle_repository_spawns_once() {
        let _held = retention();
        let fx = fixture(None).unwrap();
        eager(|| {
            for _ in 0..5 {
                git_text(&fx.main, BRANCHES, BRANCH_ARGS).unwrap();
            }
        });
        let spawned = git_cli::spawn_log::spawns_in(&fx.main)
            .into_iter()
            .filter(|argv| argv.iter().any(|arg| arg == "for-each-ref"))
            .count();
        assert_eq!(spawned, 1);
    }

    /// The listings a refresh asks for all ride the cache: asked three times
    /// over an idle repository, each starts one process.
    #[test]
    fn every_refresh_listing_spawns_once_over_an_idle_repository() {
        let _held = retention();
        let fx = fixture(None).unwrap();
        write(&fx.main.join("a.txt"), "dirty\n");
        git_in(&fx.main, &["stash", "push", "-q"]);
        git_in(&fx.main, &["tag", "v1"]);
        let path = fx.main.to_str().unwrap();
        eager(|| {
            for _ in 0..3 {
                crate::engine::GitReader::list_branches(path).unwrap();
                crate::engine::GitReader::list_tags(path).unwrap();
                crate::engine::stash::list(path).unwrap();
                crate::graph::refs::list_ref_decorations(path, crate::graph::RefScope::Named)
                    .unwrap();
                crate::graph::refs::list_ref_decorations(path, crate::graph::RefScope::All)
                    .unwrap();
                // What a graph reload asks besides the decorations.
                for scope in [crate::graph::RefScope::Named, crate::graph::RefScope::All] {
                    crate::engine::GitReader::read_commit_history_paged(
                        path, 0, 50, None, None, scope,
                    )
                    .unwrap();
                }
                crate::engine::GitReader::head_id(path).unwrap();
                crate::graph::refs::probe_hidden_history(path).unwrap();
                crate::engine::GitReader::default_branch_name(path).unwrap();
            }
        });
        let spawned = git_cli::spawn_log::spawns_in(&fx.main);
        let count = |needle: &str| {
            spawned
                .iter()
                .filter(|argv| argv.iter().any(|arg| arg.contains(needle)))
                .count()
        };
        assert_eq!(count("%(HEAD)"), 1, "branch listing");
        assert_eq!(
            count("%(refname:short)%00%(objectname)%00%(contents"),
            1,
            "tag listing"
        );
        assert_eq!(count("%gd%x00"), 1, "stash listing");
        // Named and All differ in their ref patterns, so each is one listing.
        assert_eq!(count("%(objecttype)"), 2, "graph decorations");
        // One walk per scope, however often the graph reloads.
        assert_eq!(count("--topo-order"), 2, "history walks");
        let exact = |args: &[&str]| {
            spawned
                .iter()
                .filter(|argv| {
                    argv.ends_with(&args.iter().map(|a| a.to_string()).collect::<Vec<_>>())
                })
                .count()
        };
        assert_eq!(exact(&["rev-parse", "HEAD"]), 1, "head id");
        assert_eq!(
            exact(&["symbolic-ref", "--quiet", "HEAD"]),
            1,
            "current branch"
        );
        assert_eq!(count("--max-count="), 1, "hidden-history probe");
    }

    /// A stamp read inside the racy window is not stored, so the next ask
    /// runs git again even though nothing changed.
    #[test]
    fn a_freshly_written_repository_is_not_cached() {
        let fx = fixture(None).unwrap();
        let before = hits();
        git_text(&fx.main, TAGS, TAG_ARGS).unwrap();
        git_text(&fx.main, TAGS, TAG_ARGS).unwrap();
        assert_eq!(hits(), before, "a racy stamp was served");
    }

    /// Revoking trust stops a kept answer exactly as it would stop the spawn.
    #[test]
    fn a_kept_answer_is_refused_once_trust_is_revoked() {
        let fx = fixture(None).unwrap();
        eager(|| git_text(&fx.main, TAGS, TAG_ARGS)).unwrap();
        let path = fx.main.to_str().unwrap();
        crate::repository_trust::revoke(path).expect("revoke");
        let refused = eager(|| git_text(&fx.main, TAGS, TAG_ARGS));
        assert!(refused.is_err(), "served after revoke: {refused:?}");
    }

    /// A refusal is never kept: the next caller asks git.
    #[test]
    fn a_failed_listing_is_not_kept() {
        let fx = fixture(None).unwrap();
        let bad: &[&str] = &["for-each-ref", "--format=%(no-such-atom)", "refs/heads/"];
        assert!(eager(|| git_text(&fx.main, BRANCHES, bad)).is_err());
        let before = hits();
        assert!(eager(|| git_text(&fx.main, BRANCHES, bad)).is_err());
        assert_eq!(hits(), before);
    }

    /// A symlink below a stamped namespace names a target nothing stamps.
    #[cfg(unix)]
    #[test]
    fn a_symlinked_ref_directory_is_never_cached() {
        let fx = fixture(None).unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(elsewhere.path(), fx.main.join(".git/refs/heads/linked-dir"))
            .unwrap();
        assert!(RefStamp::read(&fx.main, BRANCHES).is_none());
    }

    /// Past the entry bound there is no stamp, never a truncated one.
    #[test]
    fn a_ref_store_past_the_bound_yields_no_stamp() {
        let fx = fixture(None).unwrap();
        let heads = fx.main.join(".git/refs/heads/bulk");
        std::fs::create_dir_all(&heads).unwrap();
        let tip = git_cli::git_text(&fx.main, &["rev-parse", "HEAD"]).unwrap();
        for index in 0..MAX_STAMP_ENTRIES {
            write(&heads.join(format!("r{index}")), &tip);
        }
        assert!(RefStamp::read(&fx.main, BRANCHES).is_none());
        assert!(RefStamp::read(&fx.main, TAGS).is_some());
    }

    /// Git itself always moves a loose-ref directory, even when it deletes a
    /// ref that lives only in packed-refs (it takes a loose lock first), so
    /// the oracle cannot see these two inputs on their own. Another writer
    /// can: one that rewrites packed-refs in place, and one that replaces it
    /// by rename keeping both its length and its mtime.
    #[test]
    fn a_packed_refs_rewrite_by_another_writer_moves_the_stamp() {
        let fx = fixture(None).unwrap();
        git_in(&fx.main, &["pack-refs", "--all"]);
        let packed = fx.main.join(".git/packed-refs");
        let text = std::fs::read_to_string(&packed).unwrap();
        let before = RefStamp::read(&fx.main, TAGS).unwrap();

        // Same length, new content, in place: the same inode.
        let flipped: String = text
            .chars()
            .map(|c| match c {
                'a' => 'b',
                'b' => 'a',
                other => other,
            })
            .collect();
        assert_eq!(flipped.len(), text.len());
        std::fs::OpenOptions::new()
            .write(true)
            .open(&packed)
            .unwrap()
            .write_all_at_start(&flipped);
        let in_place = RefStamp::read(&fx.main, TAGS).unwrap();
        assert_ne!(
            before, in_place,
            "an in-place packed-refs rewrite was not seen"
        );

        // Renamed over, with the old length and the old mtime restored: only
        // the inode tells the two files apart.
        let old_mtime = std::fs::metadata(&packed).unwrap().modified().unwrap();
        let staged = fx.main.join(".git/packed-refs.new");
        std::fs::write(&staged, &text).unwrap();
        std::fs::File::options()
            .write(true)
            .open(&staged)
            .unwrap()
            .set_modified(old_mtime)
            .unwrap();
        std::fs::rename(&staged, &packed).unwrap();
        let renamed = RefStamp::read(&fx.main, TAGS).unwrap();
        #[cfg(unix)]
        assert_ne!(
            in_place, renamed,
            "a same-size, same-mtime rename was not seen"
        );
        #[cfg(not(unix))]
        let _ = renamed;
    }

    trait WriteAtStart {
        fn write_all_at_start(self, text: &str);
    }

    impl WriteAtStart for std::fs::File {
        fn write_all_at_start(mut self, text: &str) {
            use std::io::{Seek, Write};
            self.seek(std::io::SeekFrom::Start(0)).unwrap();
            self.write_all(text.as_bytes()).unwrap();
        }
    }

    /// History pages are big. The kept answers never exceed the byte budget,
    /// the oldest go first, and one too large to keep is not kept at all.
    #[test]
    fn kept_answers_stay_inside_the_byte_budget() {
        let _held = retention();
        let fx = fixture(None).unwrap();
        let chunk = MAX_ENTRY_BYTES;
        let bytes_kept = || -> usize {
            kept()
                .lock()
                .unwrap()
                .iter()
                .filter(|(key, _)| key.repo == fx.main)
                .map(|(_, entry)| entry.value.len())
                .sum()
        };
        eager(|| {
            for n in 0..(MAX_KEPT_BYTES / chunk + 2) {
                let marker = format!("page-{n}");
                let value = through(&fx.main, TAGS, &["log", &marker], || {
                    Ok(("x".repeat(chunk), true))
                })
                .unwrap();
                assert_eq!(value.len(), chunk);
            }
        });
        assert!(bytes_kept() <= MAX_KEPT_BYTES, "{} kept", bytes_kept());
        let newest = Key {
            repo: fx.main.clone(),
            argv: vec!["log".into(), format!("page-{}", MAX_KEPT_BYTES / chunk + 1)],
        };
        let oldest = Key {
            repo: fx.main.clone(),
            argv: vec!["log".into(), "page-0".into()],
        };
        {
            let table = kept().lock().unwrap();
            assert!(table.contains_key(&newest), "the newest answer was evicted");
            assert!(
                !table.contains_key(&oldest),
                "the oldest answer outlived the budget"
            );
        }

        let huge = Key {
            repo: fx.main.clone(),
            argv: vec!["log".into(), "huge".into()],
        };
        eager(|| {
            through(&fx.main, TAGS, &["log", "huge"], || {
                Ok(("y".repeat(chunk + 1), true))
            })
        })
        .unwrap();
        assert!(!kept().lock().unwrap().contains_key(&huge));
    }

    /// A 100,000-commit window is read as 5,000-commit chunks (about 1.3 MB
    /// each at a typical 260 bytes a record), because one answer that size
    /// exceeded [`MAX_ENTRY_BYTES`] and was never kept. Every chunk of such a
    /// window stays kept together; a longer one evicts oldest-first and never
    /// passes the byte budget.
    #[test]
    fn a_hundred_thousand_commit_window_stays_kept_as_chunks_and_evicts_oldest_first() {
        let _held = retention();
        let fx = fixture(None).unwrap();
        let chunk_bytes = 5_000 * 260;
        assert!(chunk_bytes < MAX_ENTRY_BYTES, "a chunk must fit one entry");
        let chunks_in_window = 100_000 / 5_000;
        assert!(
            chunks_in_window * chunk_bytes <= MAX_KEPT_BYTES && chunks_in_window < MAX_ENTRIES,
            "the window must fit the budget it is meant to fit"
        );
        let key = |n: usize| Key {
            repo: fx.main.clone(),
            argv: vec!["log".into(), format!("chunk-{n}")],
        };
        let keep_chunk = |n: usize| {
            eager(|| {
                through(&fx.main, TAGS, &["log", &format!("chunk-{n}")], || {
                    Ok(("c".repeat(chunk_bytes), true))
                })
            })
            .unwrap();
        };
        for n in 0..chunks_in_window {
            keep_chunk(n);
        }
        {
            let table = kept().lock().unwrap();
            for n in 0..chunks_in_window {
                assert!(
                    table.contains_key(&key(n)),
                    "chunk {n} of the window was not kept"
                );
            }
        }
        // Growing past the budget evicts the oldest chunks, not the newest.
        for n in chunks_in_window..chunks_in_window + 12 {
            keep_chunk(n);
        }
        let table = kept().lock().unwrap();
        let bytes: usize = table
            .iter()
            .filter(|(k, _)| k.repo == fx.main)
            .map(|(_, entry)| entry.value.len())
            .sum();
        assert!(bytes <= MAX_KEPT_BYTES, "{bytes} bytes kept");
        assert!(
            !table.contains_key(&key(0)),
            "the oldest chunk outlived the budget"
        );
        assert!(
            table.contains_key(&key(chunks_in_window + 11)),
            "the newest chunk was evicted"
        );
    }

    /// A stamp for `through_stamped` whose newest file is `age` old on `clock`.
    fn stamp_aged(clock: &ManualClock, age: Duration) -> Stamp {
        Stamp(RefStamp {
            entries: Vec::new(),
            newest: clock.now() - age,
        })
    }

    /// Runs `through_stamped` and says whether `run` had to compute.
    fn computed(fx: &Fixture, args: &[&str], stamp: &Stamp) -> bool {
        let ran = std::cell::Cell::new(false);
        let value = through_stamped(&fx.main, args, Some(stamp), || {
            ran.set(true);
            Ok(("answer".to_string(), true))
        })
        .unwrap();
        assert_eq!(value, "answer");
        ran.get()
    }

    const MILLI: Duration = Duration::from_millis(1);

    /// The TTL is the safety net for inputs no stamp sees: an unchanged stamp
    /// is served until the entry is exactly `TTL` old, and recomputed from then.
    #[test]
    fn an_entry_is_served_before_the_ttl_and_recomputed_at_it() {
        let _held = retention();
        let fx = fixture(None).unwrap();
        let clock = ManualClock::install();
        let stamp = stamp_aged(&clock, Duration::from_secs(10));
        let args = ["log", "ttl-boundary"];
        assert!(computed(&fx, &args, &stamp), "first ask computes");
        clock.advance(TTL - MILLI);
        assert!(!computed(&fx, &args, &stamp), "served 1 ms before the TTL");
        clock.advance(MILLI);
        assert!(
            computed(&fx, &args, &stamp),
            "recomputed at exactly the TTL"
        );
        assert!(!computed(&fx, &args, &stamp), "and kept afresh from there");
    }

    /// `through`, which reads the stamp from the real files, honours the same
    /// clock: the files are an hour old on it, so the answer is kept, then
    /// expires after the TTL.
    #[test]
    fn through_honours_the_injected_clock() {
        let _held = retention();
        let fx = fixture(None).unwrap();
        let clock = ManualClock::install();
        let runs = std::cell::Cell::new(0);
        let ask = || {
            through(&fx.main, TAGS, &["log", "through-clock"], || {
                runs.set(runs.get() + 1);
                Ok(("answer".to_string(), true))
            })
            .unwrap()
        };
        ask();
        ask();
        assert_eq!(runs.get(), 1, "kept before the TTL");
        clock.advance(TTL);
        ask();
        assert_eq!(runs.get(), 2, "expired at the TTL with the files unchanged");
    }

    /// The coalesced path ends in `through_stamped`, so it expires on the same
    /// clock.
    #[test]
    fn the_coalesced_path_honours_the_injected_clock() {
        let _held = retention();
        let fx = fixture(None).unwrap();
        let clock = ManualClock::install();
        let stamp = stamp_aged(&clock, Duration::from_secs(10));
        let runs = std::cell::Cell::new(0);
        let ask = || {
            through_stamped_coalesced(&fx.main, &["log", "coalesced-clock"], Some(&stamp), || {
                runs.set(runs.get() + 1);
                Ok(("answer".to_string(), true))
            })
            .unwrap()
        };
        ask();
        clock.advance(TTL - MILLI);
        ask();
        assert_eq!(runs.get(), 1, "served 1 ms before the TTL");
        clock.advance(MILLI);
        ask();
        assert_eq!(runs.get(), 2, "recomputed at the TTL");
    }

    /// A stamp whose newest file is under `RACY_WINDOW` old is not stored; at
    /// exactly the window it is. The boundary is inclusive on the settled side.
    #[test]
    fn the_young_stamp_guard_holds_at_its_boundary() {
        let _held = retention();
        let fx = fixture(None).unwrap();
        let clock = ManualClock::install();
        let young = stamp_aged(&clock, RACY_WINDOW - MILLI);
        assert!(computed(&fx, &["log", "racy-young"], &young));
        assert!(
            computed(&fx, &["log", "racy-young"], &young),
            "2.999 s old is never stored"
        );
        let settled = stamp_aged(&clock, RACY_WINDOW);
        assert!(computed(&fx, &["log", "racy-settled"], &settled));
        assert!(
            !computed(&fx, &["log", "racy-settled"], &settled),
            "3.000 s old is stored"
        );
    }

    /// An NTP step backwards must neither pin a stale answer until the clock
    /// catches up nor panic: an entry stored "in the future" is recomputed.
    #[test]
    fn a_clock_stepped_backwards_neither_panics_nor_serves_a_stale_entry() {
        let _held = retention();
        let fx = fixture(None).unwrap();
        let clock = ManualClock::install();
        clock.advance(Duration::from_secs(600));
        let stamp = stamp_aged(&clock, Duration::from_secs(10));
        let args = ["log", "clock-backwards"];
        assert!(computed(&fx, &args, &stamp));
        assert!(!computed(&fx, &args, &stamp));
        clock.rewind(Duration::from_secs(5));
        assert!(
            computed(&fx, &args, &stamp),
            "an entry newer than now is not trusted"
        );
        clock.rewind(Duration::from_secs(1800));
        assert!(
            computed(&fx, &args, &stamp),
            "a stamp from the future is not stored either"
        );
        clock.advance(Duration::from_secs(1800));
        clock.advance(Duration::from_secs(5));
        assert!(
            !computed(&fx, &args, &stamp),
            "once the clock is back the entry stored after the step is 5 s old and served"
        );
    }

    /// A timestamp from the future is never settled.
    #[test]
    fn a_future_timestamp_is_never_settled() {
        let now = SystemTime::now();
        let stamp = RefStamp {
            entries: Vec::new(),
            newest: now + Duration::from_secs(3600),
        };
        assert!(!stamp.settled(now));
        let old = RefStamp {
            entries: Vec::new(),
            newest: now - Duration::from_secs(3600),
        };
        assert!(old.settled(now));
    }

    const NANO: Duration = Duration::from_nanos(1);

    /// Boundaries to the nanosecond, not the millisecond: the TTL and the young
    /// stamp guard have no float or rounding slack.
    #[test]
    fn adv_ade6_boundaries_are_exact_to_the_nanosecond() {
        let _held = retention();
        let fx = fixture(None).unwrap();
        let clock = ManualClock::install();
        let young = stamp_aged(&clock, RACY_WINDOW - NANO);
        assert!(computed(&fx, &["log", "adv-nano-young"], &young));
        assert!(
            computed(&fx, &["log", "adv-nano-young"], &young),
            "RACY_WINDOW - 1 ns old is stored"
        );
        let settled = stamp_aged(&clock, RACY_WINDOW);
        assert!(computed(&fx, &["log", "adv-nano-settled"], &settled));
        assert!(!computed(&fx, &["log", "adv-nano-settled"], &settled));
        let args = ["log", "adv-nano-ttl"];
        let stamp = stamp_aged(&clock, Duration::from_secs(10));
        assert!(computed(&fx, &args, &stamp));
        clock.advance(TTL - NANO);
        assert!(!computed(&fx, &args, &stamp), "served 1 ns before the TTL");
        clock.advance(NANO);
        assert!(computed(&fx, &args, &stamp), "recomputed at the TTL");
    }

    /// The wall clock steps back an hour while the monotonic clock keeps
    /// going: every mtime then looks "in the future", so nothing is stored
    /// (correct, uncached) and the cache recovers once the wall clock passes
    /// the newest mtime again. It is a bounded outage, not a permanent one.
    #[test]
    fn adv_ade6_a_wall_clock_step_back_refuses_to_store_and_then_recovers() {
        let _held = retention();
        let fx = fixture(None).unwrap();
        let clock = ManualClock::install();
        let stamp = stamp_aged(&clock, Duration::from_secs(10));
        let args = ["log", "adv-skew"];
        assert!(computed(&fx, &args, &stamp));
        assert!(
            !computed(&fx, &args, &stamp),
            "stored while the clocks agree"
        );
        let (mono, wall) = CLOCK.with(std::cell::Cell::get).unwrap();
        CLOCK.with(|cell| {
            cell.set(Some((
                mono + Duration::from_secs(30),
                wall - Duration::from_secs(3600),
            )))
        });
        for _ in 0..3 {
            assert!(
                computed(&fx, &args, &stamp),
                "the stamp looks like it is from the future, so it is recomputed every time"
            );
        }
        CLOCK.with(|cell| {
            cell.set(Some((
                mono + Duration::from_secs(40),
                wall + Duration::from_secs(40),
            )))
        });
        assert!(
            !computed(&fx, &args, &stamp),
            "the entry stored before the step (40 s old) is served again"
        );
        let fresh = ["log", "adv-skew-after"];
        assert!(computed(&fx, &fresh, &stamp));
        assert!(
            !computed(&fx, &fresh, &stamp),
            "storing works again once the wall clock is back"
        );
    }

    /// Single-flight: the leader computes on thread A (which has a manual
    /// clock), a follower on thread B (the real clock) joins that run and gets
    /// the same answer without computing. Then B, asking on its own, is not
    /// served an entry stamped on A's clock.
    #[test]
    fn adv_ade6_a_coalesced_waiter_on_another_thread_gets_the_leaders_answer() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::mpsc;
        let _held = retention();
        let fx = fixture(None).unwrap();
        let repo = fx.main.clone();
        let runs = std::sync::Arc::new(AtomicUsize::new(0));
        let (stamp_tx, stamp_rx) = mpsc::channel::<Stamp>();
        let (started_tx, started_rx) = mpsc::channel::<()>();
        let leader = {
            let repo = repo.clone();
            let runs = runs.clone();
            std::thread::spawn(move || {
                let clock = ManualClock::install();
                let stamp = stamp_aged(&clock, Duration::from_secs(10));
                stamp_tx.send(stamp.clone()).unwrap();
                let value = through_stamped_coalesced(
                    &repo,
                    &["log", "adv-coalesce-thread"],
                    Some(&stamp),
                    || {
                        runs.fetch_add(1, Ordering::SeqCst);
                        let _ = started_rx.recv_timeout(Duration::from_secs(10));
                        std::thread::sleep(Duration::from_millis(300));
                        Ok(("leader-answer".to_string(), true))
                    },
                );
                (value, stamp)
            })
        };
        let stamp = stamp_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        let waiter = {
            let repo = repo.clone();
            let runs = runs.clone();
            let stamp = stamp.clone();
            std::thread::spawn(move || {
                started_tx.send(()).unwrap();
                through_stamped_coalesced(
                    &repo,
                    &["log", "adv-coalesce-thread"],
                    Some(&stamp),
                    || {
                        runs.fetch_add(1, Ordering::SeqCst);
                        Ok(("waiter-answer".to_string(), true))
                    },
                )
            })
        };
        let (led, _) = leader.join().unwrap();
        let waited = waiter.join().unwrap();
        assert_eq!(led.as_deref(), Ok("leader-answer"));
        assert_eq!(
            waited.as_deref(),
            Ok("leader-answer"),
            "the waiter did not share the run"
        );
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        // The entry A stored was stamped on A's clock (an hour ahead of the
        // real one): thread B must neither serve it nor panic on it.
        let again = through_stamped(&repo, &["log", "adv-coalesce-thread"], Some(&stamp), || {
            Ok(("b-recomputed".to_string(), true))
        });
        assert_eq!(again.as_deref(), Ok("b-recomputed"));
    }

    /// A waiter joined to a leader the spawn gate refused is handed the leader's
    /// refusal, and its own command must type it the way the leader's does: the
    /// refusal was written on the leader's thread, and the single-flight carries
    /// that record across with the answer.
    #[test]
    fn a_coalesced_waiter_is_typed_by_the_leaders_refusal() {
        use crate::ipc_error::{run_command_as, IpcErrorKind};
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::mpsc;
        let _held = retention();
        let fx = fixture(None).unwrap();
        let repo = fx.main.clone();
        let runs = std::sync::Arc::new(AtomicUsize::new(0));
        let (stamp_tx, stamp_rx) = mpsc::channel::<Stamp>();
        let (started_tx, started_rx) = mpsc::channel::<()>();
        let leader = {
            let repo = repo.clone();
            let runs = runs.clone();
            std::thread::spawn(move || {
                let clock = ManualClock::install();
                let stamp = stamp_aged(&clock, Duration::from_secs(10));
                stamp_tx.send(stamp.clone()).unwrap();
                run_command_as(Some("cmd_get_file_blame"), || {
                    through_stamped_coalesced(
                        &repo,
                        &["log", "coalesced-refusal"],
                        Some(&stamp),
                        || {
                            runs.fetch_add(1, Ordering::SeqCst);
                            let _ = started_rx.recv_timeout(Duration::from_secs(10));
                            std::thread::sleep(Duration::from_millis(300));
                            Err(git_cli::refusal_message(
                                "git log",
                                git_cli::Refusal::Refused {
                                    waited: Duration::from_millis(2000),
                                },
                            ))
                        },
                    )
                    .map_err(|refused| format!("Blame unavailable: {refused}"))
                })
            })
        };
        let stamp = stamp_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        let waiter = {
            let repo = repo.clone();
            let runs = runs.clone();
            std::thread::spawn(move || {
                started_tx.send(()).unwrap();
                run_command_as(Some("cmd_get_file_blame"), || {
                    through_stamped_coalesced(
                        &repo,
                        &["log", "coalesced-refusal"],
                        Some(&stamp),
                        || {
                            runs.fetch_add(1, Ordering::SeqCst);
                            Ok(("waiter-answer".to_string(), true))
                        },
                    )
                    .map_err(|refused| format!("Blame unavailable: {refused}"))
                })
            })
        };
        let led = leader.join().unwrap().expect_err("the leader was refused");
        let waited = waiter
            .join()
            .unwrap()
            .expect_err("the waiter shares the refusal");
        assert_eq!(
            runs.load(Ordering::SeqCst),
            1,
            "the waiter must have joined the run"
        );
        assert_eq!(led.kind, IpcErrorKind::DeferredUnderLoad, "{led}");
        assert_eq!(waited.kind, IpcErrorKind::DeferredUnderLoad, "{waited}");
    }
}
