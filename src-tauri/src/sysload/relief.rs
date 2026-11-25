//! Pressure relief (load-governor card 4): what GitPulse does about agent
//! process trees while the host is short of memory.
//!
//! On 2026-11-24 the Mac hung for two hours on memory-compressor exhaustion and
//! about eighteen agent sessions were lost. The governor (`governor::decide`)
//! sheds GitPulse's own work and refuses new launches; nothing acted on the
//! trees already running. This module does, on the elected sensor writer's
//! sampler tick (2 s), with **no spawn gate, no Manvi probe and no subprocess**:
//! the in-process scan of the last sample, `setpriority(2)` and `kill(2)`.
//!
//! # The two rungs
//!
//! Only the memory inputs count ([`super::governor::memory_sample`]: memory
//! pressure, compressor thrash, compressor segments, swap). A Severe that CPU
//! load, thermals, the battery or a filling disk holds never acts.
//!
//! * **Rung A, slow.** Memory-bound Elevated or worse for
//!   [`super::ENTER_SAMPLES`] samples *with a cause* puts every eligible process
//!   of every agent session (launched or observed) in `PRIO_DARWIN_BG`; the
//!   priority is put back once the host has read below Elevated, or the cause
//!   has gone, for [`super::EXIT_SAMPLES`] samples. A cause is the kernel at Warn
//!   or worse, compressor thrash, compressor segments at their Elevated level,
//!   or swap or segments *rising* across the last [`SLOW_TREND_SAMPLES`]. Swap
//!   *level* is not one: it stays high long after a recovery, and a host that
//!   was in trouble is not one that is.
//! * **Rung B, freeze.** Memory-bound Severe for [`SEVERE_SAMPLES`] samples (60
//!   s) *and* worsening `SIGSTOP`s the single heaviest eligible subtree of a
//!   GitPulse-launched session in a trusted repository, below the shells an
//!   agent started to run a command, never the shell or the agent. Worsening
//!   is swap or compressor segments still rising across that window by
//!   [`SWAP_RISE_BYTES`] or [`SEGMENT_RISE_POINTS`], or *pinned at the edge*
//!   for all of it: the kernel at Critical, compressor thrash, or segments at
//!   [`PINNED_SEGMENT_PCT`] of their limit, where flat is still failing. Swap
//!   level never counts. At most one subtree per session and
//!   [`MAX_FROZEN_PER_HOST`] per host.
//!
//! Nothing is frozen indefinitely. The subtrees resume (`SIGCONT`) when
//! worsening has been flat for [`FLAT_SAMPLES`] samples (not while pinned),
//! when the tier has read below Severe for [`super::EXIT_SAMPLES`] samples, on
//! the first tick the build-lease board stops refusing (for a frozen build that
//! held a slot), or at [`FREEZE_CAP`], whichever comes first; at the cap the
//! freeze is not escalated (there is no terminate rung in this slice) and the
//! cap hit is counted. Any future kill path must `SIGCONT` first: a signal to a
//! stopped process waits for the continue (`terminal/jobs.rs` does the same).
//!
//! `activity.rs` rejects `SIGSTOP` for idle agent *roots*. That stands: the
//! root and the shells beside it are never signalled. A subtree below a shell
//! is a build, a test run or a script, which the agent is waiting on, not the
//! agent.
//!
//! # Exemptions
//!
//! One list ([`super::procscan`]'s shared `throttle_exempt`: the root,
//! `memguard`, GitPulse's, Manvi's, ptyd's and devmap's binaries, an IDE's
//! non-agent children, Manvi worker trees) spares both rungs. Rung B adds, and
//! one exempt member spares the whole subtree: `git` processes and any process
//! working inside a worktree with a live `index.lock`; build-lease *waiters*
//! (the lease board, `lease::Dir::board`, because `procscan` cannot see a
//! descriptor lock) and trees whose environment carries `MAC_HEAVY_LABEL` (read
//! from `KERN_PROCARGS2`, which macOS hides for Apple platform binaries: the
//! marker is found on the program that carries it, `cargo`, `go`, `rustc`, not
//! on `/bin/bash`); MCP servers (anything named `*mcp*`, and everything under a
//! direct child of the root that is not positively an agent's tool shell, see
//! below); and the shells that are direct children of the root.
//!
//! A build-lease *holder* (a slot's lock holder, or a tree carrying
//! `GITPULSE_LEASE_HELD` or `CARGO_SLOT_HELD`, any value) is eligible only while
//! the lease board is refusing new builds (`lease::capacity(..).refuses`: the
//! verdict is Severe, or not admitting launches). A frozen build is one nobody
//! can replace then, and the first tick the board stops refusing every frozen
//! holder is continued, whatever memory is doing. The board knows a holder is
//! alive because the slot is an `fcntl` record lock the OS drops when its
//! process dies and `F_GETLK` names the process that holds it: a stopped holder
//! still holds it.
//!
//! A check that could not run (an unreadable board, environment, argument list
//! or working directory) spares the subtree: unknown is never "clear". The same
//! predicate judges a process that joins a frozen subtree after it was stopped.
//!
//! The shells an agent runs commands through are told from the shells that
//! launch its MCP servers by their arguments: Claude Code's Bash tool starts
//! `/bin/zsh -c source <config>/shell-snapshots/snapshot-zsh-….sh …`
//! ([`TOOL_SHELL_MARK`]), and macOS shows the arguments of `/bin/zsh` though not
//! its environment. No equivalent mark has been verified for Codex's or
//! Antigravity's shells, so a session of theirs has no identified shell and is
//! never frozen (it is still slowed).
//!
//! # One owner of slowing
//!
//! Every priority change GitPulse makes to an agent's process goes through the
//! ledger here. The steward's slowdown ([`steward_throttle`]) delegates to it:
//! it may slow what the relief has not, and it cannot put back what the relief
//! slowed, which is the fight the steward's "Manvi is not configured, restore"
//! path would otherwise start.
//!
//! # Crash safety
//!
//! Before a `SIGSTOP` (or a priority change) the process is written to a
//! private journal (`<config>/relief/journal.v1.json`, pid, start time,
//! session, time, rung); a failed journal write means no signal. The journal is
//! replayed (`SIGCONT`, priority restored) when a sampler wins the writer
//! election, when the relief is dropped (sampler stop or panic), and from the
//! app's teardown ([`shutdown`]); an entry is cleared only after its resume was
//! attempted, and a failure is recorded in [`ReliefSnapshot`], never dropped. A
//! recycled pid is refused by `procguard::signal::signal_process` through its
//! start time.
//!
//! The journal is the first line; a [`Watch`] is the second. It is an in-memory
//! copy of the frozen processes behind a lock of its own (no file, no action
//! lock), and an independent thread continues what is past [`FREEZE_CAP`] plus
//! [`WATCHDOG_GRACE`], or everything when the tick has not advanced for
//! [`WATCHDOG_STALL`]: a sampler that hangs on a disk write, or that lost the
//! writer election with a freeze standing, cannot leave a build stopped.
//! [`shutdown`] waits for a tick that is mid-action only a couple of seconds
//! before it continues from that copy. A freeze a live writer left past its cap
//! and grace is continued by the next sampler to win the election.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::governor::{
    memory_sample, Input as GovernorInput, MemoryReading, ENTER_SAMPLES, EXIT_SAMPLES,
};
use super::procscan::{ReliefProc, ReliefSession, ReliefTable};
use super::{Binding, Tier};
use crate::tool_config::SupervisorMode;

/// Consecutive memory-bound Severe samples (2 s each) before a freeze: 60 s.
pub const SEVERE_SAMPLES: u32 = 30;
/// Consecutive samples below Severe that end a Severe run, so one reading that
/// dips does not restart the 60 s, and a real recovery does.
pub const SEVERE_BREAK_SAMPLES: u32 = EXIT_SAMPLES;
/// Rise in swap in use across the window that counts as deterioration: the
/// median of the last [`TREND_ENDS`] readings over the median of the first.
pub const SWAP_RISE_BYTES: u64 = 256 << 20;
/// Rise in compressor segments in use, in percentage points of the segment
/// limit, that counts as deterioration. The 2026-10-07 panic was this reaching
/// 100 %.
pub const SEGMENT_RISE_POINTS: f64 = 1.0;
/// Readings whose median stands for each end of a window, so one noisy sample
/// (or two) is not a trend. A window with fewer readable readings than twice this is "not
/// shown to be rising", which never starts a freeze and never holds one.
pub const TREND_ENDS: usize = 5;
/// Samples with no deterioration after which a freeze ends.
pub const FLAT_SAMPLES: usize = 15;
/// The longest a freeze lasts, measured on the monotonic clock (a tick that
/// ran late under thrash must not stretch it).
pub const FREEZE_CAP: Duration = Duration::from_secs(10 * 60);
/// Samples to wait after a freeze that selected nothing before looking again.
pub const RETRY_SAMPLES: u32 = FLAT_SAMPLES as u32;
/// Most subtrees frozen in one session.
pub const MAX_FROZEN_PER_SESSION: usize = 1;
/// Most subtrees frozen on the host.
pub const MAX_FROZEN_PER_HOST: usize = 3;
/// A subtree lighter than this cannot relieve the host: not worth stopping.
pub const MIN_FREEZE_BYTES: u64 = 256 << 20;
/// Most processes in one frozen subtree: a bound on what one decision stops.
pub const MAX_SUBTREE_MEMBERS: usize = 256;
/// Most processes put in the background at once.
pub const MAX_SLOWED: usize = 2048;
/// Most candidate subtrees whose exemptions are checked per session per
/// decision: each check reads environments and working directories.
const MAX_CANDIDATES_CHECKED: usize = 16;
/// Most subtree members whose environment is read per candidate.
const MAX_ENV_CHECKS: usize = 24;
/// Most processes whose working directory is read per candidate.
const MAX_CWD_CHECKS: usize = 32;
/// Failures kept for the status.
const MAX_FAILURES: usize = 16;
/// Samples between passes of rung A after the first (10 s): the tick writes a
/// journal on a pass, and a host short of memory is one whose disk may stall.
pub const SLOW_PASS_EVERY: u32 = 5;
/// The deepest process tree walked.
const MAX_TREE_DEPTH: usize = 64;
const JOURNAL_MAX_BYTES: u64 = 4 << 20;
const JOURNAL_DIR: &str = "relief";
const JOURNAL_FILE: &str = "journal.v1.json";

/// Environment variables that mark a build holding a lease: the lease
/// wrapper's nested-call marker and the slot shim's. Any value counts. Such a
/// build is a holder of the lease board, and is eligible for rung B only while
/// the board is refusing new builds.
const LEASE_MARKERS: [&str; 2] = ["GITPULSE_LEASE_HELD", "CARGO_SLOT_HELD"];
/// `mac_heavy.sh`'s marker: a heavy job a person started, never frozen.
const HEAVY_MARKER: &str = "MAC_HEAVY_LABEL";
/// What Claude Code's Bash tool shells carry in their arguments
/// (`/bin/zsh -c source <config>/shell-snapshots/snapshot-zsh-….sh …`). The one
/// positive identification of "a shell an agent runs a command through":
/// every other direct child of an agent is read as something else (an MCP
/// server's launcher, a helper) and is never entered.
const TOOL_SHELL_MARK: &str = "shell-snapshots/snapshot-";
/// How long past [`FREEZE_CAP`] a freeze may stand before the watchdog thread
/// continues it without the sampler.
pub const WATCHDOG_GRACE: Duration = Duration::from_secs(60);
/// A sampler tick that has not advanced for this long is a hung sampler (or a
/// lost writer election): the watchdog continues every frozen process.
pub const WATCHDOG_STALL: Duration = Duration::from_secs(60);
/// How often the watchdog looks.
const WATCHDOG_EVERY: Duration = Duration::from_secs(5);
/// How long [`shutdown`] waits for a tick that is mid-action before it
/// continues the frozen processes from memory.
const SHUTDOWN_WAIT: Duration = Duration::from_secs(2);
/// Samples whose trend decides whether pressure is *building*, which is what
/// lets rung A act on swap alone.
pub const SLOW_TREND_SAMPLES: usize = 15;
/// Compressor segments at or above this percent of the limit are the edge of
/// the 2026-10-07 panic: pinned there counts as worsening even when flat.
pub const PINNED_SEGMENT_PCT: f64 = 95.0;
/// Most processes one pass of rung A newly slows: each costs a few syscalls,
/// and the pass runs on the sampler's tick. The rest follow on the next pass.
const MAX_SLOW_PER_PASS: usize = 256;
/// Most per-process records (environment markers, argv) kept across ticks.
const MAX_MARKS: usize = 4096;
/// The most processes a replay's one look at the process table reads.
const MAX_TABLE_PROCESSES: usize = 16_384;
/// The most processes one replay restores by walking descendants.
const MAX_SWEPT: usize = 256;
/// The longest a person's resume waits for the action lock before it answers
/// "busy" (the request is kept and tried again on the next tick).
const ACT_WAIT: Duration = Duration::from_secs(2);
/// Consecutive samples below the edge before a pinned run is forgotten.
pub const PINNED_BREAK_SAMPLES: u32 = 3;

/// Names of the shells an agent runs commands through.
const SHELLS: [&str; 8] = ["sh", "bash", "zsh", "fish", "dash", "ksh", "tcsh", "csh"];

// ---------------------------------------------------------------------------
// The trigger: a pure decision core, one sample at a time.
// ---------------------------------------------------------------------------

/// What one tick read, reduced to what the trigger decides on.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub(crate) struct Sample {
    /// The memory-only tier and the input that put it there; `None` when the
    /// host cannot be governed on (a stale or unreadable sensor).
    pub tier: Option<(Tier, Option<Binding>)>,
    pub swap_used: Option<u64>,
    /// Compressor segments in use, in percent of the limit.
    pub segment_pct: Option<f64>,
    /// A reason to slow that is not swap *level*: the kernel reports Warn or
    /// worse, the compressor is thrashing, or segments are at their Elevated
    /// threshold. Swap stays high long after a recovery, so a high swap alone
    /// is not a cause; swap (or segments) *rising* is, and the trigger adds
    /// that from its own window.
    pub cause: bool,
    /// Pinned at the edge: kernel level Critical, compressor thrash, or
    /// segments at [`PINNED_SEGMENT_PCT`]. Flat is still failing here.
    pub pinned: bool,
}

impl Sample {
    pub(crate) fn of(input: &GovernorInput) -> Self {
        let segment_pct = input
            .readings
            .segments
            .value()
            .filter(|s| s.limit > 0)
            .map(|s| s.total as f64 * 100.0 / s.limit as f64);
        let reading = memory_sample(input);
        let at = |f: fn(&MemoryReading) -> bool| reading.as_ref().is_some_and(f);
        Self {
            tier: reading.map(|r| (r.tier, r.binding)),
            swap_used: input.readings.swap_used.value(),
            segment_pct,
            cause: at(|r| r.kernel_warn || r.thrash || r.segments_elevated),
            pinned: at(|r| r.kernel_critical || r.thrash)
                || (reading.is_some() && segment_pct.is_some_and(|p| p >= PINNED_SEGMENT_PCT)),
        }
    }
}

/// Why a freeze ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResumeReason {
    /// Deterioration was flat for [`FLAT_SAMPLES`] samples.
    Flat,
    /// [`FREEZE_CAP`] elapsed.
    Cap,
    /// The memory tier read below Severe for [`SEVERE_BREAK_SAMPLES`] samples.
    Calmed,
    /// The setting left `act`, or host-load measurement was switched off.
    Off,
    /// The subtree's own process ended.
    Gone,
    /// GitPulse is stopping.
    Shutdown,
    /// The build-lease board stopped refusing new builds: a frozen build that
    /// held a slot goes on at once.
    LeaseOpened,
    /// The watchdog continued it because the sampler stopped ticking (or its
    /// freeze outlived the cap and the grace).
    Watchdog,
    /// Something else continued the subtree's first process.
    Continued,
}

impl ResumeReason {
    fn words(self) -> &'static str {
        match self {
            Self::Flat => "swap and compressor segments stopped rising",
            Self::Cap => "the 10-minute limit was reached",
            Self::Calmed => "memory pressure eased",
            Self::Off => "the supervisor was switched off",
            Self::Gone => "the subtree's process ended",
            Self::Shutdown => "GitPulse stopped",
            Self::LeaseOpened => "the build lease board is taking new builds again",
            Self::Watchdog => "the watchdog continued it when the sampler stopped ticking",
            Self::Continued => "something else continued it",
        }
    }
}

/// What the trigger asks of the freeze rung this sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Step {
    /// Nothing changes.
    Hold,
    /// Choose a subtree and freeze it; answer with [`Trigger::began`].
    Begin,
    /// Resume what is frozen.
    Resume(ResumeReason),
}

