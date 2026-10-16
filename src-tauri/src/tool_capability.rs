//! Process-wide capability cache for optional CLIs.
//!
//! Caches both positive and negative answers. A tool that will never be found
//! was previously re-searched on every save burst / Map reload; caching the
//! *absent* answer is the actual fix. Invalidated on install, config change,
//! and explicit refresh; a found path also ends when the file at it changes
//! or goes, and an absent answer ends after [`ABSENT_TTL`].

use crate::tool_install::ExternalTool;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityAnswer {
    Present { path: String },
    Absent { reason: String },
}

/// How long a cached "not found" is believed. A tool installed from a terminal
/// is picked up within this window; GitPulse's own installer and config changes
/// still invalidate at once. The probe cannot tell a transient failure from a
/// real absence, so this bounds how long either is remembered.
pub(crate) const ABSENT_TTL: Duration = Duration::from_secs(60);

/// One cached answer. A found tool carries the stamp of the file it was found
/// at, so the answer ends when that file is replaced or removed; a missing tool
/// carries when it was asked, so the answer ends after [`ABSENT_TTL`].
#[derive(Debug, Clone)]
enum Entry {
    Present { path: String, stamp: BinaryStamp },
    Absent { reason: String, at: Instant },
}

impl Entry {
    fn answer(&self) -> CapabilityAnswer {
        match self {
            Self::Present { path, .. } => CapabilityAnswer::Present { path: path.clone() },
            Self::Absent { reason, .. } => CapabilityAnswer::Absent {
                reason: reason.clone(),
            },
        }
    }
}

/// The per-tool answers plus one gate per tool that lets a single thread
/// re-probe an expired entry while the others wait for its answer.
struct CapabilityCache {
    slots: Mutex<Slots>,
    gates: [Mutex<()>; 2],
}

/// The answers and, per tool, how many times they have been invalidated. A
/// probe remembers the epoch it started under and stores nothing if an
/// invalidation passed while it ran, so an answer read before an install is
/// never written back after it.
struct Slots {
    entries: [Option<Entry>; 2],
    epochs: [u64; 2],
}

impl CapabilityCache {
    const fn new() -> Self {
        Self {
            slots: Mutex::new(Slots {
                entries: [None, None],
                epochs: [0, 0],
            }),
            gates: [Mutex::new(()), Mutex::new(())],
        }
    }

    fn slots(&self) -> std::sync::MutexGuard<'_, Slots> {
        self.slots
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The live answer for `tool`: a stale one is dropped, never returned. A
    /// hit on a found tool costs one `stat`, taken outside the lock.
    fn get(&self, tool: ExternalTool, now: impl FnOnce() -> Instant) -> Option<CapabilityAnswer> {
        let entry = self.slots().entries[idx(tool)].clone()?;
        let live = match &entry {
            Entry::Present { path, stamp } => stamp_of(Path::new(path)).as_ref() == Some(stamp),
            Entry::Absent { at, .. } => now().saturating_duration_since(*at) < ABSENT_TTL,
        };
        if live {
            return Some(entry.answer());
        }
        // Evict only the entry that was judged stale: another thread may have
        // replaced it with a fresh answer in the meantime.
        let mut slots = self.slots();
        if slots.entries[idx(tool)]
            .as_ref()
            .is_some_and(|held| held.answer() == entry.answer())
        {
            slots.entries[idx(tool)] = None;
        }
        None
    }

    fn set(&self, tool: ExternalTool, answer: CapabilityAnswer, at: Instant) {
        let epoch = self.slots().epochs[idx(tool)];
        self.set_in_epoch(tool, answer, at, epoch);
    }

    /// Stores `answer` unless `tool` was invalidated since `epoch` was read.
    fn set_in_epoch(&self, tool: ExternalTool, answer: CapabilityAnswer, at: Instant, epoch: u64) {
        // A found path that is not a regular file right now has no stamp to
        // validate against, so it is not kept: it is asked again every time.
        let entry = match answer {
            CapabilityAnswer::Present { path } => {
                stamp_of(Path::new(&path)).map(|stamp| Entry::Present { path, stamp })
            }
            CapabilityAnswer::Absent { reason } => Some(Entry::Absent { reason, at }),
        };
        let mut slots = self.slots();
        if slots.epochs[idx(tool)] == epoch {
            slots.entries[idx(tool)] = entry;
        }
    }

