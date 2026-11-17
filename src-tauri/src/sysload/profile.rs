//! The resource profile: how much this machine can afford GitPulse to hold,
//! derived once from what the sensor already measures.
//!
//! # What it sizes, and what it must not
//!
//! Only **capacity budgets**: how many WebGL terminal contexts may be live,
//! how long a hidden terminal keeps its context, and how many repositories
//! keep their commit-graph rows. These are the measured memory consumers
//! (about 49 MB of graphics memory per terminal context, about 3 MB per
//! cached graph at the default row count). Protocol constants (output window,
//! ring bytes, session and watch counts) and every stored user setting are
//! not derived from it: a smaller machine gets the same behaviour with less
//! kept warm, never a different wire contract.
//!
//! # Where it comes from
//!
//! [`derive`] is pure: installed memory, logical cores and the user's
//! [`ProfileChoice`]. Installed memory is a fact about the machine, not a
//! pressure sample, so it is read directly with [`super::physical_memory`],
//! the one reader the automatic disk floors also scale with (`sysconf` page
//! count times page size on macOS and Linux), and the sampler is not involved:
//! the tier is the same with the "Measure memory pressure" switch off, before
//! the first tick, and on a platform that has no sensor. A memory read that
//! fails (or returns a nonsensical zero), and every platform that reader does
//! not cover (Windows), is not guessed from: the tier is `standard` and the
//! basis says [`ProfileBasis::MemoryUnreadable`], so the page can tell
//! "derived" from "could not tell".
//!
//! The `roomy` budgets are the constants GitPulse shipped with before there
//! was a profile, so a machine with more than 16 GiB behaves as it always did.

use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use super::sensor::{Reading, Why};

const GIB: u64 = 1 << 30;

/// Physical memory up to and including this is `compact`.
pub const COMPACT_MAX_BYTES: u64 = 8 * GIB;
/// Physical memory up to and including this (and above [`COMPACT_MAX_BYTES`])
/// is `standard`; anything larger is `roomy`.
pub const STANDARD_MAX_BYTES: u64 = 16 * GIB;

/// What `host_load_profile` asks for. `Auto` derives the tier from memory.
///
/// An unrecognised stored value reads as `Auto`, so a settings file written
/// by a newer build cannot cost the rest of the `host_load` block.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileChoice {
    Compact,
    Standard,
    Roomy,
    /// Last because serde's `other` must be: it catches an unrecognised value.
    #[default]
    #[serde(other)]
    Auto,
}

/// How much headroom the machine has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileTier {
    Compact,
    Standard,
    Roomy,
}

/// How the tier was decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileBasis {
    /// From a readable physical-memory reading.
    Memory,
    /// The user chose the tier (`host_load_profile`).
    Override,
    /// Memory could not be read and nothing was chosen: the tier is a
    /// default (`standard`), not an answer.
    MemoryUnreadable,
}

/// The capacity budgets one tier allows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ResourceBudgets {
    /// WebGL terminal contexts live at once.
    pub gpu_contexts: u32,
    /// How long a hidden terminal keeps its context, in milliseconds.
    pub gpu_hide_grace_ms: u64,
    /// Repositories whose commit-graph rows stay in memory.
    pub graph_cache_repositories: u32,
}

impl ProfileTier {
    pub const fn budgets(self) -> ResourceBudgets {
        match self {
            Self::Compact => ResourceBudgets {
                gpu_contexts: 2,
                gpu_hide_grace_ms: 15_000,
                graph_cache_repositories: 2,
            },
            Self::Standard => ResourceBudgets {
                gpu_contexts: 4,
                gpu_hide_grace_ms: 60_000,
                graph_cache_repositories: 4,
            },
            // What GitPulse did before it had a profile.
            Self::Roomy => ResourceBudgets {
                gpu_contexts: 8,
                gpu_hide_grace_ms: 5 * 60_000,
                graph_cache_repositories: 6,
            },
        }
    }

    /// The tier physical memory of `bytes` earns.
    pub const fn of_memory(bytes: u64) -> Self {
        if bytes <= COMPACT_MAX_BYTES {
            Self::Compact
        } else if bytes <= STANDARD_MAX_BYTES {
            Self::Standard
        } else {
            Self::Roomy
        }
    }
}