/// One sample's decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Verdict {
    /// Rung A is in force.
    pub slow: bool,
    pub freeze: Step,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Phase {
    #[default]
    Idle,
    Frozen {
        since: Instant,
        /// Samples seen since the freeze began.
        samples: usize,
    },
}

/// The trigger. Pure: fed [`Sample`]s and a monotonic clock, it holds no OS
/// state, so every threshold below is tested without a process.
#[derive(Debug, Default)]
pub(crate) struct Trigger {
    slowing: bool,
    slow_up: u32,
    slow_down: u32,
    severe_run: u32,
    calm_run: u32,
    /// Consecutive Severe samples that were also [`Sample::pinned`]. It
    /// survives a sample or two below the edge ([`PINNED_BREAK_SAMPLES`]).
    pinned_run: u32,
    /// Consecutive samples that were not Severe and pinned.
    unpinned_run: u32,
    window: VecDeque<(Option<u64>, Option<f64>)>,
    phase: Phase,
    retry_wait: u32,
    /// A person ended this episode's freeze (`resume_all`): no new freeze
    /// until the tier has read below Severe for [`SEVERE_BREAK_SAMPLES`].
    held_off: bool,
    /// A freeze that began on *pinned alone* (nothing rising) has been spent
    /// in this Severe episode: a host pinned by something unrelated would
    /// otherwise have the same builds frozen again after every cap. Rung B
    /// then needs a fresh rise (the deterioration rule) or a new episode.
    /// Cleared only where the episode ends, never by [`Trigger::ended`].
    pinned_spent: bool,
    /// The freeze [`Step::Begin`] asked for rested on pinned alone.
    begin_pinned_only: bool,
    /// A freeze has begun in this Severe episode.
    froze_this_episode: bool,
    /// Counts the Severe episodes that have ended: the relief forgets what it
    /// froze in the last one when this moves.
    episode: u64,
}

impl Trigger {
    /// No freeze until the memory tier has dropped below Severe.
    pub(crate) fn hold_off(&mut self) {
        self.held_off = true;
    }

    pub(crate) fn reset(&mut self) {
        let episode = self.episode.wrapping_add(1);
        *self = Self::default();
        self.episode = episode;
    }

    /// Which Severe episode this is; it moves when one ends (the tier read
    /// below Severe for [`SEVERE_BREAK_SAMPLES`] after a freeze or a person's
    /// resume) and on [`Trigger::reset`].
    pub(crate) fn episode(&self) -> u64 {
        self.episode
    }

    pub(crate) fn observe(&mut self, now: Instant, sample: &Sample) -> Verdict {
        let tier = sample.tier.map(|(tier, _)| tier);
        if tier == Some(Tier::Severe) {
            self.severe_run = self.severe_run.saturating_add(1);
            self.calm_run = 0;
        } else {
            self.calm_run = self.calm_run.saturating_add(1);
            if self.calm_run >= SEVERE_BREAK_SAMPLES {
                if self.froze_this_episode || self.held_off {
                    self.episode = self.episode.wrapping_add(1);
                }
                self.severe_run = 0;
                self.window.clear();
                self.held_off = false;
                self.pinned_spent = false;
                self.froze_this_episode = false;
            }
        }
        if tier == Some(Tier::Severe) && sample.pinned {
            self.pinned_run = self.pinned_run.saturating_add(1);
            self.unpinned_run = 0;
        } else {
            // Kernel Critical flickers: a sample or two below the edge is not
            // a host that recovered.
            self.unpinned_run = self.unpinned_run.saturating_add(1);
            if self.unpinned_run >= PINNED_BREAK_SAMPLES {
                self.pinned_run = 0;
            }
        }
        self.window
            .push_back((sample.swap_used, sample.segment_pct));
        while self.window.len() > SEVERE_SAMPLES as usize {
            self.window.pop_front();
        }

        // Rung A needs a cause, not just a tier: swap *level* stays high after
        // a recovery, so a high flat swap under a Normal kernel is not a reason
        // to slow anyone. The kernel at Warn or worse, thrash, segments at
        // their Elevated level, or swap/segments rising across the last
        // [`SLOW_TREND_SAMPLES`] are.
        let recent = self.window.len().saturating_sub(SLOW_TREND_SAMPLES);
        let cause = sample.cause || deteriorating(self.window.iter().skip(recent));
        if tier.is_some_and(|t| t >= Tier::Elevated) && cause {
            self.slow_down = 0;
            self.slow_up = self.slow_up.saturating_add(1);
            if self.slow_up >= ENTER_SAMPLES {
                self.slowing = true;
            }
        } else {
            // An ungovernable sample (stale, dead or unreadable sensor), and
            // one with no cause, cannot justify slowing: it counts toward
            // letting go.
            self.slow_up = 0;
            self.slow_down = self.slow_down.saturating_add(1);
            if self.slow_down >= EXIT_SAMPLES {
                self.slowing = false;
            }
        }

        let freeze = match self.phase {
            Phase::Idle => {
                self.retry_wait = self.retry_wait.saturating_sub(1);
                let rose = deteriorating(self.window.iter());
                let pinned = self.pinned_run >= SEVERE_SAMPLES && !self.pinned_spent;
                if self.retry_wait == 0
                    && !self.held_off
                    && self.severe_run >= SEVERE_SAMPLES
                    && (rose || pinned)
                {
                    self.begin_pinned_only = !rose;
                    Step::Begin
                } else {
                    Step::Hold
                }
            }
            Phase::Frozen { since, samples } => {
                let samples = samples + 1;
                self.phase = Phase::Frozen { since, samples };
                let recent = self.window.len().saturating_sub(FLAT_SAMPLES);
                if now.saturating_duration_since(since) >= FREEZE_CAP {
                    Step::Resume(ResumeReason::Cap)
                } else if self.calm_run >= SEVERE_BREAK_SAMPLES {
                    Step::Resume(ResumeReason::Calmed)
                } else if samples >= FLAT_SAMPLES
                    && !deteriorating(self.window.iter().skip(recent))
                    // Pinned at the edge (kernel Critical, thrash, segments at
                    // their limit) is failing even when nothing rises.
                    && self.pinned_run < FLAT_SAMPLES as u32
                {
                    Step::Resume(ResumeReason::Flat)
                } else {
                    Step::Hold
                }
            }
        };
        if matches!(freeze, Step::Resume(_)) {
            self.ended();
        }
        Verdict {
            slow: self.slowing,
            freeze,
        }
    }

    /// The answer to [`Step::Begin`]: whether anything was frozen.
    pub(crate) fn began(&mut self, now: Instant, frozen: bool) {
        if frozen {
            self.phase = Phase::Frozen {
                since: now,
                samples: 0,
            };
            self.froze_this_episode = true;
            if self.begin_pinned_only {
                self.pinned_spent = true;
            }
        } else {
            self.retry_wait = RETRY_SAMPLES;
        }
    }

    /// A freeze ended outside the trigger's own decision (the subtree's
    /// process ended, the setting changed): the next one waits for a whole new
    /// Severe run.
    pub(crate) fn ended(&mut self) {
        self.phase = Phase::Idle;
        self.severe_run = 0;
        self.calm_run = 0;
        self.pinned_run = 0;
        self.unpinned_run = 0;
        self.window.clear();
        self.retry_wait = 0;
    }

    /// Rung A is in force.
    pub(crate) fn slowing(&self) -> bool {
        self.slowing
    }

    pub(crate) fn frozen(&self) -> bool {
        matches!(self.phase, Phase::Frozen { .. })
    }
}

/// Whether swap or compressor segments rose across `readings` by their
/// minimum: the median of the last [`TREND_ENDS`] readable values over the
/// median of the first. A series with too few readable values is not shown to rise.
fn deteriorating<'a>(readings: impl Iterator<Item = &'a (Option<u64>, Option<f64>)>) -> bool {
    let (mut swap, mut segments) = (Vec::new(), Vec::new());
    for (swap_used, segment_pct) in readings {
        if let Some(bytes) = swap_used {
            swap.push(*bytes as f64);
        }
        if let Some(pct) = segment_pct {
            segments.push(*pct);
        }
    }
    rises(&swap, SWAP_RISE_BYTES as f64) || rises(&segments, SEGMENT_RISE_POINTS)
}

fn rises(values: &[f64], minimum: f64) -> bool {
    if values.len() < 2 * TREND_ENDS {
        return false;
    }
    let median = |slice: &[f64]| {
        let mut sorted = slice.to_vec();
        sorted.sort_by(f64::total_cmp);
        sorted[sorted.len() / 2]
    };
    median(&values[values.len() - TREND_ENDS..]) - median(&values[..TREND_ENDS]) >= minimum
}

// ---------------------------------------------------------------------------
// The OS seams.
// ---------------------------------------------------------------------------

/// The signals the relief sends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Signal {
    Stop,
    Cont,
}

/// What became of a signal that was not refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Delivery {
    Delivered,
    /// The process had already ended: nothing to stop or resume.
    Gone,
}

/// The only things the relief does to a process.
pub(crate) trait Actuator: Send {
    /// `Ok(true)` when the priority changed, `Ok(false)` when it already was
    /// what was asked; `Err` when refused (a recycled pid among them).
    fn set_background(&self, pid: i32, start_us: u64, enable: bool) -> Result<bool, String>;
    /// Signals `pid` only while its start time is still `start_us`.
    fn signal(&self, pid: i32, start_us: u64, signal: Signal) -> Result<Delivery, String>;
    /// Whether the process reads back as in the background; `None` when that
    /// cannot be read (it ended, it is not ours, the platform cannot say).
    fn is_background(&self, pid: i32, start_us: u64) -> Option<bool>;
    /// Whether the process is stopped now, read from the OS immediately before
    /// the relief signals it: a process somebody else stopped (a Ctrl-Z job, a
    /// debugger) is not the relief's to stop, and not its to continue. `Err`
    /// names a process that ended ("no longer running"), is another one now
    /// ("recycled"), or whose state could not be read: that is never taken for
    /// a running one.
    fn is_stopped(&self, pid: i32, start_us: u64) -> Result<bool, String>;
    /// Every process as `(pid, ppid, start_us)`, bounded; empty when the host
    /// cannot list them. Read once by a replay that looks for what inherited a
    /// background priority.
    fn process_table(&self) -> Vec<(i32, i32, u64)>;
}

/// Who is on the build-lease board, by pid.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct LeaseBoard {
    /// Processes holding a slot. A slot is a descriptor lock the OS drops when
    /// its process dies, and `F_GETLK` names the process: a holder on this list
    /// is alive, and a stopped one still holds it.
    pub holders: BTreeSet<i32>,
    /// Processes queued for one. Never frozen: a waiter is about to run.
    pub waiters: BTreeSet<i32>,
}

/// What selection reads about a process beyond the scan.
pub(crate) trait Probe: Send {
    fn cwd(&self, pid: i32) -> Option<PathBuf>;
    /// `NAME=value` strings; `None` when unreadable.
    fn environment(&self, pid: i32) -> Option<Vec<String>>;
    /// The arguments the process was started with; `None` when unreadable.
    fn argv(&self, pid: i32) -> Option<Vec<String>>;
    /// The holders of and waiters for a build lease. `Err` when the board
    /// could not be read in full: then nothing is frozen.
    fn lease_board(&self) -> Result<LeaseBoard, String>;
}

/// The real actuator: `setpriority(2)` through `procscan` and `kill(2)`
/// through `procguard`.
pub(crate) struct OsActuator;

impl Actuator for OsActuator {
    fn set_background(&self, pid: i32, start_us: u64, enable: bool) -> Result<bool, String> {
        super::procscan::set_background_throttle(pid, start_us, enable)
    }

    #[cfg(unix)]
    fn signal(&self, pid: i32, start_us: u64, signal: Signal) -> Result<Delivery, String> {
        let signo = match signal {
            Signal::Stop => libc::SIGSTOP,
            Signal::Cont => libc::SIGCONT,
        };
        let start_of = |p: i32| u32::try_from(p).ok().and_then(super::process_start_us);
        match crate::procguard::signal::signal_process(pid, start_us, &start_of, signo, "relief") {
            Ok(()) => Ok(Delivery::Delivered),
            Err(error)
                if error.raw_os_error() == Some(libc::ESRCH)
                    || (crate::procguard::signal::is_refusal(&error)
                        && error.to_string().contains("no longer running")) =>
            {
                Ok(Delivery::Gone)
            }
            Err(error) => Err(error.to_string()),
        }
    }

    #[cfg(not(unix))]
    fn signal(&self, _pid: i32, _start_us: u64, _signal: Signal) -> Result<Delivery, String> {
        Err("signals are not supported on this platform".into())
    }

    fn is_background(&self, pid: i32, start_us: u64) -> Option<bool> {
        super::procscan::is_background(pid, start_us)
    }

    fn is_stopped(&self, pid: i32, start_us: u64) -> Result<bool, String> {
        super::procscan::is_stopped(pid, start_us)
    }

    fn process_table(&self) -> Vec<(i32, i32, u64)> {
        super::procscan::process_table(MAX_TABLE_PROCESSES)
    }
}

/// The real probe: the OS for working directories and environments, the lease
/// directory under `config` for builds.
pub(crate) struct OsProbe {
    pub config: PathBuf,
}

impl Probe for OsProbe {
    fn cwd(&self, pid: i32) -> Option<PathBuf> {
        super::procscan::cwd_of(pid)
    }

    fn environment(&self, pid: i32) -> Option<Vec<String>> {
        super::procscan::environment_of(pid)
    }

    fn argv(&self, pid: i32) -> Option<Vec<String>> {
        super::procscan::argv_of_pid(pid)
    }

    #[cfg(unix)]
    fn lease_board(&self) -> Result<LeaseBoard, String> {
        let root = super::lease::lease_dir(&self.config);
        if !root.exists() {
            // No build ever took a lease on this host.
            return Ok(LeaseBoard::default());
        }
        let board = super::lease::Dir::open(&root)?.board(&super::lease::Own::default());
        if !board.unreadable.is_empty() || !board.unreadable_slots.is_empty() {
            return Err(format!(
                "{} lease entries could not be read, so a build may be running",
                board.unreadable.len() + board.unreadable_slots.len()
            ));
        }
        Ok(LeaseBoard {
            holders: board.holders.iter().map(|h| h.pid).collect(),
            waiters: board.waiters.iter().map(|w| w.record.pid).collect(),
        })
    }

    #[cfg(not(unix))]
    fn lease_board(&self) -> Result<LeaseBoard, String> {
        Err("build leases are not supported on this platform".into())
    }
}

/// Whether the repository `cwd` is in is trusted: the nearest checkout above
/// it holds a current grant. Reads the grant store and the directory identity;
/// it starts no `git`.
fn repository_trusted(cwd: &Path) -> bool {
    crate::engine::git_cli::find_git_root(cwd)
        .is_some_and(|root| crate::repository_trust::require(&root).is_ok())
}

// ---------------------------------------------------------------------------
// Selection: which subtree rung B stops.
// ---------------------------------------------------------------------------

/// One subtree chosen for a freeze.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Selection {
    pub session_pid: i32,
    pub session_start_us: u64,
    pub terminal_session_id: Option<String>,
    pub root: ReliefProc,
    /// Parent before child.
    pub members: Vec<ReliefProc>,
    pub footprint: u64,
    /// The checkout the session works in.
    pub repo_path: Option<String>,
    /// A build that holds a lease slot is in it: eligible only while the lease
    /// board refuses, and continued on the first tick it does not.
    pub holder: bool,
}

fn is_shell(name: &str) -> bool {
    let lower = name.trim_start_matches('-').to_ascii_lowercase();
    SHELLS.contains(&lower.as_str())
}

fn is_git(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower == "git" || lower.starts_with("git-")
}

fn is_mcp(name: &str) -> bool {
    name.to_ascii_lowercase().contains("mcp")
}

/// Whether a git index lock guards the worktree `cwd` is in. The nearest
/// `.git` decides: a directory has its lock beside it, a file (a linked
/// worktree) names the gitdir whose `index.lock` guards it. A `.git` that
/// cannot be read is a lock: unknown is not clear.
pub(crate) fn index_lock_live(cwd: &Path) -> bool {
    for dir in cwd.ancestors().take(MAX_TREE_DEPTH) {
        let dot_git = dir.join(".git");
        let Ok(meta) = std::fs::symlink_metadata(&dot_git) else {
            continue;
        };
        if meta.is_dir() {
            return dot_git.join("index.lock").exists();
        }
        let gitdir = std::fs::read_to_string(&dot_git).ok().and_then(|text| {
            text.lines()
                .find_map(|line| line.strip_prefix("gitdir:"))
                .map(|rest| dir.join(rest.trim()))
        });
        return match gitdir {
            Some(gitdir) => gitdir.join("index.lock").exists(),
            None => true,
        };
    }
    false
}