    fn invalidate(&self, tool: ExternalTool) {
        let mut slots = self.slots();
        slots.entries[idx(tool)] = None;
        slots.epochs[idx(tool)] += 1;
    }

    fn invalidate_all(&self) {
        let mut slots = self.slots();
        slots.entries = [None, None];
        for epoch in &mut slots.epochs {
            *epoch += 1;
        }
    }

    /// Miss or stale: one thread probes, the rest wait on the gate and are
    /// answered from what it stored, so an expired entry costs one probe.
    fn resolve(
        &self,
        tool: ExternalTool,
        now: impl Fn() -> Instant,
        probe: impl FnOnce() -> Result<String, String>,
    ) -> Result<String, String> {
        let answered = |answer| match answer {
            CapabilityAnswer::Present { path } => Ok(path),
            CapabilityAnswer::Absent { reason } => Err(reason),
        };
        if let Some(answer) = self.get(tool, &now) {
            return answered(answer);
        }
        let _flight = self.gates[idx(tool)]
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(answer) = self.get(tool, &now) {
            return answered(answer);
        }
        let epoch = self.slots().epochs[idx(tool)];
        let result = probe();
        let answer = match &result {
            Ok(path) => CapabilityAnswer::Present { path: path.clone() },
            Err(reason) => CapabilityAnswer::Absent {
                reason: reason.clone(),
            },
        };
        self.set_in_epoch(tool, answer, now(), epoch);
        result
    }
}

static CACHE: CapabilityCache = CapabilityCache::new();

fn idx(tool: ExternalTool) -> usize {
    match tool {
        ExternalTool::Devmap => 0,
        ExternalTool::Manvi => 1,
    }
}

pub fn get(tool: ExternalTool) -> Option<CapabilityAnswer> {
    CACHE.get(tool, Instant::now)
}

pub fn set(tool: ExternalTool, answer: CapabilityAnswer) {
    CACHE.set(tool, answer, Instant::now());
}

pub fn invalidate(tool: ExternalTool) {
    CACHE.invalidate(tool);
}

pub fn invalidate_all() {
    CACHE.invalidate_all();
}

/// Resolve through the cache: miss or stale, probe, store (including Absent).
pub fn resolve_cached(
    tool: ExternalTool,
    probe: impl FnOnce() -> Result<String, String>,
) -> Result<String, String> {
    CACHE.resolve(tool, Instant::now, probe)
}

/// What a binary on disk looked like when it was probed: replacing or
/// upgrading it changes one of these, which is what ends the cached answer.
#[derive(Debug, Clone, PartialEq, Eq)]
struct BinaryStamp {
    len: u64,
    modified: Option<SystemTime>,
}

/// The stamp of the regular file at an absolute `path` (symlinks followed), or
/// `None` when there is no such file: a bare name, a directory, a dangling
/// link. The one place a binary is stamped, for both caches in this file.
fn stamp_of(path: &Path) -> Option<BinaryStamp> {
    path.is_absolute()
        .then(|| std::fs::metadata(path).ok())
        .flatten()
        .filter(std::fs::Metadata::is_file)
        .map(|meta| BinaryStamp {
            len: meta.len(),
            modified: meta.modified().ok(),
        })
}

/// Probe answers for CLI binaries (`gh --version`, an agent's `--version` and
/// `--help`), keyed by the resolved binary path and stamped with its size and
/// mtime.
///
/// These probes ran before every GitHub call — including the live run poll —
/// and before every agent launch, each a process spawn for an answer that
/// changes only when the binary does. Only answers the caller marks cacheable
/// are kept: a probe that was shed under load, or that started and failed,
/// may succeed a moment later and must be asked again. A program that does not
/// resolve to a regular file is never cached either, so a tool installed after
/// a miss is seen at once.
pub(crate) struct BinaryProbeCache<T> {
    entries: OnceLock<Mutex<HashMap<PathBuf, (BinaryStamp, T)>>>,
}

/// More distinct binaries than a machine has; past it the cache starts over
/// rather than growing with every path ever probed.
const MAX_PROBED_BINARIES: usize = 16;

