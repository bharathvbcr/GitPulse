//! Host load: what this machine is carrying right now, measured in one place
//! and published for every GitPulse process to read.
//!
//! # Why this exists
//!
//! The Mac this was written on went down three times in a week, each time
//! under one runaway process (a 120 GiB `go test`, a 74.8 GiB Python, a
//! 40.8 GiB `rg`), with a compressor at its segment limit and agents still
//! being launched into it. Nothing in GitPulse could see any of that: launches
//! were admitted on a session count alone, and the session count itself came
//! from one Claude Code registry directory, so it undercounted.
//!
//! # What this module owns, and what it does not
//!
//! The sensor **measures and publishes**. It never kills a process for its
//! footprint and never starts or restarts memguard. What to do about a
//! reading is decided in one place, `governor`: its verdict refuses agent
//! launches (card 1b, [`admit`]) and sheds GitPulse's own background and
//! reactive work through the spawn gate's existing classes (1c, [`shed`]).
//! Leases (1d), the hook (3) and the tree-sum supervisor (4) consume the same
//! verdict; `docs/LOAD_GOVERNOR.md` has the ownership table. The one action
//! reachable from here on a process is a user-clicked Terminate of a foreign
//! orphan, re-checked at click time ([`terminate_orphan`]).
//!
//! # Where it runs
//!
//! Only in the GitPulse app process ([`start`] is called from `lib.rs::run`
//! and nowhere else). The per-profile single-instance guard already makes the
//! login-item launch and the foreground app one process, so the two-writer
//! case is a dev build or a second profile beside the installed app: those
//! elect a writer with an exclusive lock on `host-load.v1.lock`, and the loser
//! reads the winner's snapshot.
//!
//! # Platforms
//!
//! macOS is governed. Linux is observe-and-advise through PSI, which has not
//! been verified on a real Linux host. Windows reports unavailable. See
//! [`Platform`].

mod governor;
mod probe;
pub(crate) mod procscan;
mod sampler;
mod sensor;
mod snapshot;

pub use governor::{
    admit, admit_anyway, admit_on, decide, grant_override, report as governor_report, shed,
    Admitted, Binding, Governor, GovernorPulse, GovernorReport, GovernorVerdict,
    Input as GovernorInput, Observation, Reason, Refused, Shed, Site, Tier, ENTER_SAMPLES,
    EXIT_SAMPLES, OVERRIDE_WINDOW, REFUSAL_CODE, REFUSAL_NOTICE_AFTER,
};
#[cfg(test)]
pub(crate) use governor::{inject as inject_governor, injected_input as injected_governor_input};
pub use procscan::{
    observe_sessions, terminate_orphan, AgentKind, AgentSession, ForeignProcess, LaunchedTree,
    MemguardHealth, Orphan, ProcView, ScanContext, Scanner as ProcessScanner, SessionCount,
};
pub use sampler::{
    pulse_of, start, start_app, status, HostLoadPulse, HostLoadStatus, PressureInput, Role,
    SamplerOptions, StopHandle,
};
pub use sensor::{
    Compressor, CompressorDelta, CompressorTrend, Cores, HostSample, Level, OsSensor, Power,
    Reading, ReplaySensor, Sensor, Thermal, Why,
};
pub use snapshot::{
    read, report, Snapshot, SnapshotError, TickStats, Writer as SnapshotWriter, WriterIdentity,
    LOCK_FILE_NAME, RECORD_LEN, SNAPSHOT_FILE_NAME, VERSION,
};

use serde::{Deserialize, Serialize};

/// The environment variable a launched task attempt carries: its run id.
///
/// Set on the PTY spawn path from the launch table in
/// `workbench::terminal_command`, after the caller's own environment, so an
/// IPC caller cannot point it at another run. Every descendant inherits it,
/// which is what lets a child that was reparented to launchd still be
/// attributed to the run that started it (card 4 relies on that).
pub const RUN_ID_ENV: &str = "GITPULSE_RUN_ID";

/// How GitPulse treats host load on this platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Platform {
    /// macOS: measured, and the cards that consume it may govern on it.
    Governed,
    /// Linux: measured through PSI and advised on, never governed.
    Advise,
    /// Anything else: no sensor. Readers see [`SensorState::Dead`].
    Unavailable,
}