/// What an environment says about a process, and what its arguments say about
/// it as a shell. Neither changes while the process runs, so each is read once
/// per `(pid, start_us)` and kept across ticks: a selection reads up to a few
/// hundred of them, every time one is wanted. A read that failed is not kept.
#[derive(Debug, Default)]
pub(crate) struct MarkCache {
    env: HashMap<(i32, u64), EnvMarks>,
    tool_shell: HashMap<(i32, u64), bool>,
}

/// The markers a process's environment carries.
#[derive(Debug, Clone, Copy, Default)]
struct EnvMarks {
    /// The lease marker that was found, if any.
    lease: Option<&'static str>,
    heavy: bool,
}

impl MarkCache {
    fn env(&mut self, probe: &dyn Probe, p: &ReliefProc) -> Option<EnvMarks> {
        let key = (p.pid, p.start_us);
        if let Some(marks) = self.env.get(&key) {
            return Some(*marks);
        }
        let env = probe.environment(p.pid)?;
        // Any value counts, an empty one does not (`NAME=` is an unset one).
        let has = |name: &str| {
            let prefix = format!("{name}=");
            env.iter().any(|var| {
                var.strip_prefix(&prefix)
                    .is_some_and(|value| !value.is_empty())
            })
        };
        let marks = EnvMarks {
            lease: LEASE_MARKERS.into_iter().find(|name| has(name)),
            heavy: has(HEAVY_MARKER),
        };
        self.bound();
        self.env.insert(key, marks);
        Some(marks)
    }

    /// Whether `p` is an agent's tool-command shell, by its arguments. `None`
    /// when they cannot be read.
    fn tool_shell(&mut self, probe: &dyn Probe, p: &ReliefProc) -> Option<bool> {
        let key = (p.pid, p.start_us);
        if let Some(is) = self.tool_shell.get(&key) {
            return Some(*is);
        }
        let argv = probe.argv(p.pid)?;
        let is = argv.iter().any(|arg| arg.contains(TOOL_SHELL_MARK));
        self.bound();
        self.tool_shell.insert(key, is);
        Some(is)
    }

    /// Keeps the cache to what is running: dropped wholesale when it outgrows
    /// [`MAX_MARKS`], and refilled by the next selection.
    fn bound(&mut self) {
        if self.env.len() + self.tool_shell.len() >= MAX_MARKS {
            self.env.clear();
            self.tool_shell.clear();
        }
    }
}

/// The checks a process passes to be frozen, with what they read.
struct Screen<'a> {
    probe: &'a dyn Probe,
    board: &'a LeaseBoard,
    /// The lease board is refusing new builds: a holder's tree is then
    /// eligible, because a frozen build that nobody can replace is what
    /// relieves the host, and it resumes the moment the board stops refusing.
    refusing: bool,
    marks: &'a mut MarkCache,
    locks: &'a mut HashMap<PathBuf, bool>,
}

impl Screen<'_> {
    /// What the scan and the lease board say. `Ok(true)` when `p` holds a
    /// lease slot (eligible only while the board refuses); `Err` is the reason
    /// it may not be frozen.
    fn by_name(&self, p: &ReliefProc) -> Result<bool, String> {
        if is_git(&p.name) {
            return Err("a git process is in it".into());
        }
        if is_mcp(&p.name) {
            return Err(format!("{} looks like an MCP server", p.name));
        }
        if self.board.waiters.contains(&p.pid) {
            return Err(format!("{} waits for a build lease", p.name));
        }
        if self.board.holders.contains(&p.pid) {
            return if self.refusing {
                Ok(true)
            } else {
                Err(format!("{} holds a build lease", p.name))
            };
        }
        Ok(false)
    }

    /// What `p`'s environment says. `mac_heavy.sh`'s marker bars it always; the
    /// lease markers bar it unless the board is refusing (`Ok(true)`: a build).
    fn by_environment(&mut self, p: &ReliefProc) -> Result<bool, String> {
        let Some(marks) = self.marks.env(self.probe, p) else {
            return Err(format!("the environment of {} could not be read", p.name));
        };
        if marks.heavy {
            return Err(format!("{} carries {HEAVY_MARKER}", p.name));
        }
        match marks.lease {
            Some(_) if self.refusing => Ok(true),
            Some(marker) => Err(format!("{} carries {marker}", p.name)),
            None => Ok(false),
        }
    }

    /// Whether `p` works in a worktree whose git index is locked.
    fn by_directory(&mut self, p: &ReliefProc) -> Result<(), String> {
        let Some(cwd) = self.probe.cwd(p.pid) else {
            return Err(format!(
                "the working directory of {} could not be read",
                p.name
            ));
        };
        if *self
            .locks
            .entry(cwd.clone())
            .or_insert_with(|| index_lock_live(&cwd))
        {
            return Err(format!(
                "{} works in a worktree with a live git index.lock",
                p.name
            ));
        }
        Ok(())
    }

    /// Every check, for one process that joined a frozen subtree late.
    /// `Ok(true)` when it holds a lease slot (eligible only while the board
    /// refuses), which makes the whole subtree a holder.
    fn eligible(&mut self, p: &ReliefProc) -> Result<bool, String> {
        if p.exempt {
            return Err(format!("{} is exempt from throttling", p.name));
        }
        let holder = self.by_name(p)? | self.by_environment(p)?;
        self.by_directory(p)?;
        Ok(holder)
    }
}

/// Why a subtree may not be frozen (`Err`), or whether a build in it holds a
/// lease (`Ok(true)`, which is eligible only while the board refuses).
fn barred(
    chain: &[&ReliefProc],
    subtree: &[&ReliefProc],
    screen: &mut Screen<'_>,
) -> Result<bool, String> {
    for p in chain.iter().chain(subtree.iter()) {
        if p.exempt {
            return Err(format!("{} is exempt from throttling", p.name));
        }
    }
    let mut holder = false;
    for p in chain.iter().chain(subtree.iter()) {
        holder |= screen.by_name(p)?;
    }
    // Environments: the subtree's root and the first few members, and the
    // shells above it. `mac_heavy.sh`'s and the lease wrapper's markers are
    // inherited, so a marker anywhere in the chain reaches the programs below.
    for p in chain.iter().chain(subtree.iter().take(MAX_ENV_CHECKS)) {
        holder |= screen.by_environment(p)?;
    }
    for p in chain.iter().chain(subtree.iter()).take(MAX_CWD_CHECKS) {
        screen.by_directory(p)?;
    }
    Ok(holder)
}

/// The heaviest eligible subtree of one session, below the shells the agent
/// started, and why the heavier ones were passed over.
fn select_in_session(
    session: &ReliefSession,
    screen: &mut Screen<'_>,
    spent: &HashSet<(i32, u64)>,
    notes: &mut Vec<String>,
) -> Option<Selection> {
    let members = &session.members;
    let at_of: HashMap<i32, usize> = members
        .iter()
        .enumerate()
        .map(|(i, p)| (p.pid, i))
        .collect();
    // A parent that started after its child is a recycled pid, not a parent.
    let parent_of = |i: usize| {
        at_of
            .get(&members[i].ppid)
            .copied()
            .filter(|&p| members[p].start_us <= members[i].start_us)
    };
    let mut kids: Vec<Vec<usize>> = vec![Vec::new(); members.len()];
    for i in 0..members.len() {
        if let Some(p) = parent_of(i) {
            kids[p].push(i);
        }
    }
    // Only what runs below a shell the agent itself started to run a command:
    // one whose arguments say so ([`TOOL_SHELL_MARK`]). Every other direct
    // child of the root is something else (an MCP server's launcher, a
    // helper), and so is everything under it. A shell that cannot be read, or
    // reads as anything else, is not entered: unidentified is not eligible.
    let shells: Vec<usize> = (0..members.len())
        .filter(|&i| {
            members[i].ppid == session.root.pid
                && is_shell(&members[i].name)
                && !members[i].exempt
                && screen.marks.tool_shell(screen.probe, &members[i]) == Some(true)
        })
        .collect();
    if shells.is_empty() && members.iter().any(|m| m.ppid == session.root.pid) {
        notes.push(format!(
            "session {}: none of its child shells is identified as an agent tool shell",
            session.root.pid
        ));
    }
    let mut order = Vec::new();
    let mut seen = HashSet::new();
    let mut stack: Vec<(usize, usize)> = Vec::new();
    for &shell in &shells {
        stack.extend(kids[shell].iter().map(|&k| (k, 1)));
        while let Some((i, depth)) = stack.pop() {
            if depth > MAX_TREE_DEPTH || !seen.insert(i) {
                continue;
            }
            order.push(i);
            stack.extend(kids[i].iter().map(|&k| (k, depth + 1)));
        }
    }
    let position: HashMap<usize, usize> = order.iter().enumerate().map(|(n, &i)| (i, n)).collect();
    let mut sums: Vec<u64> = order
        .iter()
        .map(|&i| members[i].footprint.unwrap_or(0))
        .collect();
    // `order` is a pre-order walk of each tree, so folding it backwards adds a
    // process to its parent after its own children have been added to it.
    for n in (0..order.len()).rev() {
        if let Some(&parent) = parent_of(order[n]).and_then(|p| position.get(&p)) {
            let add = sums[n];
            sums[parent] = sums[parent].saturating_add(add);
        }
    }
    let mut ranked: Vec<usize> = (0..order.len())
        .filter(|&n| sums[n] >= MIN_FREEZE_BYTES)
        .collect();
    ranked.sort_by_key(|&n| (std::cmp::Reverse(sums[n]), members[order[n]].pid));
    for &n in ranked.iter().take(MAX_CANDIDATES_CHECKED) {
        let top = order[n];
        if spent.contains(&(members[top].pid, members[top].start_us)) {
            notes.push(format!(
                "{} was frozen already in this Severe episode",
                members[top].name
            ));
            continue;
        }
        let mut subtree = Vec::new();
        let mut walk = vec![top];
        while let Some(i) = walk.pop() {
            subtree.push(i);
            if subtree.len() > MAX_SUBTREE_MEMBERS {
                break;
            }
            walk.extend(kids[i].iter().rev().copied());
        }
        if subtree.len() > MAX_SUBTREE_MEMBERS {
            notes.push(format!(
                "{} has more than {MAX_SUBTREE_MEMBERS} processes",
                members[top].name
            ));
            continue;
        }
        let mut chain = Vec::new();
        let mut at = parent_of(top);
        while let Some(i) = at {
            chain.push(&members[i]);
            if chain.len() > MAX_TREE_DEPTH {
                break;
            }
            at = parent_of(i);
        }
        let subtree_procs: Vec<&ReliefProc> = subtree.iter().map(|&i| &members[i]).collect();
        let holder = match barred(&chain, &subtree_procs, screen) {
            Ok(holder) => holder,
            Err(why) => {
                notes.push(format!("{}: {why}", members[top].name));
                continue;
            }
        };
        return Some(Selection {
            session_pid: session.root.pid,
            session_start_us: session.root.start_us,
            terminal_session_id: session.terminal_session_id.clone(),
            root: members[top].clone(),
            members: subtree_procs.into_iter().cloned().collect(),
            footprint: sums[n],
            repo_path: None,
            holder,
        });
    }
    None
}

/// What a selection is asked for.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Want<'a> {
    /// Sessions that already have a subtree frozen.
    pub busy: &'a BTreeSet<(i32, u64)>,
    /// Most subtrees wanted.
    pub room: usize,
    /// The lease board is refusing new builds.
    pub refusing: bool,
    /// Processes this Severe episode already froze: never the root of another
    /// subtree.
    pub spent: &'a HashSet<(i32, u64)>,
}

/// The subtrees rung B stops: for each GitPulse-launched session in a trusted
/// repository that has none frozen already, its heaviest eligible subtree; the
/// heaviest `room` of those. `Err` when the lease board could not be read:
/// then nothing is chosen.
pub(crate) fn select(
    table: &ReliefTable,
    probe: &dyn Probe,
    trusted: &dyn Fn(&Path) -> bool,
    want: &Want<'_>,
    marks: &mut MarkCache,
    notes: &mut Vec<String>,
) -> Result<Vec<Selection>, String> {
    let Want {
        busy,
        room,
        refusing,
        spent,
    } = *want;
    if room == 0 {
        return Ok(Vec::new());
    }
    let board = probe
        .lease_board()
        .map_err(|why| format!("the build lease board could not be read: {why}"))?;
    let mut locks: HashMap<PathBuf, bool> = HashMap::new();
    let mut screen = Screen {
        probe,
        board: &board,
        refusing,
        marks,
        locks: &mut locks,
    };
    let mut picks = Vec::new();
    for session in &table.sessions {
        if !session.launched || busy.contains(&(session.root.pid, session.root.start_us)) {
            continue;
        }
        let Some(cwd) = probe.cwd(session.root.pid) else {
            notes.push(format!(
                "session {}: its working directory could not be read",
                session.root.pid
            ));
            continue;
        };
        if !trusted(&cwd) {
            notes.push(format!(
                "session {}: its repository is not trusted",
                session.root.pid
            ));
            continue;
        }
        if let Some(mut pick) = select_in_session(session, &mut screen, spent, notes) {
            pick.repo_path = Some(
                crate::engine::git_cli::find_git_root(&cwd)
                    .unwrap_or(cwd)
                    .to_string_lossy()
                    .into_owned(),
            );
            picks.push(pick);
        }
    }
    picks.sort_by_key(|p| (std::cmp::Reverse(p.footprint), p.root.pid));
    picks.truncate(room);
    Ok(picks)
}

// ---------------------------------------------------------------------------
// The ledger: one owner of priority changes.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SlowOwner {
    Relief,
    Steward,
}

/// Every process GitPulse has put in the background, and who asked, by
/// namespace. The app's relief and the steward share one (the empty path); a
/// relief built for a test is its own directory's, so two reliefs that share
/// nothing never see each other's pids.
type Ledger = BTreeMap<PathBuf, BTreeMap<i32, (u64, SlowOwner)>>;

static LEDGER: Mutex<Ledger> = Mutex::new(BTreeMap::new());

fn ledger_get(ledger: &Ledger, ns: &Path, pid: i32) -> Option<(u64, SlowOwner)> {
    ledger.get(ns).and_then(|space| space.get(&pid)).copied()
}

fn ledger_put(ledger: &mut Ledger, ns: &Path, pid: i32, start_us: u64, owner: SlowOwner) {
    ledger
        .entry(ns.to_path_buf())
        .or_default()
        .insert(pid, (start_us, owner));
}

/// Forgets `pid` when the entry is for the process that started at `start_us`.
fn ledger_clear(ledger: &mut Ledger, ns: &Path, pid: i32, start_us: u64) {
    if let Some(space) = ledger.get_mut(ns) {
        if space.get(&pid).is_some_and(|(start, _)| *start == start_us) {
            space.remove(&pid);
        }
        if space.is_empty() {
            ledger.remove(ns);
        }
    }
}

/// What the steward slowed: its own to put back.
fn ledger_stewards(ledger: &Ledger, ns: &Path) -> HashSet<(i32, u64)> {
    ledger
        .get(ns)
        .into_iter()
        .flatten()
        .filter(|(_, (_, owner))| *owner == SlowOwner::Steward)
        .map(|(pid, (start_us, _))| (*pid, *start_us))
        .collect()
}

/// Serialises every action that changes a process (a tick's, and teardown's):
/// teardown must not race a tick that is about to stop something.
static ACT: Mutex<()> = Mutex::new(());

/// Set by [`shutdown`]: from then on no new process is touched.
static STOPPING: AtomicBool = AtomicBool::new(false);

/// Where the app's relief keeps its journal, for [`shutdown`].
static APP_DIR: OnceLock<PathBuf> = OnceLock::new();

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The steward's slowdown, through the relief's ledger. It may slow what the
/// relief has not, and it cannot put back what the relief slowed (`Ok(false)`:
/// "already in the state asked", which is what the steward reads as "not mine
/// to restore"); the relief puts that back itself when pressure eases.
pub(crate) fn steward_throttle(pid: i32, start_us: u64, enable: bool) -> Result<bool, String> {
    steward_throttle_with(&OsActuator, Path::new(""), pid, start_us, enable)
}

fn steward_throttle_with(
    actuator: &dyn Actuator,
    ns: &Path,
    pid: i32,
    start_us: u64,
    enable: bool,
) -> Result<bool, String> {
    let mut ledger = lock(&LEDGER);
    if enable {
        let changed = actuator.set_background(pid, start_us, true)?;
        if changed {
            ledger_put(&mut ledger, ns, pid, start_us, SlowOwner::Steward);
        }
        return Ok(changed);
    }
    if ledger_get(&ledger, ns, pid) == Some((start_us, SlowOwner::Relief)) {
        return Ok(false);
    }
    let result = actuator.set_background(pid, start_us, false);
    ledger_clear(&mut ledger, ns, pid, start_us);
    result
}