impl<T: Clone> BinaryProbeCache<T> {
    pub(crate) const fn new() -> Self {
        Self {
            entries: OnceLock::new(),
        }
    }

    /// Forgets every answer, so a test that forces a probe to fail is not
    /// served an earlier test's cached success.
    #[cfg(test)]
    pub(crate) fn clear(&self) {
        if let Some(entries) = self.entries.get() {
            entries
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clear();
        }
    }

    /// The cached answer for `program` if the binary is unchanged, else
    /// `probe()`'s. `probe` returns the answer and whether it may be kept.
    pub(crate) fn get_or_probe(&self, program: &str, probe: impl FnOnce() -> (T, bool)) -> T {
        // A test that forces spawns to fail on this thread must reach the
        // spawn: the refusal is thread-local and this cache is shared, so any
        // other test's earlier success would otherwise answer past it. What
        // is probed under the refusal is not kept either.
        #[cfg(test)]
        if crate::engine::git_cli::forced_failure_active() {
            return probe().0;
        }
        let path = PathBuf::from(program);
        let stamp = stamp_of(&path);
        let Some(stamp) = stamp else {
            return probe().0;
        };
        let entries = self.entries.get_or_init(|| Mutex::new(HashMap::new()));
        if let Some((seen, answer)) = entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&path)
        {
            if *seen == stamp {
                return answer.clone();
            }
        }
        // Not under the lock: a probe is a process spawn.
        let (answer, cacheable) = probe();
        if cacheable {
            let mut map = entries
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if map.len() >= MAX_PROBED_BINARIES && !map.contains_key(&path) {
                map.clear();
            }
            map.insert(path, (stamp, answer.clone()));
        }
        answer
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_probe_is_kept_until_the_binary_changes_and_only_when_cacheable() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("tool");
        std::fs::write(&bin, "v1").unwrap();
        let program = bin.to_str().unwrap();
        let cache: BinaryProbeCache<u32> = BinaryProbeCache::new();
        let mut probes = 0u32;
        let mut ask = |cacheable: bool| {
            cache.get_or_probe(program, || {
                probes += 1;
                (probes, cacheable)
            })
        };
        // Not cacheable (a probe shed under load): asked again every time.
        assert_eq!((ask(false), ask(false)), (1, 2));
        assert_eq!(
            (ask(true), ask(true), ask(true)),
            (3, 3, 3),
            "kept once cacheable"
        );
        // A different binary at the same path is a different answer.
        std::fs::write(&bin, "version two").unwrap();
        assert_eq!(ask(true), 4);
        // A bare name that resolves to no file is never cached.
        let miss: BinaryProbeCache<u32> = BinaryProbeCache::new();
        assert_eq!(
            miss.get_or_probe("definitely-not-installed", || (1, true)),
            1
        );
        assert_eq!(
            miss.get_or_probe("definitely-not-installed", || (2, true)),
            2
        );
    }

    /// The spawn-failure seam is thread-local and this cache is shared: a
    /// success another thread left behind must not answer for a thread that
    /// has forced its spawns to fail, and what is probed under the refusal
    /// must not be left behind for the next thread.
    #[test]
    fn a_forced_spawn_failure_reaches_the_probe_whatever_the_cache_holds() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("tool");
        std::fs::write(&bin, "v1").unwrap();
        let program = bin.to_str().unwrap();
        let cache: BinaryProbeCache<&'static str> = BinaryProbeCache::new();
        assert_eq!(cache.get_or_probe(program, || ("real", true)), "real");
        let forced = crate::engine::git_cli::with_forced_spawn_failure(|| {
            cache.get_or_probe(program, || ("refused", true))
        });
        assert_eq!(forced, "refused", "the warm entry answered past the seam");
        assert_eq!(
            cache.get_or_probe(program, || ("asked again", true)),
            "real",
            "a probe made under the refusal replaced the real answer"
        );
        cache.clear();
        let _ = crate::engine::git_cli::with_forced_spawn_failure(|| {
            cache.get_or_probe(program, || ("refused", true))
        });
        assert_eq!(
            cache.get_or_probe(program, || ("real", true)),
            "real",
            "a probe made under the refusal was kept for other threads"
        );
    }

    #[test]
    fn caches_negative_answer() {
        invalidate_all();
        let mut probes = 0;
        let first = resolve_cached(ExternalTool::Devmap, || {
            probes += 1;
            Err("missing".into())
        });
        assert!(first.is_err());
        assert_eq!(probes, 1);
        let second = resolve_cached(ExternalTool::Devmap, || {
            probes += 1;
            Err("should not run".into())
        });
        assert_eq!(second.unwrap_err(), "missing");
        assert_eq!(probes, 1, "negative answer must be cached");
        invalidate(ExternalTool::Devmap);
        let third = resolve_cached(ExternalTool::Devmap, || {
            probes += 1;
            Ok("/bin/devmap".into())
        });
        assert_eq!(third.unwrap(), "/bin/devmap");
        assert_eq!(probes, 2);
        invalidate_all();
    }

    #[test]
    fn invalidate_all_clears_both() {
        invalidate_all();
        set(
            ExternalTool::Devmap,
            CapabilityAnswer::Present { path: "/a".into() },
        );
        set(
            ExternalTool::Manvi,
            CapabilityAnswer::Absent {
                reason: "no".into(),
            },
        );
        invalidate_all();
        assert!(get(ExternalTool::Devmap).is_none());
        assert!(get(ExternalTool::Manvi).is_none());
    }

    /// A clock the test moves by hand: every timing assertion below goes
    /// through it, so none depends on how long the machine takes.
    struct Clock {
        base: Instant,
        offset_ms: std::sync::atomic::AtomicU64,
    }

    impl Clock {
        fn new() -> Self {
            Self {
                base: Instant::now(),
                offset_ms: std::sync::atomic::AtomicU64::new(0),
            }
        }
        fn now(&self) -> Instant {
            self.base
                + Duration::from_millis(self.offset_ms.load(std::sync::atomic::Ordering::SeqCst))
        }
        fn advance(&self, by: Duration) {
            let ms = u64::try_from(by.as_millis()).unwrap();
            self.offset_ms
                .fetch_add(ms, std::sync::atomic::Ordering::SeqCst);
        }
    }

    fn stub(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        path
    }

    fn found(path: &Path) -> Result<String, String> {
        Ok(path.to_str().unwrap().to_string())
    }

    fn present(path: &Path) -> CapabilityAnswer {
        CapabilityAnswer::Present {
            path: path.to_str().unwrap().into(),
        }
    }

    #[test]
    fn an_absent_answer_expires_at_the_ttl_and_the_tool_is_found() {
        let cache = CapabilityCache::new();
        let clock = Clock::new();
        let dir = tempfile::tempdir().unwrap();
        let mut probes = 0;
        let miss = cache.resolve(
            ExternalTool::Devmap,
            || clock.now(),
            || {
                probes += 1;
                Err("not installed".into())
            },
        );
        assert_eq!(miss.unwrap_err(), "not installed");
        // Installed from a terminal while the entry is cached.
        let bin = stub(dir.path(), "devmap", "v1");
        clock.advance(ABSENT_TTL - Duration::from_millis(1));
        let still = cache.resolve(
            ExternalTool::Devmap,
            || clock.now(),
            || {
                probes += 1;
                found(&bin)
            },
        );
        assert_eq!(
            (still.unwrap_err(), probes),
            ("not installed".to_string(), 1),
            "one millisecond inside the TTL the answer is kept"
        );
        clock.advance(Duration::from_millis(1));
        let now_found = cache.resolve(
            ExternalTool::Devmap,
            || clock.now(),
            || {
                probes += 1;
                found(&bin)
            },
        );
        assert_eq!(
            (now_found.unwrap(), probes),
            (bin.to_str().unwrap().to_string(), 2),
            "exactly at the TTL the answer has ended and the tool is re-probed"
        );
    }

    #[test]
    fn a_transient_probe_error_is_remembered_only_for_the_ttl() {
        let cache = CapabilityCache::new();
        let clock = Clock::new();
        let dir = tempfile::tempdir().unwrap();
        let bin = stub(dir.path(), "manvi", "v1");
        let mut probes = 0;
        let mut ask = |fail: bool| {
            cache.resolve(
                ExternalTool::Manvi,
                || clock.now(),
                || {
                    probes += 1;
                    if fail {
                        Err("spawn failed: EAGAIN".into())
                    } else {
                        found(&bin)
                    }
                },
            )
        };
        assert!(ask(true).is_err());
        assert!(ask(false).is_err(), "the failure is cached inside the TTL");
        clock.advance(ABSENT_TTL);
        assert!(ask(false).is_ok(), "and is gone once the TTL has passed");
        assert_eq!(probes, 2);
    }

    #[test]
    fn a_present_answer_is_dropped_when_the_binary_is_deleted() {
        let cache = CapabilityCache::new();
        let clock = Clock::new();
        let dir = tempfile::tempdir().unwrap();
        let bin = stub(dir.path(), "devmap", "v1");
        let probes = std::cell::Cell::new(0);
        let ask = || {
            cache.resolve(
                ExternalTool::Devmap,
                || clock.now(),
                || {
                    probes.set(probes.get() + 1);
                    found(&bin)
                },
            )
        };
        assert!(ask().is_ok());
        assert!(ask().is_ok());
        assert_eq!(
            probes.get(),
            1,
            "an unchanged binary is served from the cache"
        );
        std::fs::remove_file(&bin).unwrap();
        assert_eq!(ask().unwrap(), bin.to_str().unwrap());
        assert_eq!(
            probes.get(),
            2,
            "the deleted binary was not served from the cache"
        );
        assert_eq!(
            cache.get(ExternalTool::Devmap, || clock.now()),
            None,
            "a path that is not a file is not kept"
        );
    }

    #[test]
    fn a_present_answer_is_dropped_when_the_length_changes() {
        let cache = CapabilityCache::new();
        let dir = tempfile::tempdir().unwrap();
        let bin = stub(dir.path(), "devmap", "v1");
        cache.set(ExternalTool::Devmap, present(&bin), Instant::now());
        assert!(cache.get(ExternalTool::Devmap, Instant::now).is_some());
        std::fs::write(&bin, "a longer v2").unwrap();
        assert_eq!(cache.get(ExternalTool::Devmap, Instant::now), None);
    }

    #[test]
    fn a_replacement_of_the_same_length_with_a_new_mtime_is_seen() {
        let cache = CapabilityCache::new();
        let dir = tempfile::tempdir().unwrap();
        let bin = stub(dir.path(), "devmap", "v1");
        cache.set(ExternalTool::Devmap, present(&bin), Instant::now());
        std::fs::write(&bin, "v2").unwrap();
        let file = std::fs::File::options().write(true).open(&bin).unwrap();
        file.set_modified(SystemTime::now() + Duration::from_secs(3600))
            .unwrap();
        assert_eq!(
            std::fs::metadata(&bin).unwrap().len(),
            2,
            "same length as before"
        );
        assert_eq!(cache.get(ExternalTool::Devmap, Instant::now), None);
    }

    #[cfg(unix)]
    #[test]
    fn a_binary_replaced_by_a_symlink_to_another_file_is_seen() {
        let cache = CapabilityCache::new();
        let dir = tempfile::tempdir().unwrap();
        let bin = stub(dir.path(), "devmap", "v1");
        let other = stub(dir.path(), "devmap-next", "a different, longer binary");
        cache.set(ExternalTool::Devmap, present(&bin), Instant::now());
        std::fs::remove_file(&bin).unwrap();
        std::os::unix::fs::symlink(&other, &bin).unwrap();
        assert_eq!(
            cache.get(ExternalTool::Devmap, Instant::now),
            None,
            "the link points at a file with another stamp"
        );
        // A link that dangles is no file at all.
        cache.set(ExternalTool::Devmap, present(&bin), Instant::now());
        assert!(cache.get(ExternalTool::Devmap, Instant::now).is_some());
        std::fs::remove_file(&other).unwrap();
        assert_eq!(cache.get(ExternalTool::Devmap, Instant::now), None);
    }

    #[test]
    fn a_path_that_becomes_a_directory_is_dropped_and_never_kept() {
        let cache = CapabilityCache::new();
        let dir = tempfile::tempdir().unwrap();
        let bin = stub(dir.path(), "devmap", "v1");
        cache.set(ExternalTool::Devmap, present(&bin), Instant::now());
        std::fs::remove_file(&bin).unwrap();
        std::fs::create_dir(&bin).unwrap();
        assert_eq!(cache.get(ExternalTool::Devmap, Instant::now), None);
        cache.set(ExternalTool::Devmap, present(&bin), Instant::now());
        assert_eq!(
            cache.get(ExternalTool::Devmap, Instant::now),
            None,
            "a directory has no stamp to hold an answer against"
        );
        cache.set(
            ExternalTool::Devmap,
            CapabilityAnswer::Present {
                path: "devmap".into(),
            },
            Instant::now(),
        );
        assert_eq!(
            cache.get(ExternalTool::Devmap, Instant::now),
            None,
            "a bare name is searched for again every time"
        );
    }

    #[test]
    fn a_stale_entry_costs_one_probe_however_many_threads_arrive() {
        let cache = CapabilityCache::new();
        let clock = Clock::new();
        cache.set(
            ExternalTool::Manvi,
            CapabilityAnswer::Absent {
                reason: "old".into(),
            },
            clock.now(),
        );
        clock.advance(ABSENT_TTL);
        let dir = tempfile::tempdir().unwrap();
        let bin = stub(dir.path(), "manvi", "v1");
        let probes = std::sync::atomic::AtomicUsize::new(0);
        let start = std::sync::Barrier::new(16);
        std::thread::scope(|scope| {
            for _ in 0..16 {
                scope.spawn(|| {
                    start.wait();
                    let got = cache.resolve(
                        ExternalTool::Manvi,
                        || clock.now(),
                        || {
                            probes.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            // Long enough that every other thread is at the
                            // gate, so a missing gate cannot pass by luck.
                            std::thread::sleep(Duration::from_millis(50));
                            found(&bin)
                        },
                    );
                    assert_eq!(got.unwrap(), bin.to_str().unwrap());
                });
            }
        });
        assert_eq!(probes.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn invalidation_still_ends_both_kinds_of_answer_at_once() {
        let cache = CapabilityCache::new();
        let dir = tempfile::tempdir().unwrap();
        let bin = stub(dir.path(), "devmap", "v1");
        cache.set(ExternalTool::Devmap, present(&bin), Instant::now());
        cache.set(
            ExternalTool::Manvi,
            CapabilityAnswer::Absent {
                reason: "no".into(),
            },
            Instant::now(),
        );
        cache.invalidate(ExternalTool::Devmap);
        assert_eq!(cache.get(ExternalTool::Devmap, Instant::now), None);
        assert!(cache.get(ExternalTool::Manvi, Instant::now).is_some());
        cache.invalidate_all();
        assert_eq!(cache.get(ExternalTool::Manvi, Instant::now), None);
    }

    /// A probe that panics holds the gate while it unwinds. The next caller
    /// must get a probe of its own, not a poisoned gate or a hang.
    #[test]
    fn a_panicking_probe_does_not_wedge_the_gate() {
        let cache = CapabilityCache::new();
        let clock = Clock::new();
        let panicked = std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    let _ = cache.resolve(ExternalTool::Devmap, || clock.now(), || panic!("boom"));
                })
                .join()
        });
        assert!(panicked.is_err(), "the probe panic reaches its own caller");
        let dir = tempfile::tempdir().unwrap();
        let bin = stub(dir.path(), "devmap", "v1");
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                tx.send(cache.resolve(ExternalTool::Devmap, || clock.now(), || found(&bin)))
                    .unwrap();
            });
            let got = rx
                .recv_timeout(crate::test_support::coverage_relaxed(Duration::from_secs(
                    5,
                )))
                .expect("a caller after a panicked probe is not left waiting");
            assert_eq!(got.unwrap(), bin.to_str().unwrap());
        });
    }

    /// Waiters queue behind a running probe and read its answer; they are not
    /// answered early and not left behind once it ends.
    #[test]
    fn callers_wait_for_a_slow_probe_and_share_its_answer() {
        let cache = CapabilityCache::new();
        let clock = Clock::new();
        let dir = tempfile::tempdir().unwrap();
        let bin = stub(dir.path(), "devmap", "v1");
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let release_rx = Mutex::new(release_rx);
        let probes = std::sync::atomic::AtomicUsize::new(0);
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                let _ = cache.resolve(
                    ExternalTool::Devmap,
                    || clock.now(),
                    || {
                        probes.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        // Bounded, so a failing assertion below cannot leave
                        // this thread waiting for a release that never comes.
                        let _ = release_rx.lock().unwrap().recv_timeout(
                            crate::test_support::coverage_relaxed(Duration::from_secs(5)),
                        );
                        found(&bin)
                    },
                );
            });
            while probes.load(std::sync::atomic::Ordering::SeqCst) == 0 {
                std::thread::yield_now();
            }
            scope.spawn(|| {
                tx.send(cache.resolve(
                    ExternalTool::Devmap,
                    || clock.now(),
                    || {
                        probes.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        found(&bin)
                    },
                ))
                .unwrap();
            });
            let early = rx
                .recv_timeout(crate::test_support::coverage_relaxed(
                    Duration::from_millis(100),
                ))
                .is_ok();
            release_tx.send(()).unwrap();
            assert!(
                !early,
                "the waiter was answered while the probe was still running"
            );
            let got = rx
                .recv_timeout(crate::test_support::coverage_relaxed(Duration::from_secs(
                    5,
                )))
                .expect("the waiter is answered once the probe ends");
            assert_eq!(got.unwrap(), bin.to_str().unwrap());
        });
        assert_eq!(probes.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    /// A probe slower than the TTL stamps its Absent answer when it ends, not
    /// when it began, so the answer is born live and 16 callers cost one probe.
    #[test]
    fn a_probe_slower_than_the_ttl_still_yields_a_live_absent_answer() {
        let cache = CapabilityCache::new();
        let clock = Clock::new();
        let probes = std::sync::atomic::AtomicUsize::new(0);
        let start = std::sync::Barrier::new(16);
        std::thread::scope(|scope| {
            for _ in 0..16 {
                scope.spawn(|| {
                    start.wait();
                    let got = cache.resolve(
                        ExternalTool::Manvi,
                        || clock.now(),
                        || {
                            probes.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            clock.advance(ABSENT_TTL + Duration::from_secs(10));
                            Err("slow miss".into())
                        },
                    );
                    assert_eq!(got.unwrap_err(), "slow miss");
                });
            }
        });
        assert_eq!(probes.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    /// An invalidation that lands while a probe is running ends the answer the
    /// probe is about to store: the install it announces may postdate what the
    /// probe saw.
    #[test]
    fn an_invalidation_during_a_probe_is_not_overwritten_by_its_answer() {
        for all in [false, true] {
            let cache = CapabilityCache::new();
            let clock = Clock::new();
            let inside = std::sync::Barrier::new(2);
            let release = std::sync::Barrier::new(2);
            std::thread::scope(|scope| {
                scope.spawn(|| {
                    let got = cache.resolve(
                        ExternalTool::Devmap,
                        || clock.now(),
                        || {
                            inside.wait();
                            release.wait();
                            Err("not installed (read before the install)".into())
                        },
                    );
                    assert!(
                        got.is_err(),
                        "the in-flight caller still gets its own answer"
                    );
                });
                inside.wait();
                if all {
                    cache.invalidate_all();
                } else {
                    cache.invalidate(ExternalTool::Devmap);
                }
                release.wait();
            });
            assert_eq!(
                cache.get(ExternalTool::Devmap, || clock.now()),
                None,
                "a pre-invalidation answer was stored after the invalidation (all={all})"
            );
        }
    }

    /// Stated limit, shared with `BinaryProbeCache`: a rewrite that keeps the
    /// length and puts the mtime back is not a change the stamp can see.
    #[test]
    fn a_rewrite_that_restores_length_and_mtime_is_the_stamps_known_blind_spot() {
        let cache = CapabilityCache::new();
        let dir = tempfile::tempdir().unwrap();
        let bin = stub(dir.path(), "devmap", "v1");
        let before = std::fs::metadata(&bin).unwrap().modified().unwrap();
        cache.set(ExternalTool::Devmap, present(&bin), Instant::now());
        std::fs::write(&bin, "v2").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&bin)
            .unwrap()
            .set_modified(before)
            .unwrap();
        assert!(
            cache.get(ExternalTool::Devmap, Instant::now).is_some(),
            "len and mtime both match, so the entry is kept"
        );
    }
}