/// What the page and the Rust side read to size themselves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ResourceProfile {
    pub tier: ProfileTier,
    pub basis: ProfileBasis,
    /// Installed physical memory, when it could be read.
    pub memory_bytes: Option<u64>,
    /// Logical cores, when they could be counted.
    pub logical_cores: Option<u16>,
    pub budgets: ResourceBudgets,
}

/// The profile for a machine with `memsize` of installed memory and
/// `logical_cores`, under the user's `choice`. Pure.
pub fn derive(
    memsize: Reading<u64>,
    logical_cores: Option<u16>,
    choice: ProfileChoice,
) -> ResourceProfile {
    // Zero is what a failed sysctl can look like through a wrapper; no
    // machine has it, so it is "unreadable", not "compact".
    let memory_bytes = memsize.value().filter(|bytes| *bytes > 0);
    let logical_cores = logical_cores.filter(|n| *n > 0);
    let (tier, basis) = match (choice, memory_bytes) {
        (ProfileChoice::Compact, _) => (ProfileTier::Compact, ProfileBasis::Override),
        (ProfileChoice::Standard, _) => (ProfileTier::Standard, ProfileBasis::Override),
        (ProfileChoice::Roomy, _) => (ProfileTier::Roomy, ProfileBasis::Override),
        (ProfileChoice::Auto, Some(bytes)) => (ProfileTier::of_memory(bytes), ProfileBasis::Memory),
        (ProfileChoice::Auto, None) => (ProfileTier::Standard, ProfileBasis::MemoryUnreadable),
    };
    ResourceProfile {
        tier,
        basis,
        memory_bytes,
        logical_cores,
        budgets: tier.budgets(),
    }
}

/// [`super::physical_memory`] as a reading: `Unavailable` where it could not
/// be read, or where there is no reader at all.
fn memory() -> Reading<u64> {
    match super::physical_memory() {
        Some(bytes) => Reading::Value(bytes),
        None if cfg!(unix) => Reading::Unavailable(Why::Failed),
        None => Reading::Unavailable(Why::Unsupported),
    }
}

/// Logical cores this process may use, counted once.
fn logical_cores() -> Option<u16> {
    static CORES: OnceLock<Option<u16>> = OnceLock::new();
    *CORES.get_or_init(|| {
        std::thread::available_parallelism()
            .ok()
            .map(|n| u16::try_from(n.get()).unwrap_or(u16::MAX))
    })
}