// ---------------------------------------------------------------------------
// The journal.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum JournalRung {
    Slow,
    Freeze,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct JournalEntry {
    pid: i32,
    start_us: u64,
    session_id: String,
    at_unix_ms: u64,
    rung: JournalRung,
    /// The process was read as running (not stopped) immediately before the
    /// relief stopped it, so the stop and the continue are the relief's own.
    /// A resume acts only on entries that say so: an entry without it (an
    /// older writer's, or a hand-edited file) is not known to be ours, and
    /// continuing a process somebody else stopped would undo their pause.
    #[serde(default)]
    was_running: bool,
}

#[derive(Debug, Serialize, Deserialize)]
struct Journal {
    version: u32,
    writer_pid: u32,
    writer_start_us: u64,
    entries: Vec<JournalEntry>,
}

fn journal_path(dir: &Path) -> PathBuf {
    dir.join(JOURNAL_DIR).join(JOURNAL_FILE)
}

/// Records `entries`, or removes the journal when there are none.
fn write_journal(dir: &Path, entries: &[JournalEntry]) -> Result<(), String> {
    let pid = std::process::id();
    write_journal_as(dir, entries, pid, super::process_start_us(pid).unwrap_or(0))
}

/// [`write_journal`] naming `writer_pid` (started at `writer_start_us`) as the
/// GitPulse process that owns the entries.
fn write_journal_as(
    dir: &Path,
    entries: &[JournalEntry],
    writer_pid: u32,
    writer_start_us: u64,
) -> Result<(), String> {
    let path = journal_path(dir);
    if entries.is_empty() {
        return match std::fs::symlink_metadata(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.to_string()),
            Ok(meta) if meta.file_type().is_symlink() || !meta.is_file() => {
                Err("refusing to remove a relief journal that is not a regular file".into())
            }
            Ok(_) => std::fs::remove_file(&path).map_err(|e| e.to_string()),
        };
    }
    let journal = Journal {
        version: 1,
        writer_pid,
        writer_start_us,
        entries: entries.to_vec(),
    };
    let bytes = serde_json::to_vec(&journal).map_err(|e| e.to_string())?;
    let sub = dir.join(JOURNAL_DIR);
    crate::hooks::advisory_once::private_dir(&sub)?;
    crate::hooks::advisory_once::write_private_atomic(&sub, JOURNAL_FILE, &bytes)
}

fn read_journal(dir: &Path) -> Result<Option<Journal>, String> {
    use std::io::Read as _;
    let path = journal_path(dir);
    match std::fs::symlink_metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
        Ok(meta) if meta.file_type().is_symlink() || !meta.is_file() => {
            return Err("the relief journal is not a regular file".into())
        }
        Ok(_) => {}
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NOFOLLOW);
    }
    let mut raw = Vec::new();
    opts.open(&path)
        .and_then(|f| f.take(JOURNAL_MAX_BYTES + 1).read_to_end(&mut raw))
        .map_err(|e| e.to_string())?;
    if raw.len() as u64 > JOURNAL_MAX_BYTES {
        return Err("the relief journal is too large to read".into());
    }
    let journal: Journal = serde_json::from_slice(&raw).map_err(|e| e.to_string())?;
    if journal.version != 1 {
        return Err(format!("the relief journal is version {}", journal.version));
    }
    Ok(Some(journal))
}

/// What a replay of the journal did.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Replay {
    pub resumed: u32,
    pub restored: u32,
    pub failures: Vec<String>,
    /// The journal belongs to this other GitPulse process, which is still
    /// running: nothing was touched.
    pub left_to: Option<u32>,
}

/// Resumes and restores everything the journal in `dir` lists, then removes
/// it. An entry is cleared after its resume was attempted, whether or not it
/// worked, and each failure is returned. Shared by the election-win recovery,
/// the drop and the teardown.
pub(crate) fn replay_journal(dir: &Path, ns: &Path, actuator: &dyn Actuator) -> Replay {
    let mut out = Replay::default();
    let journal = match read_journal(dir) {
        Ok(Some(journal)) => journal,
        Ok(None) => return out,
        Err(why) => {
            out.failures.push(format!(
                "the relief journal could not be read, so what it lists was not resumed: {why}"
            ));
            let _ = std::fs::remove_file(journal_path(dir));
            return out;
        }
    };
    // A journal another GitPulse that is still running wrote is that one's:
    // its frozen subtrees are not ours to continue, and a GitPulse that only
    // reads the host-load snapshot (it lost the writer election) must not
    // undo the writer's relief when it quits. The steward's journal follows the
    // same rule (`restore_throttle_journal`).
    if journal.writer_pid != std::process::id()
        && super::process_start_us(journal.writer_pid)
            .is_some_and(|live| live == journal.writer_start_us)
    {
        out.left_to = Some(journal.writer_pid);
        return out;
    }
    // Parent-first in the file, so resume in reverse: a child never runs
    // while its parent is still stopped and would be blocked on it.
    for entry in journal.entries.iter().rev() {
        match entry.rung {
            // Only an entry read as running before it was stopped is ours to
            // continue; anything else may be a process somebody else stopped.
            JournalRung::Freeze if !entry.was_running => {}
            JournalRung::Freeze => match actuator.signal(entry.pid, entry.start_us, Signal::Cont) {
                Ok(Delivery::Delivered) => out.resumed += 1,
                Ok(Delivery::Gone) => {}
                Err(why) => out.failures.push(format!(
                    "could not resume process {} of session {}: {why}",
                    entry.pid, entry.session_id
                )),
            },
            JournalRung::Slow => match restore_priority(actuator, ns, entry.pid, entry.start_us) {
                Ok(true) => out.restored += 1,
                Ok(false) => {}
                Err(why) => out.failures.push(format!(
                    "could not restore the priority of process {}: {why}",
                    entry.pid
                )),
            },
        }
    }
    sweep_inheritors(&journal.entries, ns, actuator, &mut out);
    if let Err(why) = write_journal(dir, &[]) {
        out.failures
            .push(format!("the relief journal could not be removed: {why}"));
    }
    out
}

/// The journal lists the processes the relief slowed *by name*. A child one of
/// them started after it was slowed inherited the priority without ever being
/// named, so a restore of the listed ones alone leaves it in the background.
/// One best-effort pass over a single read of the process table puts those
/// back too, bounded by [`MAX_SWEPT`]. Only a descendant that started at or
/// after its ancestor's journal stamp qualifies, never the steward's (or
/// anything under one) and never one that is not in the background now.
///
/// Residual: a descendant born after the slowing that put *itself* in the
/// background (`taskpolicy -b`) is restored with them; the stamp separates
/// what was already there, not why a newer process is in the background.
fn sweep_inheritors(
    entries: &[JournalEntry],
    ns: &Path,
    actuator: &dyn Actuator,
    out: &mut Replay,
) {
    let slowed: Vec<&JournalEntry> = entries
        .iter()
        .filter(|e| e.rung == JournalRung::Slow)
        .collect();
    if slowed.is_empty() {
        return;
    }
    let table = actuator.process_table();
    if table.is_empty() {
        return;
    }
    let stewards = ledger_stewards(&lock(&LEDGER), ns);
    // The mark a descendant must have started after, by pid: the stamp of the
    // nearest listed ancestor, in microseconds.
    let mut under: HashMap<i32, u64> = slowed
        .iter()
        .map(|e| (e.pid, e.at_unix_ms.saturating_mul(1000)))
        .collect();
    let named: HashSet<i32> = under.keys().copied().collect();
    for _ in 0..MAX_TREE_DEPTH {
        let before = under.len();
        for &(pid, ppid, start_us) in &table {
            if under.contains_key(&pid) || stewards.contains(&(pid, start_us)) {
                continue;
            }
            if let Some(&mark) = under.get(&ppid) {
                under.insert(pid, mark);
            }
        }
        if under.len() == before {
            break;
        }
    }
    let mut swept = 0;
    for &(pid, _, start_us) in &table {
        if named.contains(&pid) || swept >= MAX_SWEPT {
            continue;
        }
        let Some(&mark) = under.get(&pid) else {
            continue;
        };
        if start_us < mark || actuator.is_background(pid, start_us) != Some(true) {
            continue;
        }
        match restore_priority(actuator, ns, pid, start_us) {
            Ok(true) => {
                out.restored += 1;
                swept += 1;
            }
            Ok(false) => {}
            Err(why) => out.failures.push(format!(
                "could not restore the priority of process {pid}, which inherited it: {why}"
            )),
        }
    }
}

/// Restores a priority, treating a process that ended as nothing to restore.
/// The ledger is not held across the call.
fn restore_priority(
    actuator: &dyn Actuator,
    ns: &Path,
    pid: i32,
    start_us: u64,
) -> Result<bool, String> {
    let result = match actuator.set_background(pid, start_us, false) {
        Err(why) if process_is_gone(&why) => Ok(false),
        other => other,
    };
    ledger_clear(&mut lock(&LEDGER), ns, pid, start_us);
    result
}

/// The teardown's part: resumes and restores whatever the app's relief put
/// there, and keeps the relief from touching anything afterwards. Safe to call
/// when no relief ran. Called before the terminals are closed: a `SIGHUP` to a
/// stopped process stays pending until it is continued.
pub fn shutdown() {
    // Only an app that started a relief has anything to stop: a process that
    // never did (a test running the teardown) must not stop later reliefs.
    let Some(dir) = APP_DIR.get() else {
        return;
    };
    STOPPING.store(true, Ordering::SeqCst);
    shutdown_within(
        dir,
        Path::new(""),
        APP_WATCH.get().map(|watch| &**watch),
        &OsActuator,
        SHUTDOWN_WAIT,
    );
    *lock(&SNAPSHOT) = None;
    rest_the_status_file(dir);
}

/// The transition to rest on the way out: what this process published is
/// replaced by an at-rest status, so the next reader does not take the last
/// claim for a live one. Only a file this process wrote: a GitPulse that lost
/// the writer election must not say the writer's relief is at rest.
fn rest_the_status_file(dir: &Path) {
    use super::relief_status as file;
    let ours = match file::read_status(dir) {
        file::ReliefOutcome::Fresh { status } | file::ReliefOutcome::Stale { status, .. } => {
            status.writer_pid == std::process::id()
        }
        _ => false,
    };
    if !ours {
        return;
    }
    let mode = match crate::tool_config::host_load_supervisor() {
        SupervisorMode::Off => file::ReliefMode::Off,
        SupervisorMode::Advise => file::ReliefMode::Advise,
        SupervisorMode::Act => file::ReliefMode::Act,
    };
    let mut status = file::ReliefStatus::at_rest(mode, file::now_ms());
    status.writer_start_us = super::process_start_us(std::process::id()).unwrap_or(0);
    if let Err(why) = file::write_status(dir, &status) {
        log::warn!(target: "relief", "the status file could not be set to rest: {why}");
    }
}

/// [`shutdown`]'s work. The frozen processes are continued first, from the
/// in-memory copy ([`Watch`]), which needs neither the journal nor the action
/// lock: a tick that hangs mid-action (a disk that stalls under the very
/// pressure this answers) must not hold up the app's quit, or leave a build
/// stopped. The journal is replayed too when the action lock comes within
/// `wait`. Returns whether it did.
pub(crate) fn shutdown_within(
    dir: &Path,
    ns: &Path,
    watch: Option<&Watch>,
    actuator: &dyn Actuator,
    wait: Duration,
) -> bool {
    if let Some(watch) = watch {
        watch.continue_all(actuator);
    }
    let Some(_act) = lock_within(&ACT, wait) else {
        log::warn!(
            target: "relief",
            "shutdown: a tick held the action lock for over {} ms; the frozen processes were continued from memory and the journal was left for the next start",
            wait.as_millis()
        );
        return false;
    };
    let replay = replay_journal(dir, ns, actuator);
    log_replay("shutdown", &replay);
    true
}

/// `mutex`, waiting at most `wait` for it.
fn lock_within<T>(mutex: &Mutex<T>, wait: Duration) -> Option<std::sync::MutexGuard<'_, T>> {
    use std::sync::TryLockError;
    let deadline = Instant::now() + wait;
    loop {
        match mutex.try_lock() {
            Ok(guard) => return Some(guard),
            Err(TryLockError::Poisoned(poisoned)) => return Some(poisoned.into_inner()),
            Err(TryLockError::WouldBlock) => {
                if Instant::now() >= deadline {
                    return None;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

/// One frozen process as the watchdog sees it.
#[derive(Debug, Clone, Copy)]
struct Watched {
    pid: i32,
    start_us: u64,
    /// When the freeze began, on the monotonic clock.
    since: Instant,
}

#[derive(Debug, Default)]
struct WatchState {
    frozen: Vec<Watched>,
    /// The last tick, on the monotonic clock.
    last_tick: Option<Instant>,
    /// What the watchdog continued, until the next tick takes account of it:
    /// the relief's own record of a freeze must not outlive the processes
    /// being stopped.
    released: Vec<(i32, u64)>,
    /// Why the watchdog thread did not start; the relief reports it.
    start_error: Option<String>,
}

/// The frozen processes and the tick's pulse, kept where an independent thread
/// can reach them: behind a lock of their own, which is held for a copy and
/// never across a signal, a file or [`ACT`]. The sampler keeps it current; the
/// watchdog thread (and [`shutdown`]) read it, so a sampler that hangs, loses
/// the writer election with a freeze standing, or stalls on a disk write cannot
/// leave a process stopped.
#[derive(Debug, Default)]
pub(crate) struct Watch {
    state: Mutex<WatchState>,
}

impl Watch {
    fn beat(&self, now: Instant) {
        lock(&self.state).last_tick = Some(now);
    }

    fn set(&self, frozen: Vec<Watched>) {
        lock(&self.state).frozen = frozen;
    }

    /// One more stopped process, added once its stop was delivered: the
    /// watchdog only ever continues what the relief really stopped.
    fn add(&self, watched: Watched) {
        lock(&self.state).frozen.push(watched);
    }

    /// What the watchdog continued since the last call.
    fn take_released(&self) -> Vec<(i32, u64)> {
        std::mem::take(&mut lock(&self.state).released)
    }

    fn start_error(&self) -> Option<String> {
        lock(&self.state).start_error.clone()
    }

    /// Continues what is overdue: every entry when the tick has not advanced
    /// for [`WATCHDOG_STALL`], else the entries older than
    /// [`FREEZE_CAP`] + [`WATCHDOG_GRACE`] (the sampler's own cap resume has
    /// had a minute to act). Returns what it continued.
    pub(crate) fn pass(&self, actuator: &dyn Actuator, now: Instant) -> Vec<(i32, u64)> {
        let due = {
            let mut state = lock(&self.state);
            let stalled = state
                .last_tick
                .is_some_and(|tick| now.saturating_duration_since(tick) >= WATCHDOG_STALL);
            let (due, keep): (Vec<Watched>, Vec<Watched>) =
                state.frozen.iter().copied().partition(|w| {
                    stalled || now.saturating_duration_since(w.since) >= FREEZE_CAP + WATCHDOG_GRACE
                });
            state.frozen = keep;
            due
        };
        let sent = Self::send_cont(&due, actuator);
        lock(&self.state).released.extend(sent.iter().copied());
        sent
    }

    /// Continues everything, now.
    fn continue_all(&self, actuator: &dyn Actuator) {
        let all = std::mem::take(&mut lock(&self.state).frozen);
        Self::send_cont(&all, actuator);
    }

    /// Last stopped first: a child never runs while its parent is stopped.
    fn send_cont(entries: &[Watched], actuator: &dyn Actuator) -> Vec<(i32, u64)> {
        let mut sent = Vec::new();
        for w in entries.iter().rev() {
            match actuator.signal(w.pid, w.start_us, Signal::Cont) {
                Ok(_) => sent.push((w.pid, w.start_us)),
                Err(why) => log::warn!(
                    target: "relief",
                    "could not continue pid {} from memory: {why}",
                    w.pid
                ),
            }
        }
        sent
    }
}

/// The app's [`Watch`], shared by every relief this process builds (the
/// sampler rebuilds its relief after a panic) and by [`shutdown`].
static APP_WATCH: OnceLock<std::sync::Arc<Watch>> = OnceLock::new();

/// The app's watch, started once with its thread.
fn app_watch() -> std::sync::Arc<Watch> {
    let watch = APP_WATCH.get_or_init(|| {
        let watch = std::sync::Arc::new(Watch::default());
        let looks = watch.clone();
        let started = crate::supervisor::spawn_loop("relief-watchdog", WATCHDOG_EVERY, move || {
            let freed = looks.pass(&OsActuator, Instant::now());
            if !freed.is_empty() {
                log::warn!(
                    target: "relief",
                    "the watchdog continued {} stopped process(es) the sampler had not",
                    freed.len()
                );
            }
        });
        if let Err(why) = started {
            log::error!(target: "relief", "the relief watchdog did not start: {why}");
            // The relief puts it in its snapshot too: a freeze with no
            // watchdog behind it is a fact the status must carry.
            lock(&watch.state).start_error = Some(why.to_string());
        }
        watch
    });
    watch.clone()
}

fn log_replay(when: &str, replay: &Replay) {
    if replay.resumed > 0 || replay.restored > 0 {
        log::info!(
            target: "relief",
            "{when}: resumed {} stopped and restored {} slowed processes",
            replay.resumed,
            replay.restored
        );
    }
    for failure in &replay.failures {
        log::warn!(target: "relief", "{when}: {failure}");
    }
}

// ---------------------------------------------------------------------------
// Status.
// ---------------------------------------------------------------------------

/// How far the relief has gone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rung {
    None,
    /// Rung A: descendants are in the background.
    Slow,
    /// Rung B: a subtree is stopped (or, in `advise`, would be).
    Freeze,
}

/// One frozen subtree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrozenSubtree {
    /// The subtree's first process.
    pub root_pid: i32,
    pub start_us: u64,
    /// The agent session's root process.
    pub session_pid: i32,
    /// The session's identity, `<pid>-<start_us>`.
    pub session_id: String,
    /// The GitPulse terminal the session runs in (`GITPULSE_SESSION_ID`).
    pub terminal_session_id: Option<String>,
    /// The checkout the session works in, when it has one.
    pub repo_path: Option<String>,
    /// The subtree's first process by name (`cargo`, `node`). The scan keeps no
    /// command line for an ordinary process, so this is its name, not its
    /// arguments.
    pub command: String,
    pub footprint_bytes: u64,
    pub members: u32,
    pub since_unix_ms: u64,
    /// `advise` mode: chosen and shown, never signalled.
    pub advised_only: bool,
}

/// What the relief did last.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionKind {
    Slow,
    Freeze,
    /// Processes continued or put back to normal priority.
    Resume,
    /// A freeze ended because [`FREEZE_CAP`] was reached.
    CapResume,
    /// Something was called for and not done: nothing qualified, the journal
    /// or the lease board could not be read.
    Refused,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastAction {
    pub at_unix_ms: u64,
    pub kind: ActionKind,
    pub detail: String,
}

/// A signal or priority change that did not take, or a file that could not be
/// written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignalFailure {
    pub at_unix_ms: u64,
    pub pid: Option<i32>,
    pub what: String,
}

/// What the relief is doing, read-only: [`snapshot`].
#[derive(Debug, Clone, PartialEq)]
pub struct ReliefSnapshot {
    pub mode: SupervisorMode,
    pub rung: Rung,
    /// The memory input that bears on the host now.
    pub binding: Option<Binding>,
    pub tier: Option<Tier>,
    pub frozen: Vec<FrozenSubtree>,
    /// Processes in the background because of the relief.
    pub slowed: u32,
    /// `advise` mode: the processes that would be slowed.
    pub would_slow: u32,
    pub last_action: Option<LastAction>,
    /// The latest failures, newest last, at most [`MAX_FAILURES`].
    pub failures: Vec<SignalFailure>,
    pub failure_count: u64,
    /// Freezes that ended at [`FREEZE_CAP`].
    pub cap_hits: u64,
    pub updated_unix_ms: u64,
}

impl ReliefSnapshot {
    fn idle(mode: SupervisorMode) -> Self {
        Self {
            mode,
            rung: Rung::None,
            binding: None,
            tier: None,
            frozen: Vec::new(),
            slowed: 0,
            would_slow: 0,
            last_action: None,
            failures: Vec::new(),
            failure_count: 0,
            cap_hits: 0,
            updated_unix_ms: unix_ms(),
        }
    }
}

static SNAPSHOT: Mutex<Option<ReliefSnapshot>> = Mutex::new(None);

/// The app's relief, read-only: what the sampler's last tick left, or idle in
/// the stored mode when no relief has run in this process.
pub fn snapshot() -> ReliefSnapshot {
    lock(&SNAPSHOT)
        .clone()
        .unwrap_or_else(|| ReliefSnapshot::idle(crate::tool_config::host_load_supervisor()))
}

/// One process a [`resume_all`] continued, or failed to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PidOutcome {
    pub pid: i32,
    pub start_us: u64,
    /// `true` when the process was continued, or had already ended.
    pub ok: bool,
    /// Why not, when it was not.
    pub detail: Option<String>,
}

