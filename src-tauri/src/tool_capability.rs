//! Process-wide capability cache for optional CLIs.
//!
//! Caches both positive and negative answers. A tool that will never be found
//! was previously re-searched on every save burst / Map reload; caching the
//! *absent* answer is the actual fix. Invalidated on install, config change,
//! and explicit refresh.

use crate::tool_install::ExternalTool;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Instant, SystemTime};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityAnswer {
    Present { path: String },
    Absent { reason: String },
}

#[derive(Debug, Clone)]
struct Cached {
    answer: CapabilityAnswer,
    #[allow(dead_code)]
    at: Instant,
}

fn store() -> &'static Mutex<[Option<Cached>; 2]> {
    static STORE: OnceLock<Mutex<[Option<Cached>; 2]>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new([None, None]))
}

fn idx(tool: ExternalTool) -> usize {
    match tool {
        ExternalTool::Devmap => 0,
        ExternalTool::Manvi => 1,
    }
}

pub fn get(tool: ExternalTool) -> Option<CapabilityAnswer> {
    let guard = store()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    guard[idx(tool)].as_ref().map(|c| c.answer.clone())
}

pub fn set(tool: ExternalTool, answer: CapabilityAnswer) {
    let mut guard = store()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    guard[idx(tool)] = Some(Cached {
        answer,
        at: Instant::now(),
    });
}

pub fn invalidate(tool: ExternalTool) {
    let mut guard = store()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    guard[idx(tool)] = None;
}

pub fn invalidate_all() {
    let mut guard = store()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    guard[0] = None;
    guard[1] = None;
}

/// Resolve through the cache: miss → probe → store (including Absent).
pub fn resolve_cached(
    tool: ExternalTool,
    probe: impl FnOnce() -> Result<String, String>,
) -> Result<String, String> {
    if let Some(cached) = get(tool) {
        return match cached {
            CapabilityAnswer::Present { path } => Ok(path),
            CapabilityAnswer::Absent { reason } => Err(reason),
        };
    }
    match probe() {
        Ok(path) => {
            set(tool, CapabilityAnswer::Present { path: path.clone() });
            Ok(path)
        }
        Err(reason) => {
            set(
                tool,
                CapabilityAnswer::Absent {
                    reason: reason.clone(),
                },
            );
            Err(reason)
        }
    }
}

/// What a binary on disk looked like when it was probed: replacing or
/// upgrading it changes one of these, which is what ends the cached answer.
#[derive(Debug, Clone, PartialEq, Eq)]
struct BinaryStamp {
    len: u64,
    modified: Option<SystemTime>,
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
        let stamp = path
            .is_absolute()
            .then(|| std::fs::metadata(&path).ok())
            .flatten()
            .filter(std::fs::Metadata::is_file)
            .map(|meta| BinaryStamp {
                len: meta.len(),
                modified: meta.modified().ok(),
            });
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
}