impl Platform {
    pub const fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::Governed
        } else if cfg!(target_os = "linux") {
            Self::Advise
        } else {
            Self::Unavailable
        }
    }

    fn code(self) -> u32 {
        match self {
            Self::Governed => 1,
            Self::Advise => 2,
            Self::Unavailable => 3,
        }
    }

    fn from_code(code: u32) -> Option<Self> {
        match code {
            1 => Some(Self::Governed),
            2 => Some(Self::Advise),
            3 => Some(Self::Unavailable),
            _ => None,
        }
    }
}

/// The fixed sampling cadence. Hysteresis downstream counts samples, so the
/// interval never adapts; a tick that runs long is published as an overrun.
pub const CADENCE_MS: u64 = 2_000;

/// A snapshot at most this old is fresh.
pub const FRESH_MS: u64 = 10_000;
/// At most this old: stale-recent. Readers may still act on its level.
pub const STALE_RECENT_MS: u64 = 120_000;
/// At most this old: stale-old. Older is dead.
pub const STALE_OLD_MS: u64 = 600_000;

/// How much a reader can trust the snapshot it read, by its age.
///
/// This is the contract cards 1b, 1d and 3 consume: what each does in each
/// state is theirs to decide, and the table in `docs/LOAD_GOVERNOR.md`
/// records the ruling they follow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SensorState {
    /// Written within [`FRESH_MS`].
    Fresh,
    /// Within [`STALE_RECENT_MS`].
    StaleRecent,
    /// Within [`STALE_OLD_MS`].
    StaleOld,
    /// Older, never written, unreadable, from a previous boot, or an
    /// unsupported platform.
    Dead,
}

impl SensorState {
    pub fn from_age_ms(age_ms: u64) -> Self {
        if age_ms <= FRESH_MS {
            Self::Fresh
        } else if age_ms <= STALE_RECENT_MS {
            Self::StaleRecent
        } else if age_ms <= STALE_OLD_MS {
            Self::StaleOld
        } else {
            Self::Dead
        }
    }

    /// Whether a consumer may act on the last-known level: fresh and
    /// stale-recent govern (with a "stale Ns" warning for the latter);
    /// stale-old and dead do not.
    pub fn governs(self) -> bool {
        matches!(self, Self::Fresh | Self::StaleRecent)
    }
}

/// What a reader learns from the snapshot: the state, the last-known level
/// with its age, and why when there is nothing to go on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SensorReport {
    pub state: SensorState,
    /// Milliseconds since the snapshot was written. `None` when there is no
    /// readable snapshot or its age cannot be established.
    pub age_ms: Option<u64>,
    /// The last-known memory-pressure level, whatever the state. A consumer
    /// decides from [`SensorReport::state`] whether to act on it.
    pub level: Option<Level>,
    /// The kill switch as the writer last published it.
    pub enabled: Option<bool>,
    /// Why the state is what it is when that is not just the age.
    pub reason: Option<String>,
}

/// One point in time on the clocks a snapshot is dated with.
///
/// The age of a snapshot is computed on the system-wide monotonic clock when
/// both ends are in the same boot, so a wall clock stepped by NTP, a user or a
/// VM resume cannot make a dead sampler look fresh or a live one look dead.
/// The wall clock is kept for display and as the fallback across a boot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Clock {
    pub wall_ms: u64,
    /// Nanoseconds on a clock shared by every process and counting through
    /// sleep: `CLOCK_MONOTONIC` on macOS, `CLOCK_BOOTTIME` on Linux.
    pub mono_ns: u64,
    /// A hash of this boot's identity (`kern.bootsessionuuid`, Linux's
    /// `boot_id`); 0 when unknown. Not the boot *time*: macOS moves
    /// `kern.boottime` whenever the wall clock is stepped.
    pub boot_id: u64,
}

impl Clock {
    pub fn now() -> Self {
        let wall_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
            .unwrap_or(0);
        Self {
            wall_ms,
            mono_ns: probe::shared_monotonic_ns(),
            boot_id: probe::boot_id(),
        }
    }
}