/// What [`resume_all`] did.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ResumeOutcome {
    /// Every journaled frozen process, last stopped first.
    pub results: Vec<PidOutcome>,
    /// The relief will not freeze again until the tier drops below Severe.
    pub held_off: bool,
    /// Why nothing was done at all (no relief here, another GitPulse's journal,
    /// an unreadable one).
    pub refused: Option<String>,
    /// A tick held the action lock for the whole wait ([`ACT_WAIT`]): nothing
    /// was done, and the request may be made again.
    pub busy: bool,
}

/// What the relief's sampler is told about a [`resume_all`] it did not make
/// itself.
#[derive(Debug, Clone, Default)]
struct External {
    generation: u64,
    hold_off: bool,
    reason: String,
    /// Only the pauses that began at or before this (Unix ms); all when `None`.
    up_to_ms: Option<u64>,
}

/// Per relief directory, so the reliefs of two directories (two tests) never
/// hear of each other's requests.
static EXTERNAL: Mutex<BTreeMap<PathBuf, External>> = Mutex::new(BTreeMap::new());

fn external_of(dir: &Path) -> External {
    lock(&EXTERNAL).get(dir).cloned().unwrap_or_default()
}

/// Resumes every frozen subtree now, from any thread. Each journaled process
/// is continued through `procguard::signal::signal_process` (a recycled pid is
/// refused, and reported), and its journal entry is cleared once that was
/// attempted. With `hold_off_episode` the relief does not freeze again until
/// the memory tier has dropped below Severe. Rung A is not touched.
pub fn resume_all(hold_off_episode: bool, reason: &str) -> ResumeOutcome {
    match APP_DIR.get() {
        Some(dir) => resume_all_in(dir, &OsActuator, hold_off_episode, reason),
        None => ResumeOutcome {
            held_off: false,
            refused: Some("no pressure relief is running in this process".into()),
            ..ResumeOutcome::default()
        },
    }
}

pub(crate) fn resume_all_in(
    dir: &Path,
    actuator: &dyn Actuator,
    hold_off_episode: bool,
    reason: &str,
) -> ResumeOutcome {
    resume_up_to_in(dir, actuator, hold_off_episode, reason, None)
}

/// [`resume_all_in`] limited to the pauses that began at or before `up_to_ms`
/// (Unix milliseconds): a person's request resumes what they saw, never a pause
/// that began after they asked. Every journal entry of one pause carries that
/// pause's start, so a pause is resumed whole or not at all.
pub(crate) fn resume_up_to_in(
    dir: &Path,
    actuator: &dyn Actuator,
    hold_off_episode: bool,
    reason: &str,
    up_to_ms: Option<u64>,
) -> ResumeOutcome {
    resume_up_to_within(dir, actuator, hold_off_episode, reason, up_to_ms, ACT_WAIT)
}

/// [`resume_up_to_in`] waiting at most `wait` for the action lock, so a tick
/// stalled on a disk write cannot hang the caller (a command thread, the
/// request consumer on the sampler's own thread): the answer is
/// [`ResumeOutcome::busy`] and nothing was done.
pub(crate) fn resume_up_to_within(
    dir: &Path,
    actuator: &dyn Actuator,
    hold_off_episode: bool,
    reason: &str,
    up_to_ms: Option<u64>,
    wait: Duration,
) -> ResumeOutcome {
    let mut out = ResumeOutcome::default();
    let Some(_act) = lock_within(&ACT, wait) else {
        out.busy = true;
        out.refused = Some(format!(
            "a pass of the relief held the action lock for over {} ms",
            wait.as_millis()
        ));
        return out;
    };
    let journal = match read_journal(dir) {
        Ok(journal) => journal,
        Err(why) => {
            out.refused = Some(format!("the relief journal could not be read: {why}"));
            return out;
        }
    };
    if let Some(journal) = journal {
        if journal.writer_pid != std::process::id()
            && super::process_start_us(journal.writer_pid)
                .is_some_and(|live| live == journal.writer_start_us)
        {
            out.refused = Some(format!(
                "the frozen processes belong to GitPulse process {}",
                journal.writer_pid
            ));
            return out;
        }
        let (covered, rest): (Vec<JournalEntry>, Vec<JournalEntry>) =
            journal.entries.into_iter().partition(|e| {
                e.rung == JournalRung::Freeze && up_to_ms.is_none_or(|up_to| e.at_unix_ms <= up_to)
            });
        // Only an entry read as running before it was stopped is the relief's
        // to continue; one that is not is dropped from the file, not signalled.
        let frozen = covered.iter().filter(|e| e.was_running);
        for entry in frozen.rev() {
            let (ok, detail) = match actuator.signal(entry.pid, entry.start_us, Signal::Cont) {
                Ok(_) => (true, None),
                Err(why) => (false, Some(why)),
            };
            out.results.push(PidOutcome {
                pid: entry.pid,
                start_us: entry.start_us,
                ok,
                detail,
            });
        }
        // Cleared after the attempt, failures included; the slowed entries stay.
        if let Err(why) = write_journal(dir, &rest) {
            out.refused = Some(format!("the relief journal could not be trimmed: {why}"));
        }
    }
    out.held_off = hold_off_episode;
    let mut all = lock(&EXTERNAL);
    let external = all.entry(dir.to_path_buf()).or_default();
    external.generation += 1;
    external.hold_off = hold_off_episode;
    external.reason = reason.to_string();
    external.up_to_ms = up_to_ms;
    out
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

// ---------------------------------------------------------------------------
// The supervisor.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Frozen {
    info: FrozenSubtree,
    session: (i32, u64),
    root: (i32, u64),
    /// The processes this freeze stopped, parent first.
    members: Vec<(i32, u64)>,
    /// When it was frozen, on the monotonic clock.
    since: Instant,
    /// A build holding a lease slot is in it: continued on the first tick the
    /// lease board stops refusing.
    holder: bool,
    /// Members somebody else had stopped already: left alone, never stopped,
    /// never continued, and not asked about again.
    left: BTreeSet<(i32, u64)>,
}

/// The relief. One per sampler; it lives on the sampler's thread and is
/// ticked only while that sampler holds the writer election.
pub(crate) struct Relief {
    dir: PathBuf,
    /// The ledger namespace: the app's relief shares the steward's; any other
    /// relief is its own directory's.
    ns: PathBuf,
    watch: std::sync::Arc<Watch>,
    marks: MarkCache,
    /// The lease board was refusing new builds at the last tick.
    refusing: bool,
    /// Where the published status file (`relief_status`) and the resume
    /// request live; `None` publishes nothing (a relief a test builds).
    status_dir: Option<PathBuf>,
    /// The status last written, without its time, to write on a change.
    last_written: Option<super::relief_status::ReliefStatus>,
    /// The last write failed (logged once, retried every tick).
    status_write_failed: bool,
    #[cfg(test)]
    table_override: Option<std::sync::Arc<dyn Fn() -> ReliefTable + Send + Sync>>,
    actuator: Box<dyn Actuator>,
    probe: Box<dyn Probe>,
    mode: Box<dyn Fn() -> SupervisorMode + Send>,
    trusted: Box<dyn Fn(&Path) -> bool + Send>,
    trigger: Trigger,
    /// Processes this relief put in the background (its ledger share).
    slowed: BTreeMap<i32, u64>,
    /// For each of `slowed`, the newest process start time in the scan that
    /// slowed it: a process that started after that read inherited the
    /// priority; one that started at or before it was already there and is not
    /// the restore's to touch.
    slowed_mark: BTreeMap<i32, u64>,
    /// Already in the background when first seen and not by a parent of ours:
    /// someone else's choice, left alone until the slowing ends.
    foreign: BTreeSet<(i32, u64)>,
    frozen: Vec<Frozen>,
    journal: Vec<JournalEntry>,
    status: ReliefSnapshot,
    /// Whether [`snapshot`] serves this relief's state: only the app's does, so
    /// a relief a test builds never overwrites it.
    publishing: bool,
    /// The last [`resume_all`] this relief has taken account of.
    seen_external: u64,
    /// Samples since the last pass of rung A, `None` when none has run in this
    /// slowing.
    slow_since_pass: Option<u32>,
    recovered: bool,
    /// Every process a freeze of this Severe episode stopped, and their
    /// subtrees' roots: none is chosen again until the episode ends.
    spent: HashSet<(i32, u64)>,
    /// The trigger's [`Trigger::episode`] when `spent` was last cleared.
    episode_seen: u64,
    /// A person's resume request that found the action lock busy, kept (it
    /// was already taken from its file) and tried again each tick until its
    /// time to live is up.
    pending_request: Option<super::relief_status::ResumeRequest>,
    /// How long a request waits for the action lock.
    act_wait: Duration,
    /// The watchdog thread's failure to start is in the snapshot's failures.
    watchdog_reported: bool,
    /// A test's hook, called just before the tick takes the action lock.
    #[cfg(test)]
    before_act: Option<std::sync::Arc<dyn Fn() + Send + Sync>>,
}

impl Relief {
    pub(crate) fn new(
        dir: PathBuf,
        actuator: Box<dyn Actuator>,
        probe: Box<dyn Probe>,
        mode: Box<dyn Fn() -> SupervisorMode + Send>,
        trusted: Box<dyn Fn(&Path) -> bool + Send>,
    ) -> Self {
        let initial = mode();
        let seen_external = external_of(&dir).generation;
        Self {
            ns: dir.clone(),
            watch: std::sync::Arc::default(),
            marks: MarkCache::default(),
            refusing: false,
            status_dir: None,
            last_written: None,
            status_write_failed: false,
            #[cfg(test)]
            table_override: None,
            dir,
            actuator,
            probe,
            mode,
            trusted,
            trigger: Trigger::default(),
            slowed: BTreeMap::new(),
            slowed_mark: BTreeMap::new(),
            foreign: BTreeSet::new(),
            frozen: Vec::new(),
            journal: Vec::new(),
            status: ReliefSnapshot::idle(initial),
            publishing: false,
            seen_external,
            slow_since_pass: None,
            recovered: false,
            spent: HashSet::new(),
            episode_seen: 0,
            pending_request: None,
            act_wait: ACT_WAIT,
            watchdog_reported: false,
            #[cfg(test)]
            before_act: None,
        }
    }

    /// The relief on this machine: real signals, the stored mode, trust from
    /// the grant store. Remembers `dir` for [`shutdown`].
    pub(crate) fn os(dir: PathBuf) -> Self {
        let _ = APP_DIR.set(dir.clone());
        let mut relief = Self::new(
            dir.clone(),
            Box::new(OsActuator),
            Box::new(OsProbe { config: dir }),
            Box::new(crate::tool_config::host_load_supervisor),
            Box::new(repository_trusted),
        )
        .publishing();
        // The steward's slowing and this relief's are one ledger, and the
        // watchdog thread and the teardown reach the frozen ones through one
        // watch.
        relief.ns = PathBuf::new();
        relief.watch = app_watch();
        relief.status_dir = Some(relief.dir.clone());
        relief
    }

    /// Publishes the status file and takes resume requests in `dir`: a test's
    /// seam for what [`Relief::os`] does for the app.
    #[cfg(test)]
    pub(crate) fn with_status_dir(mut self, dir: PathBuf) -> Self {
        self.status_dir = Some(dir);
        self
    }

    /// Reads this relief's process table from `table` instead of the scan the
    /// sampler hands in: a test's seam.
    #[cfg(test)]
    pub(crate) fn with_table(
        mut self,
        table: std::sync::Arc<dyn Fn() -> ReliefTable + Send + Sync>,
    ) -> Self {
        self.table_override = Some(table);
        self
    }

    #[cfg(test)]
    pub(crate) fn watch(&self) -> std::sync::Arc<Watch> {
        self.watch.clone()
    }

    /// This relief's state is what [`snapshot`] serves.
    pub(crate) fn publishing(mut self) -> Self {
        self.publishing = true;
        self
    }

    /// Puts back what a previous GitPulse left (a crash, a kill): called once,
    /// on the tick this sampler wins the writer election.
    pub(crate) fn recover(&mut self) {
        let first = !self.recovered;
        self.recover_locked();
        if first {
            // An at-rest file at startup, so a reader never mistakes the
            // previous run's last claim for this one's.
            self.publish_file();
        }
    }

