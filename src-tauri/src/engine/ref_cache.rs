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
#[derive(Clone, Debug, PartialEq, Eq)]
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
fn through(
    repo: &Path,
    namespaces: &[&str],
    args: &[&str],
    run: impl FnOnce() -> Result<(String, bool), String>,
) -> Result<String, String> {
    crate::repository_trust::require(repo)?;
    let key = Key {
        repo: repo.to_path_buf(),
        argv: args.iter().map(|arg| (*arg).to_string()).collect(),
    };
    let stamp = RefStamp::read(repo, namespaces).filter(|stamp| stamp.settled(SystemTime::now()));
    if let Some(stamp) = &stamp {
        let table = kept().lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(entry) = table.get(&key) {
            if &entry.stamp == stamp && entry.stored_at.elapsed() < TTL {
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
                stored_at: Instant::now(),
                value: value.clone(),
            },
        );
    }
    Ok(value)
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
}

#[cfg(test)]
fn racy_window() -> Duration {
    RACY_OVERRIDE.with(|cell| cell.get()).unwrap_or(RACY_WINDOW)
}

#[cfg(test)]
fn note_hit() {
    HITS.with(|cell| cell.set(cell.get() + 1));
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

    /// Runs `f` with no racy window, so every stamp is stored: what the oracle
    /// then proves is that the stamp itself sees every change, with no help
    /// Held by every test that counts on an answer staying kept, and by the
    /// one that fills the budget: that one evicts whatever else is in the
    /// process-wide table.
    fn retention() -> std::sync::MutexGuard<'static, ()> {
        static SERIAL: Mutex<()> = Mutex::new(());
        SERIAL.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// from the window.
    fn eager<T>(f: impl FnOnce() -> T) -> T {
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
        let base = holder.path().canonicalize().unwrap();
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
            exact(&["symbolic-ref", "--quiet", "--short", "HEAD"]),
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
}