/// Snapshot age at `now`, and the reason when it cannot be trusted.
fn age_ms(written: Clock, now: Clock) -> Result<u64, String> {
    let same_boot = written.boot_id != 0 && written.boot_id == now.boot_id;
    if same_boot && written.mono_ns != 0 && now.mono_ns != 0 {
        return now
            .mono_ns
            .checked_sub(written.mono_ns)
            .map(|ns| ns / 1_000_000)
            .ok_or_else(|| "the snapshot is dated after now on the monotonic clock".to_string());
    }
    if written.boot_id != 0 && now.boot_id != 0 && !same_boot {
        return Err("the snapshot was written before this boot".to_string());
    }
    now.wall_ms
        .checked_sub(written.wall_ms)
        .ok_or_else(|| "the snapshot is dated in the future on the wall clock".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_four_states_split_exactly_at_their_bounds() {
        for (age, want) in [
            (0, SensorState::Fresh),
            (FRESH_MS, SensorState::Fresh),
            (FRESH_MS + 1, SensorState::StaleRecent),
            (STALE_RECENT_MS, SensorState::StaleRecent),
            (STALE_RECENT_MS + 1, SensorState::StaleOld),
            (STALE_OLD_MS, SensorState::StaleOld),
            (STALE_OLD_MS + 1, SensorState::Dead),
            (u64::MAX, SensorState::Dead),
        ] {
            assert_eq!(SensorState::from_age_ms(age), want, "age {age}");
        }
        assert!(SensorState::Fresh.governs() && SensorState::StaleRecent.governs());
        assert!(!SensorState::StaleOld.governs() && !SensorState::Dead.governs());
    }

    #[test]
    fn age_rides_the_monotonic_clock_through_a_wall_clock_jump() {
        let written = Clock {
            wall_ms: 10_000_000,
            mono_ns: 5_000_000_000,
            boot_id: 100,
        };
        // The wall clock stepped back an hour and then forward a day; the
        // monotonic clock says 3 s passed, and that is the answer both times.
        for wall_ms in [10_000_000 - 3_600_000, 10_000_000 + 86_400_000] {
            let now = Clock {
                wall_ms,
                mono_ns: 8_000_000_000,
                boot_id: 100,
            };
            assert_eq!(age_ms(written, now), Ok(3_000));
        }
    }

    #[test]
    fn a_snapshot_from_a_previous_boot_or_the_future_has_no_age() {
        let written = Clock {
            wall_ms: 10_000,
            mono_ns: 9_000_000_000,
            boot_id: 100,
        };
        let rebooted = Clock {
            wall_ms: 20_000,
            mono_ns: 1_000_000_000,
            boot_id: 500,
        };
        assert!(age_ms(written, rebooted)
            .unwrap_err()
            .contains("before this boot"));
        let earlier = Clock {
            wall_ms: 20_000,
            mono_ns: 1_000_000_000,
            boot_id: 100,
        };
        assert!(age_ms(written, earlier).unwrap_err().contains("after now"));
        // No boot identity at either end: the wall clock is all there is.
        let unknown = Clock {
            boot_id: 0,
            ..written
        };
        let later = Clock {
            wall_ms: 12_500,
            mono_ns: 0,
            boot_id: 0,
        };
        assert_eq!(age_ms(unknown, later), Ok(2_500));
        let before = Clock {
            wall_ms: 9_000,
            ..later
        };
        assert!(age_ms(unknown, before).unwrap_err().contains("future"));
    }

    #[test]
    fn the_platform_tier_round_trips_and_names_this_build() {
        for p in [Platform::Governed, Platform::Advise, Platform::Unavailable] {
            assert_eq!(Platform::from_code(p.code()), Some(p));
        }
        assert_eq!(Platform::from_code(0), None);
        #[cfg(target_os = "macos")]
        assert_eq!(Platform::current(), Platform::Governed);
        #[cfg(target_os = "linux")]
        assert_eq!(Platform::current(), Platform::Advise);
        #[cfg(windows)]
        assert_eq!(Platform::current(), Platform::Unavailable);
    }
}