    fn recover_locked(&mut self) {
        if self.recovered {
            return;
        }
        self.recovered = true;
        let _act = lock(&ACT);
        // A journal another running GitPulse wrote is that one's: its
        // watchdog continues what it stopped, and nothing here guesses at its
        // entries by age (that could continue a process somebody else stopped).
        let replay = replay_journal(&self.dir, &self.ns, &*self.actuator);
        log_replay("recovery", &replay);
        if replay.resumed > 0 || replay.restored > 0 {
            self.note(
                ActionKind::Resume,
                format!(
                    "resumed {} stopped and restored {} slowed processes a previous GitPulse left",
                    replay.resumed, replay.restored
                ),
            );
        }
        for failure in replay.failures {
            self.fail(None, failure);
        }
        self.publish();
    }

    /// Takes account of what the watchdog continued from its own copy while
    /// this sampler was stalled: those subtrees are running, so they leave
    /// `frozen`, the journal and the snapshot, and rung B holds off as after a
    /// cap (a whole new Severe run). Without this the relief would go on
    /// stopping the late children of a build that is running.
    fn reconcile_watchdog(&mut self) {
        let released: HashSet<(i32, u64)> = self.watch.take_released().into_iter().collect();
        if released.is_empty() {
            return;
        }
        let (hit, kept): (Vec<Frozen>, Vec<Frozen>) = std::mem::take(&mut self.frozen)
            .into_iter()
            .partition(|f| !f.info.advised_only && f.members.iter().any(|m| released.contains(m)));
        self.frozen = kept;
        if hit.is_empty() {
            return;
        }
        for f in &hit {
            // Whatever of it the watchdog did not reach is still ours to continue.
            let rest: Vec<(i32, u64)> = f
                .members
                .iter()
                .copied()
                .filter(|m| !released.contains(m))
                .collect();
            self.continue_all(&rest);
            let ids: HashSet<i32> = f.members.iter().map(|(pid, _)| *pid).collect();
            self.journal
                .retain(|e| e.rung != JournalRung::Freeze || !ids.contains(&e.pid));
            self.note(
                ActionKind::Resume,
                format!(
                    "resumed {} in session {}: {}",
                    f.info.command,
                    f.info.session_id,
                    ResumeReason::Watchdog.words()
                ),
            );
        }
        if let Err(why) = self.sync_journal() {
            self.fail(None, format!("the journal could not be trimmed: {why}"));
        }
        self.watch_frozen();
        if self.frozen.is_empty() {
            self.trigger.ended();
        }
    }

    /// Takes account of a [`resume_all`] made from another thread: it already
    /// continued the journaled processes, so this only forgets them.
    fn reconcile_external(&mut self) {
        let external = external_of(&self.dir);
        if external.generation == self.seen_external {
            return;
        }
        self.seen_external = external.generation;
        let covered = |since_ms: u64| external.up_to_ms.is_none_or(|up_to| since_ms <= up_to);
        // Exactly the pauses the request covered were continued (their journal
        // entries carry the pause's own start); the others stay frozen.
        let (resumed, kept): (Vec<Frozen>, Vec<Frozen>) = std::mem::take(&mut self.frozen)
            .into_iter()
            .partition(|f| !f.info.advised_only && covered(f.info.since_unix_ms));
        self.frozen = kept;
        let had = resumed.len();
        // An advised pause is nothing stopped: a person's resume clears it too.
        if external.up_to_ms.is_none() {
            self.frozen.retain(|f| !f.info.advised_only);
        }
        self.watch_frozen();
        self.journal
            .retain(|e| e.rung != JournalRung::Freeze || !covered(e.at_unix_ms));
        if self.frozen.is_empty() {
            self.trigger.ended();
        }
        if external.hold_off {
            self.trigger.hold_off();
        } else {
            // A person who resumed without holding off decided the next
            // Severe minute may freeze again, the same builds included.
            self.spent.clear();
        }
        self.note(
            ActionKind::Resume,
            format!(
                "resumed {had} frozen subtree(s) on request: {}",
                external.reason
            ),
        );
    }

    /// One sample: a person's pending resume request first, then the tick
    /// itself under the action lock, then the published status file once that
    /// lock is released (the write is synced to disk, which must never happen
    /// under [`ACT`] or the ledger). See [`Relief::tick_locked`].
    pub(crate) fn tick(
        &mut self,
        now: Instant,
        input: &GovernorInput,
        refuses: bool,
        table: &dyn Fn() -> ReliefTable,
    ) {
        self.take_resume_request();
        self.tick_locked(now, input, refuses, table);
        self.publish_file();
    }

    /// Resumes what a banner or Health "Resume now" asked for
    /// (`relief_status::take_resume_request`): the pauses that began at or
    /// before the request, through the journal like every resume, holding off
    /// a new freeze this episode. The tick that follows takes account of it and
    /// records it as the last action.
    fn take_resume_request(&mut self) {
        let Some(dir) = self.status_dir.clone() else {
            return;
        };
        if STOPPING.load(Ordering::SeqCst) {
            return;
        }
        let now_ms = super::relief_status::now_ms();
        // A request that found the lock busy is still the first in line; the
        // file it came from is gone, so it is held here, for the contract's
        // time to live and no longer.
        let request = match self.pending_request.take() {
            Some(held)
                if now_ms.saturating_sub(held.at_ms) <= super::relief_status::REQUEST_TTL_MS =>
            {
                held
            }
            Some(_) => {
                self.fail(
                    None,
                    "a resume request was not carried out: the action lock stayed busy until it expired"
                        .into(),
                );
                return;
            }
            None => match super::relief_status::take_resume_request(&dir, now_ms) {
                Some(request) => request,
                None => return,
            },
        };
        let outcome = resume_up_to_within(
            &self.dir,
            &*self.actuator,
            true,
            &request.reason,
            Some(request.at_ms),
            self.act_wait,
        );
        if outcome.busy {
            self.pending_request = Some(request);
            return;
        }
        if let Some(why) = outcome.refused {
            self.fail(None, format!("a resume request was not carried out: {why}"));
        }
        for result in outcome.results.into_iter().filter(|r| !r.ok) {
            self.fail(
                Some(result.pid),
                format!(
                    "could not resume pid {} on request: {}",
                    result.pid,
                    result.detail.unwrap_or_default()
                ),
            );
        }
    }

    /// The status as the published file carries it.
    fn file_status(&self) -> super::relief_status::ReliefStatus {
        use super::relief_status as file;
        let snapshot = &self.status;
        let pid = std::process::id();
        let act = snapshot.mode == SupervisorMode::Act;
        let rung = match snapshot.rung {
            Rung::None => file::ReliefRung::None,
            Rung::Slow => file::ReliefRung::Slow,
            Rung::Freeze => file::ReliefRung::Freeze,
        };
        file::ReliefStatus {
            version: file::VERSION,
            written_at_ms: file::now_ms(),
            writer_pid: pid,
            writer_start_us: super::process_start_us(pid).unwrap_or(0),
            mode: match snapshot.mode {
                SupervisorMode::Off => file::ReliefMode::Off,
                SupervisorMode::Advise => file::ReliefMode::Advise,
                SupervisorMode::Act => file::ReliefMode::Act,
            },
            rung,
            // The binding says why a rung is in force; at rest there is none.
            binding: (rung != file::ReliefRung::None)
                .then_some(snapshot.binding)
                .flatten()
                .and_then(|binding| match binding {
                    Binding::Swap => Some(file::ReliefBinding::Swap),
                    Binding::CompressorSegments => Some(file::ReliefBinding::CompressorSegments),
                    Binding::CompressorThrash => Some(file::ReliefBinding::CompressorThrash),
                    Binding::MemoryPressure => Some(file::ReliefBinding::MemoryPressure),
                    _ => None,
                }),
            // Only what was actually slowed, and only what Act would have.
            slowed: if act { snapshot.slowed } else { 0 },
            would_slow: if snapshot.mode == SupervisorMode::Advise {
                snapshot.would_slow
            } else {
                0
            },
            frozen: snapshot
                .frozen
                .iter()
                .map(|f| file::FrozenTree {
                    pid: u32::try_from(f.root_pid).unwrap_or(0),
                    start_us: f.start_us,
                    agent_root_pid: u32::try_from(f.session_pid).unwrap_or(0),
                    terminal_session_id: f.terminal_session_id.clone(),
                    repo_path: f.repo_path.clone().unwrap_or_default(),
                    command: f.command.clone(),
                    footprint_bytes: f.footprint_bytes,
                    since_ms: f.since_unix_ms,
                    advised_only: f.advised_only,
                })
                .collect(),
            last_action: snapshot.last_action.as_ref().map(|a| file::ReliefAction {
                at_ms: a.at_unix_ms,
                kind: match a.kind {
                    ActionKind::Slow => file::ReliefActionKind::Slow,
                    ActionKind::Freeze => file::ReliefActionKind::Freeze,
                    ActionKind::Resume => file::ReliefActionKind::Resume,
                    ActionKind::CapResume => file::ReliefActionKind::CapResume,
                    ActionKind::Refused => file::ReliefActionKind::Refused,
                },
                detail: a.detail.clone(),
            }),
            failures: snapshot
                .failures
                .iter()
                .map(|f| file::ReliefFailure {
                    at_ms: f.at_unix_ms,
                    pid: f.pid.and_then(|p| u32::try_from(p).ok()).unwrap_or(0),
                    what: f.what.clone(),
                })
                .collect(),
            cap_hits: u32::try_from(snapshot.cap_hits).unwrap_or(u32::MAX),
        }
    }

    /// Writes the status file: every tick while it claims activity, and once
    /// on each change (including the one back to rest, and the first). Called
    /// with no lock held.
    fn publish_file(&mut self) {
        let Some(dir) = self.status_dir.clone() else {
            return;
        };
        if STOPPING.load(Ordering::SeqCst) {
            return;
        }
        let status = self.file_status();
        let mut key = status.clone();
        key.written_at_ms = 0;
        if !status.claims_activity() && self.last_written.as_ref() == Some(&key) {
            return;
        }
        match super::relief_status::write_status(&dir, &status) {
            Ok(()) => {
                self.last_written = Some(key);
                self.status_write_failed = false;
            }
            Err(why) => {
                if !self.status_write_failed {
                    log::warn!(target: "relief", "the status file could not be written: {why}");
                }
                self.status_write_failed = true;
            }
        }
    }

    /// One sample. `table` reads the sessions of the scan this tick just made;
    /// it is called only when something needs the processes.
    ///
    /// `refuses` is whether the build-lease board is refusing new builds under
    /// the governor's verdict (`lease::capacity(..).refuses`): while it is, a
    /// build that holds a slot may be frozen, and the first tick it is not,
    /// those are continued.
    fn tick_locked(
        &mut self,
        now: Instant,
        input: &GovernorInput,
        refuses: bool,
        table: &dyn Fn() -> ReliefTable,
    ) {
        if STOPPING.load(Ordering::SeqCst) {
            return;
        }
        // The whole tick runs under the action lock, taken before anything is
        // read: a `resume_all` from another thread is then either wholly
        // before this tick (its generation is read below, under the lock) or
        // wholly after it, never between the read and the action it changes.
        #[cfg(test)]
        if let Some(hook) = self.before_act.clone() {
            hook();
        }
        let _act = lock(&ACT);
        if STOPPING.load(Ordering::SeqCst) {
            return;
        }
        #[cfg(test)]
        let over = self.table_override.clone();
        #[cfg(test)]
        let table: &dyn Fn() -> ReliefTable = &|| over.as_ref().map_or_else(table, |read| read());
        self.watch.beat(now);
        self.refusing = refuses;
        if !self.watchdog_reported {
            if let Some(why) = self.watch.start_error() {
                self.watchdog_reported = true;
                self.fail(
                    None,
                    format!("the watchdog thread did not start, so nothing continues a freeze if this sampler stalls: {why}"),
                );
            }
        }
        // The watchdog's work first, before anything rebuilds its copy from
        // this relief's own record of what is frozen.
        self.reconcile_watchdog();
        self.reconcile_external();
        let mode = (self.mode)();
        self.status.mode = mode;
        if mode == SupervisorMode::Off || !input.enabled {
            self.stand_down(ResumeReason::Off, table);
            self.status.binding = None;
            self.status.tier = None;
            self.publish();
            return;
        }
        let sample = Sample::of(input);
        self.status.tier = sample.tier.map(|(tier, _)| tier);
        self.status.binding = sample.tier.and_then(|(_, binding)| binding);
        let verdict = self.trigger.observe(now, &sample);
        if self.trigger.episode() != self.episode_seen {
            // The Severe episode is over: what it froze may be frozen again.
            self.episode_seen = self.trigger.episode();
            self.spent.clear();
        }
        let act = mode == SupervisorMode::Act;

        // An advised freeze left `act`, or the setting moved to `advise` with
        // real freezes standing: the real ones end now.
        if !act && self.frozen.iter().any(|f| !f.info.advised_only) {
            self.resume_frozen(ResumeReason::Off);
        }
        if act && self.frozen.iter().any(|f| f.info.advised_only) {
            self.frozen.retain(|f| !f.info.advised_only);
            self.trigger.ended();
        }

        // Rung A. The first pass of a slowing runs at once; the rest every
        // [`SLOW_PASS_EVERY`] samples (a child of a slowed process inherits the
        // background priority in the meantime), so the tick does not write a
        // journal on every sample of a long pressure episode.
        if verdict.slow {
            let due = self
                .slow_since_pass
                .is_none_or(|since| since + 1 >= SLOW_PASS_EVERY);
            self.slow_since_pass = Some(if due {
                0
            } else {
                self.slow_since_pass.map_or(0, |since| since + 1)
            });
            if due {
                let scan = table();
                if act {
                    self.slow(&scan);
                } else {
                    self.status.would_slow = Self::eligible_for_slowing(&scan);
                }
            }
        } else {
            self.slow_since_pass = None;
            self.status.would_slow = 0;
            self.foreign.clear();
        }
        // What is in the background goes back as soon as the slowing is over
        // or the setting left `act`.
        if (!verdict.slow || !act) && !self.slowed.is_empty() {
            self.restore_slowed(table);
        }

        // Rung B. A build that held a lease slot was frozen because the board
        // was refusing new builds; the first tick it is not, it goes on,
        // without waiting for memory to flatten.
        if !refuses && self.frozen.iter().any(|f| f.holder && !f.info.advised_only) {
            self.resume_holders();
        }
        match verdict.freeze {
            Step::Begin => {
                let scan = table();
                self.begin_freeze(now, &scan, act);
            }
            Step::Resume(reason) => {
                self.resume_frozen(reason);
            }
            Step::Hold => {
                if act && self.frozen.iter().any(|f| !f.info.advised_only) {
                    let scan = table();
                    self.maintain_frozen(&scan);
                }
            }
        }
        self.publish();
    }

    /// Ends everything in force: the setting went off, or measurement did.
    fn stand_down(&mut self, reason: ResumeReason, table: &dyn Fn() -> ReliefTable) {
        if !self.frozen.is_empty() {
            self.resume_frozen(reason);
        }
        if !self.slowed.is_empty() {
            self.restore_slowed(table);
        }
        self.foreign.clear();
        self.status.would_slow = 0;
        self.trigger.reset();
    }

    // -- bookkeeping ------------------------------------------------------

    fn note(&mut self, kind: ActionKind, detail: String) {
        self.status.last_action = Some(LastAction {
            at_unix_ms: unix_ms(),
            kind,
            detail,
        });
    }

    fn fail(&mut self, pid: Option<i32>, what: String) {
        log::warn!(target: "relief", "{what}");
        self.status.failure_count = self.status.failure_count.saturating_add(1);
        if self
            .status
            .failures
            .last()
            .is_some_and(|last| last.what == what && last.pid == pid)
        {
            return;
        }
        self.status.failures.push(SignalFailure {
            at_unix_ms: unix_ms(),
            pid,
            what,
        });
        let excess = self.status.failures.len().saturating_sub(MAX_FAILURES);
        self.status.failures.drain(..excess);
    }

    fn sync_journal(&self) -> Result<(), String> {
        write_journal(&self.dir, &self.journal)
    }

    fn entry(&self, pid: i32, start_us: u64, session: &str, rung: JournalRung) -> JournalEntry {
        JournalEntry {
            pid,
            start_us,
            session_id: session.to_string(),
            at_unix_ms: unix_ms(),
            rung,
            // Callers write a freeze entry only for a process they read as
            // running just before; a slow entry has no such question.
            was_running: true,
        }
    }

