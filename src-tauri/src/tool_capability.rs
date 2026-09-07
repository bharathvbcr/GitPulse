//! Process-wide capability cache for optional CLIs.
//!
//! Caches both positive and negative answers. A tool that will never be found
//! was previously re-searched on every save burst / Map reload; caching the
//! *absent* answer is the actual fix. Invalidated on install, config change,
//! and explicit refresh.

use crate::tool_install::ExternalTool;
use serde::{Deserialize, Serialize};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

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

#[cfg(test)]
mod tests {
    use super::*;

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