/// The profile of this machine under `choice`: [`derive`] over what the
/// operating system says it has. Independent of the sampler.
pub fn current(choice: ProfileChoice) -> ResourceProfile {
    derive(memory(), logical_cores(), choice)
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1 << 30;

    fn tier_of(bytes: u64) -> ProfileTier {
        derive(Reading::Value(bytes), None, ProfileChoice::Auto).tier
    }

    #[test]
    fn the_tier_boundaries_are_inclusive_at_8_and_16_gib() {
        assert_eq!(tier_of(8 * GIB - 1), ProfileTier::Compact);
        assert_eq!(tier_of(8 * GIB), ProfileTier::Compact);
        assert_eq!(tier_of(8 * GIB + 1), ProfileTier::Standard);
        assert_eq!(tier_of(16 * GIB), ProfileTier::Standard);
        assert_eq!(tier_of(16 * GIB + 1), ProfileTier::Roomy);
    }

    #[test]
    fn absurd_memory_is_roomy_and_does_not_overflow() {
        assert_eq!(tier_of(u64::MAX), ProfileTier::Roomy);
        assert_eq!(tier_of(1), ProfileTier::Compact);
    }

    #[test]
    fn a_readable_memory_reading_is_a_derived_answer() {
        let p = derive(Reading::Value(8 * GIB), Some(8), ProfileChoice::Auto);
        assert_eq!(p.basis, ProfileBasis::Memory);
        assert_eq!(p.memory_bytes, Some(8 * GIB));
        assert_eq!(p.logical_cores, Some(8));
    }

    #[test]
    fn unreadable_memory_is_standard_and_says_it_was_not_derived() {
        for memsize in [
            Reading::Unavailable(Why::Unreadable),
            Reading::Unavailable(Why::NotYet),
            Reading::Unavailable(Why::Unsupported),
            Reading::Unavailable(Why::Failed),
            // A zero from a failed sysctl is not a machine with no memory.
            Reading::Value(0),
        ] {
            let p = derive(memsize, None, ProfileChoice::Auto);
            assert_eq!(p.tier, ProfileTier::Standard, "{memsize:?}");
            assert_eq!(p.basis, ProfileBasis::MemoryUnreadable, "{memsize:?}");
            assert_eq!(p.memory_bytes, None, "{memsize:?}");
        }
    }

    /// The installed-memory read is the operating system's own answer: it
    /// works with no sampler and no snapshot, and agrees with itself.
    #[cfg(unix)]
    #[test]
    fn installed_memory_is_read_without_a_sampler() {
        let first = memory();
        assert!(
            matches!(first, Reading::Value(bytes) if bytes >= 64 * 1024 * 1024),
            "{first:?}"
        );
        assert_eq!(memory(), first, "read once, remembered");
        let profile = current(ProfileChoice::Auto);
        assert_eq!(profile.basis, ProfileBasis::Memory);
        assert_eq!(profile.memory_bytes, first.value());
        assert_eq!(
            profile.tier,
            ProfileTier::of_memory(first.value().unwrap_or(0))
        );
        assert!(profile.logical_cores.is_some_and(|n| n > 0));
    }

    #[test]
    fn each_override_wins_over_memory_and_over_unreadable_memory() {
        for (choice, tier) in [
            (ProfileChoice::Compact, ProfileTier::Compact),
            (ProfileChoice::Standard, ProfileTier::Standard),
            (ProfileChoice::Roomy, ProfileTier::Roomy),
        ] {
            for memsize in [
                Reading::Value(64 * GIB),
                Reading::Value(4 * GIB),
                Reading::Unavailable(Why::Unreadable),
            ] {
                let p = derive(memsize, None, choice);
                assert_eq!(p.tier, tier);
                assert_eq!(p.basis, ProfileBasis::Override);
            }
        }
    }

    #[test]
    fn roomy_is_what_shipped_before_there_was_a_profile() {
        // MAX_VISIBLE_SESSIONS (src/lib/terminal/attendance.ts), GPU_HIDE_GRACE_MS
        // (gpuRenderer.ts) and GRAPH_CACHE_LIMIT (graphLimits.ts); the vitest
        // contract in resourceBudgets.test.ts reads this file to keep them equal.
        let roomy = ProfileTier::Roomy.budgets();
        assert_eq!(roomy.gpu_contexts, 8);
        assert_eq!(roomy.gpu_hide_grace_ms, 300_000);
        assert_eq!(roomy.graph_cache_repositories, 6);
    }

    #[test]
    fn budgets_never_grow_as_the_machine_shrinks() {
        let (c, s, r) = (
            ProfileTier::Compact.budgets(),
            ProfileTier::Standard.budgets(),
            ProfileTier::Roomy.budgets(),
        );
        assert!(c.gpu_contexts < s.gpu_contexts && s.gpu_contexts < r.gpu_contexts);
        assert!(
            c.gpu_hide_grace_ms < s.gpu_hide_grace_ms && s.gpu_hide_grace_ms < r.gpu_hide_grace_ms
        );
        assert!(
            c.graph_cache_repositories < s.graph_cache_repositories
                && s.graph_cache_repositories < r.graph_cache_repositories
        );
    }

    #[test]
    fn the_choice_reads_snake_case_and_an_unknown_value_is_auto() {
        let read = |s: &str| serde_json::from_str::<ProfileChoice>(s).unwrap();
        assert_eq!(read("\"auto\""), ProfileChoice::Auto);
        assert_eq!(read("\"compact\""), ProfileChoice::Compact);
        assert_eq!(read("\"standard\""), ProfileChoice::Standard);
        assert_eq!(read("\"roomy\""), ProfileChoice::Roomy);
        assert_eq!(read("\"tiny\""), ProfileChoice::Auto);
    }

    #[test]
    fn the_profile_serialises_the_shape_the_page_reads() {
        let p = derive(Reading::Value(8 * GIB), None, ProfileChoice::Auto);
        let json = serde_json::to_value(p).unwrap();
        assert_eq!(json["tier"], "compact");
        assert_eq!(json["basis"], "memory");
        assert_eq!(json["memory_bytes"], 8 * GIB);
        assert_eq!(json["logical_cores"], serde_json::Value::Null);
        assert_eq!(json["budgets"]["gpu_contexts"], 2);
        assert_eq!(json["budgets"]["gpu_hide_grace_ms"], 15_000);
        assert_eq!(json["budgets"]["graph_cache_repositories"], 2);
    }
}