    /// Makes the state current for [`snapshot`]. Memory only: the tick writes
    /// no status file.
    fn publish(&mut self) {
        self.status.slowed = u32::try_from(self.slowed.len()).unwrap_or(u32::MAX);
        self.status.frozen = self.frozen.iter().map(|f| f.info.clone()).collect();
        self.status.rung = if !self.frozen.is_empty() {
            Rung::Freeze
        } else if self.slowed.is_empty() && self.status.would_slow == 0 && !self.trigger.slowing() {
            Rung::None
        } else {
            Rung::Slow
        };
        self.status.updated_unix_ms = unix_ms();
        if self.publishing {
            *lock(&SNAPSHOT) = Some(self.status.clone());
        }
    }

    // -- rung A -----------------------------------------------------------

    fn eligible_for_slowing(table: &ReliefTable) -> u32 {
        let n: usize = table
            .sessions
            .iter()
            .map(|s| s.members.iter().filter(|m| !m.exempt).count())
            .sum();
        u32::try_from(n).unwrap_or(u32::MAX)
    }

    /// Lets go of what has left the scanned trees: a process that is no longer
    /// in the table (by pid and start time) leaves `slowed`, `foreign`, the
    /// ledger and the journal, so the cap counts live processes and not every
    /// one a long build made.
    ///
    /// A process that left the trees is not necessarily gone: when its agent
    /// ended it became launchd's child and kept running, in the background.
    /// Forgetting it unrestored would be forgetting the one thing this relief
    /// promised to put back, so it is restored first (the actuator checks the
    /// start time, so a recycled pid is refused, not touched); only a process
    /// that is gone is simply dropped. Returns whether the journal changed.
    fn prune(&mut self, table: &ReliefTable) -> bool {
        let live: HashSet<(i32, u64)> = table
            .sessions
            .iter()
            .flat_map(|s| s.members.iter().map(|m| (m.pid, m.start_us)))
            .collect();
        let gone: Vec<(i32, u64)> = self
            .slowed
            .iter()
            .map(|(&pid, &start_us)| (pid, start_us))
            .filter(|key| !live.contains(key))
            .collect();
        if !gone.is_empty() {
            let stewards = ledger_stewards(&lock(&LEDGER), &self.ns);
            for &(pid, start_us) in &gone {
                self.slowed.remove(&pid);
                self.slowed_mark.remove(&pid);
                if !stewards.contains(&(pid, start_us)) {
                    match self.actuator.set_background(pid, start_us, false) {
                        Ok(_) => {}
                        Err(why) if process_is_gone(&why) => {}
                        Err(why) => self.fail(
                            Some(pid),
                            format!(
                                "could not put process {pid} back to normal priority after it left the agent's tree: {why}"
                            ),
                        ),
                    }
                }
                ledger_clear(&mut lock(&LEDGER), &self.ns, pid, start_us);
            }
        }
        self.foreign.retain(|key| live.contains(key));
        let before = self.journal.len();
        self.journal
            .retain(|e| e.rung != JournalRung::Slow || self.slowed.contains_key(&e.pid));
        self.journal.len() != before
    }

    fn slow(&mut self, table: &ReliefTable) {
        let pruned = self.prune(table);
        // Parents before children, so a child that inherited the background
        // priority is recognised as ours rather than as someone else's.
        let mut fresh: Vec<(usize, &ReliefProc, String)> = Vec::new();
        // What the steward slowed is its own to put back.
        let stewards = ledger_stewards(&lock(&LEDGER), &self.ns);
        for session in &table.sessions {
            let depth_of = depths(&session.members, session.root.pid);
            let session_id = format!("{}-{}", session.root.pid, session.root.start_us);
            for m in session.members.iter().filter(|m| !m.exempt) {
                if self.slowed.contains_key(&m.pid)
                    || self.foreign.contains(&(m.pid, m.start_us))
                    || stewards.contains(&(m.pid, m.start_us))
                {
                    continue;
                }
                fresh.push((
                    depth_of.get(&m.pid).copied().unwrap_or(0),
                    m,
                    session_id.clone(),
                ));
            }
        }
        fresh.sort_by_key(|(depth, m, _)| (*depth, m.pid));
        // Bounded per pass as well as in all: each is a few syscalls on the
        // sampler's tick. The rest follow on the next pass.
        fresh.truncate(
            MAX_SLOWED
                .saturating_sub(self.slowed.len())
                .min(MAX_SLOW_PER_PASS),
        );
        if fresh.is_empty() {
            if pruned {
                if let Err(why) = self.sync_journal() {
                    self.fail(None, format!("the journal could not be trimmed: {why}"));
                }
            }
            return;
        }
        let before = self.journal.len();
        for (_, m, session) in &fresh {
            let entry = self.entry(m.pid, m.start_us, session, JournalRung::Slow);
            self.journal.push(entry);
        }
        if let Err(why) = self.sync_journal() {
            self.journal.truncate(before);
            self.fail(
                None,
                format!("nothing was slowed: the journal could not be written: {why}"),
            );
            return;
        }
        let mut refused: BTreeMap<String, u32> = BTreeMap::new();
        let mut taken = 0u32;
        let mut drop_entries: HashSet<i32> = HashSet::new();
        // The syscalls first, with no lock held: another thread (the steward)
        // takes the ledger for one process at a time and must not wait behind
        // a pass over hundreds. The answers are recorded after.
        let answers: Vec<Result<bool, String>> = fresh
            .iter()
            .map(|(_, m, _)| self.actuator.set_background(m.pid, m.start_us, true))
            .collect();
        // The newest start in this scan: whatever starts after it is born of
        // the slowing, not part of what was there.
        let mark = table
            .sessions
            .iter()
            .flat_map(|s| s.members.iter().map(|m| m.start_us))
            .max()
            .unwrap_or(0);
        let mut ledger = lock(&LEDGER);
        for ((_, m, _), answer) in fresh.iter().zip(answers) {
            match answer {
                Ok(true) => {
                    self.slowed.insert(m.pid, m.start_us);
                    self.slowed_mark.insert(m.pid, mark);
                    ledger_put(&mut ledger, &self.ns, m.pid, m.start_us, SlowOwner::Relief);
                    taken += 1;
                }
                Ok(false) => {
                    if self.slowed.contains_key(&m.ppid) {
                        // Inherited from a parent this relief slowed.
                        self.slowed.insert(m.pid, m.start_us);
                        self.slowed_mark.insert(m.pid, mark);
                        ledger_put(&mut ledger, &self.ns, m.pid, m.start_us, SlowOwner::Relief);
                    } else {
                        self.foreign.insert((m.pid, m.start_us));
                        drop_entries.insert(m.pid);
                    }
                }
                Err(why) => {
                    drop_entries.insert(m.pid);
                    // Refused once is refused until the slowing ends or the
                    // process does; it is not asked again every pass.
                    self.foreign.insert((m.pid, m.start_us));
                    if !process_is_gone(&why) {
                        *refused.entry(why).or_default() += 1;
                    }
                }
            }
        }
        drop(ledger);
        if !drop_entries.is_empty() {
            self.journal.retain(|e| {
                e.rung != JournalRung::Slow
                    || !drop_entries.contains(&e.pid)
                    || self.slowed.contains_key(&e.pid)
            });
            if let Err(why) = self.sync_journal() {
                self.fail(None, format!("the journal could not be trimmed: {why}"));
            }
        }
        for (why, count) in refused {
            self.fail(None, format!("could not slow {count} process(es): {why}"));
        }
        if taken > 0 {
            self.note(
                ActionKind::Slow,
                format!("put {taken} agent process(es) in the background under memory pressure"),
            );
        }
    }

    /// What would stay in the background after its parent is put back: the
    /// descendants of `slowed` (in this scan) that were never slowed by name,
    /// because they started after the last pass and inherited the priority,
    /// and that read back as in the background now. What the steward slowed is
    /// its own and is not listed.
    fn inheritors(
        &self,
        slowed: &BTreeMap<i32, u64>,
        stewards: &HashSet<(i32, u64)>,
        table: &dyn Fn() -> ReliefTable,
    ) -> Vec<(i32, u64)> {
        if slowed.is_empty() {
            return Vec::new();
        }
        let scan = table();
        let mut found = Vec::new();
        for session in &scan.sessions {
            // Each process under a slowed one, with the scan mark a descendant
            // must have started after (no mark, no inheritor).
            let mut under: HashMap<i32, u64> = session
                .members
                .iter()
                .filter(|m| slowed.get(&m.pid) == Some(&m.start_us))
                .map(|m| {
                    (
                        m.pid,
                        self.slowed_mark.get(&m.pid).copied().unwrap_or(u64::MAX),
                    )
                })
                .collect();
            // Members come in table order, not tree order: sweep until none
            // joins. What the steward owns does not carry the walk on: its
            // children inherited *its* priority, which is its to put back.
            for _ in 0..MAX_TREE_DEPTH {
                let before = under.len();
                for m in &session.members {
                    if under.contains_key(&m.pid) || stewards.contains(&(m.pid, m.start_us)) {
                        continue;
                    }
                    if let Some(&mark) = under.get(&m.ppid) {
                        under.insert(m.pid, mark);
                    }
                }
                if under.len() == before {
                    break;
                }
            }
            for m in &session.members {
                let key = (m.pid, m.start_us);
                // Started after the slowing pass read the table: born of it.
                // One that was there already and is in the background was put
                // there by something else, and stays.
                if under.get(&m.pid).is_some_and(|&mark| m.start_us > mark)
                    && slowed.get(&m.pid) != Some(&m.start_us)
                    && !stewards.contains(&key)
                    && self.actuator.is_background(m.pid, m.start_us) == Some(true)
                {
                    found.push(key);
                }
            }
        }
        found.truncate(MAX_SLOWED);
        found
    }

    fn restore_slowed(&mut self, table: &dyn Fn() -> ReliefTable) {
        let slowed = std::mem::take(&mut self.slowed);
        // The marks are read by `inheritors` below, then forgotten with them.
        let mut restored = 0u32;
        let mut refused: BTreeMap<String, u32> = BTreeMap::new();
        let stewards = ledger_stewards(&lock(&LEDGER), &self.ns);
        let inherited = self.inheritors(&slowed, &stewards, table);
        self.slowed_mark.clear();
        for &(pid, start_us) in &inherited {
            match self.actuator.set_background(pid, start_us, false) {
                Ok(true) => restored += 1,
                Ok(false) => {}
                Err(why) if process_is_gone(&why) => {}
                Err(why) => *refused.entry(why).or_default() += 1,
            }
        }
        // No lock across the syscalls; the ledger is cleared after.
        for (&pid, &start_us) in &slowed {
            if stewards.contains(&(pid, start_us)) {
                continue;
            }
            match self.actuator.set_background(pid, start_us, false) {
                Ok(true) => restored += 1,
                Ok(false) => {}
                Err(why) if process_is_gone(&why) => {}
                Err(why) => *refused.entry(why).or_default() += 1,
            }
        }
        {
            let mut ledger = lock(&LEDGER);
            for (&pid, &start_us) in &slowed {
                if !stewards.contains(&(pid, start_us)) {
                    ledger_clear(&mut ledger, &self.ns, pid, start_us);
                }
            }
        }
        // Cleared after the restore was attempted, failures included.
        self.journal.retain(|e| e.rung != JournalRung::Slow);
        if let Err(why) = self.sync_journal() {
            self.fail(None, format!("the journal could not be trimmed: {why}"));
        }
        for (why, count) in refused {
            self.fail(
                None,
                format!("could not restore {count} process(es): {why}"),
            );
        }
        if restored > 0 {
            self.note(
                ActionKind::Resume,
                format!("put {restored} agent process(es) back to normal priority"),
            );
        }
    }

    // -- rung B -----------------------------------------------------------

    fn begin_freeze(&mut self, now: Instant, table: &ReliefTable, act: bool) {
        let busy: BTreeSet<(i32, u64)> = self.frozen.iter().map(|f| f.session).collect();
        let room = MAX_FROZEN_PER_HOST.saturating_sub(self.frozen.len());
        let mut notes = Vec::new();
        let spent = self.spent.clone();
        let want = Want {
            busy: &busy,
            room,
            refusing: self.refusing,
            spent: &spent,
        };
        let picks = match select(
            table,
            &*self.probe,
            &*self.trusted,
            &want,
            &mut self.marks,
            &mut notes,
        ) {
            Ok(picks) => picks,
            Err(why) => {
                self.fail(None, format!("nothing was frozen: {why}"));
                self.trigger.began(now, false);
                return;
            }
        };
        if picks.is_empty() {
            let why = notes
                .first()
                .cloned()
                .unwrap_or_else(|| "no subtree qualified".into());
            self.note(
                ActionKind::Refused,
                format!("memory is failing but nothing could be frozen ({why})"),
            );
            self.trigger.began(now, false);
            return;
        }
        let mut any = false;
        for pick in picks {
            if act {
                any |= self.freeze(&pick, now);
            } else {
                self.frozen.push(Frozen {
                    info: frozen_info(&pick, true),
                    session: (pick.session_pid, pick.session_start_us),
                    root: (pick.root.pid, pick.root.start_us),
                    members: Vec::new(),
                    since: now,
                    holder: pick.holder,
                    left: BTreeSet::new(),
                });
                self.spent
                    .extend(pick.members.iter().map(|m| (m.pid, m.start_us)));
                self.note(
                    ActionKind::Freeze,
                    format!(
                        "would freeze {} ({} process(es)) in session {}",
                        pick.root.name,
                        pick.members.len(),
                        pick.session_pid
                    ),
                );
                any = true;
            }
        }
        self.trigger.began(now, any);
    }

    /// Stops one subtree. Each member's state is read first: one that is
    /// already stopped (a Ctrl-Z job, a debugger) is somebody else's, so it is
    /// left alone: not journaled, not stopped, never continued. The journal
    /// (each entry `was_running`) is written before the first signal, every
    /// process is read again immediately before its own stop, and a stop that
    /// was delivered goes into the watchdog's copy at once, so the watchdog
    /// only ever continues what this relief really stopped.
    fn freeze(&mut self, pick: &Selection, now: Instant) -> bool {
        let session = format!("{}-{}", pick.session_pid, pick.session_start_us);
        let mut left: BTreeSet<(i32, u64)> = BTreeSet::new();
        let mut running: Vec<&ReliefProc> = Vec::new();
        for (n, m) in pick.members.iter().enumerate() {
            match self.actuator.is_stopped(m.pid, m.start_us) {
                Ok(false) => running.push(m),
                Ok(true) => {
                    left.insert((m.pid, m.start_us));
                    if n == 0 {
                        self.note(
                            ActionKind::Refused,
                            format!(
                                "{} was not frozen: something else had stopped it already",
                                m.name
                            ),
                        );
                        return false;
                    }
                }
                Err(why) if has_ended(&why) => {
                    if n == 0 {
                        return false;
                    }
                }
                Err(why) => {
                    self.fail(
                        Some(m.pid),
                        format!(
                            "{} (pid {}) was not stopped: its state could not be read: {why}",
                            m.name, m.pid
                        ),
                    );
                    if n == 0 {
                        return false;
                    }
                }
            }
        }
        let before = self.journal.len();
        // One stamp for the whole pause: the journal's entries and the status's
        // `since_ms` agree on when it began, so a resume request covers it
        // whole or not at all.
        let since_ms = unix_ms();
        for m in &running {
            let entry = JournalEntry {
                at_unix_ms: since_ms,
                ..self.entry(m.pid, m.start_us, &session, JournalRung::Freeze)
            };
            self.journal.push(entry);
        }
        if let Err(why) = self.sync_journal() {
            self.journal.truncate(before);
            self.fail(
                None,
                format!(
                    "{} was not frozen: the journal could not be written: {why}",
                    pick.root.name
                ),
            );
            return false;
        }
        let mut stopped: Vec<(i32, u64)> = Vec::new();
        for (n, m) in running.iter().enumerate() {
            // GitPulse is quitting: stop nothing more, and undo what this
            // freeze stopped (the teardown may already have continued the
            // rest, and would not know of these).
            if STOPPING.load(Ordering::SeqCst) {
                self.continue_all(&stopped);
                stopped.clear();
                break;
            }
            // Read again just before the signal: a person may have stopped it
            // since the first read, and then it is not ours.
            match self.actuator.is_stopped(m.pid, m.start_us) {
                Ok(false) => {}
                Ok(true) => {
                    left.insert((m.pid, m.start_us));
                    if n == 0 {
                        break;
                    }
                    continue;
                }
                Err(why) => {
                    if !has_ended(&why) {
                        self.fail(
                            Some(m.pid),
                            format!(
                                "{} (pid {}) was not stopped: its state could not be read: {why}",
                                m.name, m.pid
                            ),
                        );
                    }
                    if n == 0 {
                        break;
                    }
                    continue;
                }
            }
            match self.actuator.signal(m.pid, m.start_us, Signal::Stop) {
                Ok(Delivery::Delivered) => {
                    stopped.push((m.pid, m.start_us));
                    self.watch.add(Watched {
                        pid: m.pid,
                        start_us: m.start_us,
                        since: now,
                    });
                }
                Ok(Delivery::Gone) => {}
                Err(why) => {
                    self.fail(
                        Some(m.pid),
                        format!("could not stop {} (pid {}): {why}", m.name, m.pid),
                    );
                    if n == 0 {
                        // The subtree's root stays running: stopping its
                        // children alone would hang them against a parent
                        // that is not stopped. Undo.
                        break;
                    }
                }
            }
        }
        let root_stopped = stopped.first() == Some(&(pick.root.pid, pick.root.start_us));
        if !root_stopped {
            self.continue_all(&stopped);
            self.journal.truncate(before);
            if let Err(why) = self.sync_journal() {
                self.fail(None, format!("the journal could not be trimmed: {why}"));
            }
            self.watch_frozen();
            return false;
        }
        // Journal only what was stopped.
        let stopped_set: HashSet<i32> = stopped.iter().map(|(pid, _)| *pid).collect();
        let member_ids: HashSet<i32> = pick.members.iter().map(|m| m.pid).collect();
        self.journal.retain(|e| {
            e.rung != JournalRung::Freeze
                || !member_ids.contains(&e.pid)
                || stopped_set.contains(&e.pid)
        });
        if let Err(why) = self.sync_journal() {
            self.fail(None, format!("the journal could not be trimmed: {why}"));
        }
        let mut info = frozen_info(pick, false);
        info.since_unix_ms = since_ms;
        self.note(
            ActionKind::Freeze,
            format!(
                "froze {} ({} process(es), {}) in session {}",
                pick.root.name,
                stopped.len(),
                human_bytes(pick.footprint),
                session
            ),
        );
        if !left.is_empty() {
            self.note(
                ActionKind::Freeze,
                format!(
                    "froze {} ({} process(es), {}) in session {}; {} already stopped by something else was left alone",
                    pick.root.name,
                    stopped.len(),
                    human_bytes(pick.footprint),
                    session,
                    left.len()
                ),
            );
        }
        self.spent
            .extend(pick.members.iter().map(|m| (m.pid, m.start_us)));
        self.frozen.push(Frozen {
            info,
            session: (pick.session_pid, pick.session_start_us),
            root: (pick.root.pid, pick.root.start_us),
            members: stopped,
            since: now,
            holder: pick.holder,
            left,
        });
        self.watch_frozen();
        true
    }

    /// The watchdog's copy is exactly what this relief has frozen: the
    /// processes whose stop was delivered, each counted from its subtree's
    /// own start.
    fn watch_frozen(&self) {
        let entries: Vec<Watched> = self
            .frozen
            .iter()
            .filter(|f| !f.info.advised_only)
            .flat_map(|f| {
                let since = f.since;
                f.members.iter().map(move |&(pid, start_us)| Watched {
                    pid,
                    start_us,
                    since,
                })
            })
            .collect();
        self.watch.set(entries);
    }

    /// `SIGCONT`s each, last first; failures are recorded.
    fn continue_all(&mut self, members: &[(i32, u64)]) {
        for &(pid, start_us) in members.iter().rev() {
            if let Err(why) = self.actuator.signal(pid, start_us, Signal::Cont) {
                self.fail(Some(pid), format!("could not resume pid {pid}: {why}"));
            }
        }
    }

    /// Resumes every frozen subtree. The journal entries go only after each
    /// resume was attempted.
    fn resume_frozen(&mut self, reason: ResumeReason) {
        let frozen = std::mem::take(&mut self.frozen);
        for f in &frozen {
            if f.info.advised_only {
                continue;
            }
            self.continue_all(&f.members);
            let ids: HashSet<i32> = f.members.iter().map(|(pid, _)| *pid).collect();
            self.journal
                .retain(|e| e.rung != JournalRung::Freeze || !ids.contains(&e.pid));
            let kind = if reason == ResumeReason::Cap {
                ActionKind::CapResume
            } else {
                ActionKind::Resume
            };
            self.note(
                kind,
                format!(
                    "resumed {} in session {}: {}",
                    f.info.command,
                    f.info.session_id,
                    reason.words()
                ),
            );
        }
        if let Err(why) = self.sync_journal() {
            self.fail(None, format!("the journal could not be trimmed: {why}"));
        }
        if reason == ResumeReason::Cap && !frozen.is_empty() {
            self.status.cap_hits = self.status.cap_hits.saturating_add(1);
            self.note(
                ActionKind::CapResume,
                "the 10-minute freeze limit was reached: resumed, and not frozen again until memory fails afresh"
                    .into(),
            );
        }
        self.watch_frozen();
        self.trigger.ended();
    }

    /// Continues the frozen builds that held a lease slot, because the board
    /// stopped refusing. Everything else stays frozen on its own conditions.
    fn resume_holders(&mut self) {
        let (holders, rest): (Vec<Frozen>, Vec<Frozen>) = std::mem::take(&mut self.frozen)
            .into_iter()
            .partition(|f| f.holder && !f.info.advised_only);
        self.frozen = rest;
        for f in &holders {
            self.continue_all(&f.members);
            let ids: HashSet<i32> = f.members.iter().map(|(pid, _)| *pid).collect();
            self.journal
                .retain(|e| e.rung != JournalRung::Freeze || !ids.contains(&e.pid));
            self.note(
                ActionKind::Resume,
                format!(
                    "resumed {} in session {}: {}",
                    f.info.command,
                    f.info.session_id,
                    ResumeReason::LeaseOpened.words()
                ),
            );
        }
        if let Err(why) = self.sync_journal() {
            self.fail(None, format!("the journal could not be trimmed: {why}"));
        }
        self.watch_frozen();
        if self.frozen.is_empty() {
            self.trigger.ended();
        }
    }

    /// Stops processes that appeared under a frozen subtree after it was
    /// stopped (forked before the parent was), and lets go of a subtree whose
    /// root ended.
    fn maintain_frozen(&mut self, table: &ReliefTable) {
        let mut ended: Vec<(usize, ResumeReason)> = Vec::new();
        // What joined each subtree, judged by the same predicate that chose
        // it: a late member that is an MCP server, a lease waiter, a heavy job,
        // a git process or in a worktree with a live index.lock is left
        // running, and so is everything under it. Anything that cannot be
        // checked is left too. The board is read once for all of them.
        let board = self.probe.lease_board();
        let mut locks: HashMap<PathBuf, bool> = HashMap::new();
        // Per subtree: the late members, and whether one of them holds a lease
        // slot (which makes the whole subtree a holder).
        let mut joiners: Vec<(usize, Vec<ReliefProc>, bool)> = Vec::new();
        {
            let mut screen = board.as_ref().ok().map(|board| Screen {
                probe: &*self.probe,
                board,
                refusing: self.refusing,
                marks: &mut self.marks,
                locks: &mut locks,
            });
            let mut checked = 0usize;
            for (at, frozen) in self.frozen.iter().enumerate() {
                if frozen.info.advised_only {
                    continue;
                }
                let (session, root) = (frozen.session, frozen.root);
                let Some(scan) = table
                    .sessions
                    .iter()
                    .find(|s| (s.root.pid, s.root.start_us) == session)
                else {
                    ended.push((at, ResumeReason::Gone));
                    continue;
                };
                if !scan.members.iter().any(|m| (m.pid, m.start_us) == root) {
                    ended.push((at, ResumeReason::Gone));
                    continue;
                }
                let Some(screen) = screen.as_mut() else {
                    continue;
                };
                let mut inside: HashSet<i32> = frozen.members.iter().map(|(p, _)| *p).collect();
                let mut rejected: HashSet<i32> = HashSet::new();
                let mut fresh: Vec<ReliefProc> = Vec::new();
                let mut holder = false;
                // Members are in table order, not tree order: sweep until no more join.
                for _ in 0..MAX_TREE_DEPTH {
                    let before = (fresh.len(), rejected.len());
                    for m in &scan.members {
                        if inside.contains(&m.pid)
                            || !inside.contains(&m.ppid)
                            || rejected.contains(&m.pid)
                            || frozen.left.contains(&(m.pid, m.start_us))
                        {
                            continue;
                        }
                        if checked >= MAX_CWD_CHECKS {
                            // The rest wait for the next tick.
                            continue;
                        }
                        checked += 1;
                        match screen.eligible(m) {
                            Ok(holds) => {
                                holder |= holds;
                                inside.insert(m.pid);
                                fresh.push(m.clone());
                            }
                            Err(_) => {
                                rejected.insert(m.pid);
                            }
                        }
                    }
                    if (fresh.len(), rejected.len()) == before {
                        break;
                    }
                }
                let room = MAX_SUBTREE_MEMBERS.saturating_sub(frozen.members.len());
                fresh.truncate(room);
                joiners.push((at, fresh, holder));
            }
        }
        if let Err(why) = &board {
            self.fail(
                None,
                format!(
                    "late members of a frozen subtree were not stopped: the build lease board could not be read: {why}"
                ),
            );
        }
        for (at, fresh, holder) in joiners {
            if fresh.is_empty() {
                continue;
            }
            // A late child is stopped only while its subtree's root is stopped
            // by us. A root that reads as running (the watchdog or a person
            // continued it) means the subtree is not frozen any more: stopping
            // a child of a running build hangs the build against it.
            let (root_pid, root_start) = self.frozen[at].root;
            match self.actuator.is_stopped(root_pid, root_start) {
                Ok(true) => {}
                Ok(false) => {
                    ended.push((at, ResumeReason::Continued));
                    continue;
                }
                // Ended: the next tick's table says so.
                Err(why) if has_ended(&why) => continue,
                Err(why) => {
                    self.fail(
                        Some(root_pid),
                        format!("late members were not stopped: the root's state could not be read: {why}"),
                    );
                    continue;
                }
            }
            // Each is read as running first; one somebody else stopped is left
            // alone (and remembered, so it is not asked about every tick).
            let mut running: Vec<ReliefProc> = Vec::new();
            for m in fresh {
                match self.actuator.is_stopped(m.pid, m.start_us) {
                    Ok(false) => running.push(m),
                    Ok(true) => {
                        self.frozen[at].left.insert((m.pid, m.start_us));
                    }
                    Err(why) if has_ended(&why) => {}
                    Err(why) => self.fail(
                        Some(m.pid),
                        format!(
                            "{} (pid {}) was not stopped: its state could not be read: {why}",
                            m.name, m.pid
                        ),
                    ),
                }
            }
            let fresh = running;
            if fresh.is_empty() {
                continue;
            }
            let session_id = self.frozen[at].info.session_id.clone();
            let before = self.journal.len();
            let since_ms = self.frozen[at].info.since_unix_ms;
            for m in &fresh {
                let entry = JournalEntry {
                    at_unix_ms: since_ms,
                    ..self.entry(m.pid, m.start_us, &session_id, JournalRung::Freeze)
                };
                self.journal.push(entry);
            }
            if let Err(why) = self.sync_journal() {
                self.journal.truncate(before);
                self.fail(None, format!("a new member of a frozen subtree was not stopped: the journal could not be written: {why}"));
                continue;
            }
            let since = self.frozen[at].since;
            let mut added = Vec::new();
            for m in &fresh {
                match self.actuator.signal(m.pid, m.start_us, Signal::Stop) {
                    Ok(Delivery::Delivered) => {
                        added.push((m.pid, m.start_us));
                        // In the watchdog's copy once it is stopped, counted
                        // from the subtree's own start.
                        self.watch.add(Watched {
                            pid: m.pid,
                            start_us: m.start_us,
                            since,
                        });
                    }
                    Ok(Delivery::Gone) => {}
                    Err(why) => self.fail(
                        Some(m.pid),
                        format!("could not stop {} (pid {}): {why}", m.name, m.pid),
                    ),
                }
            }
            if holder && !added.is_empty() {
                self.frozen[at].holder = true;
            }
            let added_ids: HashSet<i32> = added.iter().map(|(p, _)| *p).collect();
            let fresh_ids: HashSet<i32> = fresh.iter().map(|m| m.pid).collect();
            self.journal.retain(|e| {
                e.rung != JournalRung::Freeze
                    || e.session_id != session_id
                    || !fresh_ids.contains(&e.pid)
                    || added_ids.contains(&e.pid)
            });
            if let Err(why) = self.sync_journal() {
                self.fail(None, format!("the journal could not be trimmed: {why}"));
            }
            self.frozen[at].members.extend(added);
            self.frozen[at].info.members =
                u32::try_from(self.frozen[at].members.len()).unwrap_or(u32::MAX);
            self.watch_frozen();
        }
        // Highest index first, so a removal never moves one still to come.
        ended.sort_by_key(|(at, _)| std::cmp::Reverse(*at));
        for (at, reason) in ended {
            let f = self.frozen.remove(at);
            self.continue_all(&f.members);
            let ids: HashSet<i32> = f.members.iter().map(|(p, _)| *p).collect();
            self.journal
                .retain(|e| e.rung != JournalRung::Freeze || !ids.contains(&e.pid));
            if let Err(why) = self.sync_journal() {
                self.fail(None, format!("the journal could not be trimmed: {why}"));
            }
            self.note(
                ActionKind::Resume,
                format!(
                    "resumed {} in session {}: {}",
                    f.info.command,
                    f.info.session_id,
                    reason.words()
                ),
            );
        }
        self.watch_frozen();
        if self.frozen.is_empty() && self.trigger.frozen() {
            self.trigger.ended();
        }
    }
}

impl Drop for Relief {
    /// A sampler that stops (or panics) puts everything back: nothing stays
    /// stopped or slowed because the thread that did it is gone.
    fn drop(&mut self) {
        if self.frozen.is_empty() && self.slowed.is_empty() && self.journal.is_empty() {
            self.watch.set(Vec::new());
            return;
        }
        let _act = lock(&ACT);
        let replay = replay_journal(&self.dir, &self.ns, &*self.actuator);
        log_replay("stop", &replay);
        // Anything the journal could not list (a failed write) is resumed
        // from memory too.
        let members: Vec<(i32, u64)> = self
            .frozen
            .iter()
            .filter(|f| !f.info.advised_only)
            .flat_map(|f| f.members.iter().copied())
            .collect();
        for (pid, start_us) in members.into_iter().rev() {
            let _ = self.actuator.signal(pid, start_us, Signal::Cont);
        }
        for (&pid, &start_us) in &self.slowed {
            let ours =
                ledger_get(&lock(&LEDGER), &self.ns, pid) == Some((start_us, SlowOwner::Relief));
            if ours {
                let _ = self.actuator.set_background(pid, start_us, false);
                ledger_clear(&mut lock(&LEDGER), &self.ns, pid, start_us);
            }
        }
        self.watch.set(Vec::new());
    }
}

/// Whether a refusal from the actuator means there is nothing to change: the
/// process ended, or its pid belongs to another process now.
fn process_is_gone(why: &str) -> bool {
    why.contains("no longer running") || why.contains("recycled")
}

/// Whether a state read says the process ended: nothing to report. A
/// *recycled* pid is not that; it is a table entry that no longer describes a
/// live process, and is recorded as a failure like any refused signal.
fn has_ended(why: &str) -> bool {
    why.contains("no longer running")
}

fn frozen_info(pick: &Selection, advised_only: bool) -> FrozenSubtree {
    FrozenSubtree {
        root_pid: pick.root.pid,
        start_us: pick.root.start_us,
        session_pid: pick.session_pid,
        session_id: format!("{}-{}", pick.session_pid, pick.session_start_us),
        terminal_session_id: pick.terminal_session_id.clone(),
        repo_path: pick.repo_path.clone(),
        command: pick.root.name.clone(),
        footprint_bytes: pick.footprint,
        members: u32::try_from(pick.members.len()).unwrap_or(u32::MAX),
        since_unix_ms: unix_ms(),
        advised_only,
    }
}

/// How deep each member is below the session root, by pid.
fn depths(members: &[ReliefProc], root: i32) -> HashMap<i32, usize> {
    let ppid: HashMap<i32, i32> = members.iter().map(|m| (m.pid, m.ppid)).collect();
    members
        .iter()
        .map(|m| {
            let (mut depth, mut at) = (0usize, m.ppid);
            while at != root && depth < MAX_TREE_DEPTH {
                match ppid.get(&at) {
                    Some(&next) => at = next,
                    None => break,
                }
                depth += 1;
            }
            (m.pid, depth)
        })
        .collect()
}

fn human_bytes(bytes: u64) -> String {
    const GIB: f64 = (1u64 << 30) as f64;
    const MIB: f64 = (1u64 << 20) as f64;
    if bytes as f64 >= GIB {
        format!("{:.1} GiB", bytes as f64 / GIB)
    } else {
        format!("{:.0} MiB", bytes as f64 / MIB)
    }
}

#[cfg(test)]
#[path = "relief_tests.rs"]
mod relief_tests;
