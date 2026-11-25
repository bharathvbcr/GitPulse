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
//!   [`super::ENTER_SAMPLES`] samples puts every eligible process of every agent
//!   session (launched or observed) in `PRIO_DARWIN_BG`; the priority is put back
//!   once the host has read below Elevated for [`super::EXIT_SAMPLES`] samples.
//! * **Rung B, freeze.** Memory-bound Severe for [`SEVERE_SAMPLES`] samples (60
//!   s) *and* deterioration (swap or compressor segments still rising across
//!   that window by [`SWAP_RISE_BYTES`] or [`SEGMENT_RISE_POINTS`]) `SIGSTOP`s
//!   the single heaviest eligible subtree of a GitPulse-launched session in a
//!   trusted repository, below the shells the agent started, never the shell or
//!   the agent. Level alone never triggers it: swap does not drain after a
//!   recovery, so a high but flat swap is not a reason. At most one subtree per
//!   session and [`MAX_FROZEN_PER_HOST`] per host.
//!
//! Nothing is frozen indefinitely. The subtrees resume (`SIGCONT`) when
//! deterioration has been flat for [`FLAT_SAMPLES`] samples, when the tier has
//! read below Severe for [`super::EXIT_SAMPLES`] samples, or at
//! [`FREEZE_CAP`], whichever comes first; at the cap the freeze is not
//! escalated (there is no terminate rung in this slice) and the cap hit is
//! counted. Any future kill path must `SIGCONT` first: a signal to a stopped
//! process waits for the continue (`terminal/jobs.rs` does the same).
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
//! working inside a worktree with a live `index.lock`; build-lease holders and
//! waiters (the lease board, `lease::Dir::board`, because `procscan` cannot see
//! a descriptor lock) and trees whose environment carries `GITPULSE_LEASE_HELD`,
//! `CARGO_SLOT_HELD` or `MAC_HEAVY_LABEL` (read from `KERN_PROCARGS2`, which
//! macOS hides for Apple platform binaries: the marker is found on the program
//! that carries it, `cargo`, `go`, `rustc`, not on `/bin/bash`); MCP servers
//! (every direct child of the root that is not a shell, and anything named
//! `*mcp*`); and the shells that are direct children of the root. A check that
//! could not run (an unreadable board, environment or working directory) spares
//! the subtree: unknown is never "clear".
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

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::governor::{memory_sample, Input as GovernorInput, ENTER_SAMPLES, EXIT_SAMPLES};
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

/// Environment variables whose presence marks a build or a heavy job: the
/// lease wrapper's nested-call marker, the slot shim's, and `mac_heavy.sh`'s.
const HELD_MARKERS: [&str; 3] = ["GITPULSE_LEASE_HELD", "CARGO_SLOT_HELD", "MAC_HEAVY_LABEL"];

/// Names of the shells an agent runs commands through.
const SHELLS: [&str; 8] = ["sh", "bash", "zsh", "fish", "dash", "ksh", "tcsh", "csh"];

// ---------------------------------------------------------------------------
// The trigger: a pure decision core, one sample at a time.
// ---------------------------------------------------------------------------

/// What one tick read, reduced to what the trigger decides on.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Sample {
    /// The memory-only tier and the input that put it there; `None` when the
    /// host cannot be governed on (a stale or unreadable sensor).
    pub tier: Option<(Tier, Option<Binding>)>,
    pub swap_used: Option<u64>,
    /// Compressor segments in use, in percent of the limit.
    pub segment_pct: Option<f64>,
}

impl Sample {
    pub(crate) fn of(input: &GovernorInput) -> Self {
        Self {
            tier: memory_sample(input),
            swap_used: input.readings.swap_used.value(),
            segment_pct: input
                .readings
                .segments
                .value()
                .filter(|s| s.limit > 0)
                .map(|s| s.total as f64 * 100.0 / s.limit as f64),
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
    window: VecDeque<(Option<u64>, Option<f64>)>,
    phase: Phase,
    retry_wait: u32,
    /// A person ended this episode's freeze (`resume_all`): no new freeze
    /// until the tier has read below Severe for [`SEVERE_BREAK_SAMPLES`].
    held_off: bool,
}

impl Trigger {
    /// No freeze until the memory tier has dropped below Severe.
    pub(crate) fn hold_off(&mut self) {
        self.held_off = true;
    }

    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }

    pub(crate) fn observe(&mut self, now: Instant, sample: &Sample) -> Verdict {
        let tier = sample.tier.map(|(tier, _)| tier);
        if tier.is_some_and(|t| t >= Tier::Elevated) {
            self.slow_down = 0;
            self.slow_up = self.slow_up.saturating_add(1);
            if self.slow_up >= ENTER_SAMPLES {
                self.slowing = true;
            }
        } else {
            // An ungovernable sample (stale, dead or unreadable sensor) cannot
            // justify slowing, so it counts toward letting go.
            self.slow_up = 0;
            self.slow_down = self.slow_down.saturating_add(1);
            if self.slow_down >= EXIT_SAMPLES {
                self.slowing = false;
            }
        }

        if tier == Some(Tier::Severe) {
            self.severe_run = self.severe_run.saturating_add(1);
            self.calm_run = 0;
        } else {
            self.calm_run = self.calm_run.saturating_add(1);
            if self.calm_run >= SEVERE_BREAK_SAMPLES {
                self.severe_run = 0;
                self.window.clear();
                self.held_off = false;
            }
        }
        self.window
            .push_back((sample.swap_used, sample.segment_pct));
        while self.window.len() > SEVERE_SAMPLES as usize {
            self.window.pop_front();
        }

        let freeze = match self.phase {
            Phase::Idle => {
                self.retry_wait = self.retry_wait.saturating_sub(1);
                if self.retry_wait == 0
                    && !self.held_off
                    && self.severe_run >= SEVERE_SAMPLES
                    && deteriorating(self.window.iter())
                {
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
                } else if samples >= FLAT_SAMPLES && !deteriorating(self.window.iter().skip(recent))
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
}

/// What selection reads about a process beyond the scan.
pub(crate) trait Probe: Send {
    fn cwd(&self, pid: i32) -> Option<PathBuf>;
    /// `NAME=value` strings; `None` when unreadable.
    fn environment(&self, pid: i32) -> Option<Vec<String>>;
    /// The pids that hold or wait for a build lease. `Err` when the board
    /// could not be read in full: then nothing is frozen.
    fn lease_pids(&self) -> Result<BTreeSet<i32>, String>;
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

    #[cfg(unix)]
    fn lease_pids(&self) -> Result<BTreeSet<i32>, String> {
        let root = super::lease::lease_dir(&self.config);
        if !root.exists() {
            // No build ever took a lease on this host.
            return Ok(BTreeSet::new());
        }
        let board = super::lease::Dir::open(&root)?.board(&super::lease::Own::default());
        if !board.unreadable.is_empty() || !board.unreadable_slots.is_empty() {
            return Err(format!(
                "{} lease entries could not be read, so a build may be running",
                board.unreadable.len() + board.unreadable_slots.len()
            ));
        }
        Ok(board
            .holders
            .iter()
            .map(|h| h.pid)
            .chain(board.waiters.iter().map(|w| w.record.pid))
            .collect())
    }

    #[cfg(not(unix))]
    fn lease_pids(&self) -> Result<BTreeSet<i32>, String> {
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

/// Why a subtree may not be frozen, or `None` when nothing bars it.
fn barred(
    chain: &[&ReliefProc],
    subtree: &[&ReliefProc],
    probe: &dyn Probe,
    leases: &BTreeSet<i32>,
    locks: &mut HashMap<PathBuf, bool>,
) -> Option<String> {
    for p in chain.iter().chain(subtree.iter()) {
        if p.exempt {
            return Some(format!("{} is exempt from throttling", p.name));
        }
    }
    for p in chain.iter().chain(subtree.iter()) {
        if is_git(&p.name) {
            return Some("a git process is in it".into());
        }
        if is_mcp(&p.name) {
            return Some(format!("{} looks like an MCP server", p.name));
        }
        if leases.contains(&p.pid) {
            return Some(format!("{} holds or waits for a build lease", p.name));
        }
    }
    // Environments: the subtree's root and the first few members, and the
    // shells above it. `mac_heavy.sh`'s and the lease wrapper's markers are
    // inherited, so a marker anywhere in the chain reaches the programs below.
    for p in chain.iter().chain(subtree.iter().take(MAX_ENV_CHECKS)) {
        let Some(env) = probe.environment(p.pid) else {
            return Some(format!("the environment of {} could not be read", p.name));
        };
        for marker in HELD_MARKERS {
            let prefix = format!("{marker}=");
            if env
                .iter()
                .any(|var| var.strip_prefix(&prefix).is_some_and(|v| !v.is_empty()))
            {
                return Some(format!("{} carries {marker}", p.name));
            }
        }
    }
    for p in chain.iter().chain(subtree.iter()).take(MAX_CWD_CHECKS) {
        let Some(cwd) = probe.cwd(p.pid) else {
            return Some(format!(
                "the working directory of {} could not be read",
                p.name
            ));
        };
        if *locks
            .entry(cwd.clone())
            .or_insert_with(|| index_lock_live(&cwd))
        {
            return Some(format!(
                "{} works in a worktree with a live git index.lock",
                p.name
            ));
        }
    }
    None
}

/// The heaviest eligible subtree of one session, below the shells the agent
/// started, and why the heavier ones were passed over.
fn select_in_session(
    session: &ReliefSession,
    probe: &dyn Probe,
    leases: &BTreeSet<i32>,
    locks: &mut HashMap<PathBuf, bool>,
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
    // Only what runs below a shell the agent itself started. Every other
    // direct child of the root is something else (an MCP server's stdio
    // process, a helper) and so is everything under it.
    let shells: Vec<usize> = (0..members.len())
        .filter(|&i| {
            members[i].ppid == session.root.pid && is_shell(&members[i].name) && !members[i].exempt
        })
        .collect();
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
        if let Some(why) = barred(&chain, &subtree_procs, probe, leases, locks) {
            notes.push(format!("{}: {why}", members[top].name));
            continue;
        }
        return Some(Selection {
            session_pid: session.root.pid,
            session_start_us: session.root.start_us,
            terminal_session_id: session.terminal_session_id.clone(),
            root: members[top].clone(),
            members: subtree_procs.into_iter().cloned().collect(),
            footprint: sums[n],
            repo_path: None,
        });
    }
    None
}

/// The subtrees rung B stops: for each GitPulse-launched session in a trusted
/// repository that has none frozen already, its heaviest eligible subtree; the
/// heaviest `room` of those. `Err` when the lease board could not be read:
/// then nothing is chosen.
pub(crate) fn select(
    table: &ReliefTable,
    probe: &dyn Probe,
    trusted: &dyn Fn(&Path) -> bool,
    busy: &BTreeSet<(i32, u64)>,
    room: usize,
    notes: &mut Vec<String>,
) -> Result<Vec<Selection>, String> {
    if room == 0 {
        return Ok(Vec::new());
    }
    let leases = probe
        .lease_pids()
        .map_err(|why| format!("the build lease board could not be read: {why}"))?;
    let mut locks: HashMap<PathBuf, bool> = HashMap::new();
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
        if let Some(mut pick) = select_in_session(session, probe, &leases, &mut locks, notes) {
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

/// Every process GitPulse has put in the background, and who asked.
static LEDGER: Mutex<BTreeMap<i32, (u64, SlowOwner)>> = Mutex::new(BTreeMap::new());

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
    steward_throttle_with(&OsActuator, pid, start_us, enable)
}

fn steward_throttle_with(
    actuator: &dyn Actuator,
    pid: i32,
    start_us: u64,
    enable: bool,
) -> Result<bool, String> {
    let mut ledger = lock(&LEDGER);
    if enable {
        let changed = actuator.set_background(pid, start_us, true)?;
        if changed {
            ledger.insert(pid, (start_us, SlowOwner::Steward));
        }
        return Ok(changed);
    }
    if ledger.get(&pid) == Some(&(start_us, SlowOwner::Relief)) {
        return Ok(false);
    }
    let result = actuator.set_background(pid, start_us, false);
    if ledger
        .get(&pid)
        .is_some_and(|(start, _)| *start == start_us)
    {
        ledger.remove(&pid);
    }
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
        writer_pid: std::process::id(),
        writer_start_us: super::process_start_us(std::process::id()).unwrap_or(0),
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
pub(crate) fn replay_journal(dir: &Path, actuator: &dyn Actuator) -> Replay {
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
            JournalRung::Freeze => match actuator.signal(entry.pid, entry.start_us, Signal::Cont) {
                Ok(Delivery::Delivered) => out.resumed += 1,
                Ok(Delivery::Gone) => {}
                Err(why) => out.failures.push(format!(
                    "could not resume process {} of session {}: {why}",
                    entry.pid, entry.session_id
                )),
            },
            JournalRung::Slow => match restore_priority(actuator, entry.pid, entry.start_us) {
                Ok(true) => out.restored += 1,
                Ok(false) => {}
                Err(why) => out.failures.push(format!(
                    "could not restore the priority of process {}: {why}",
                    entry.pid
                )),
            },
        }
    }
    if let Err(why) = write_journal(dir, &[]) {
        out.failures
            .push(format!("the relief journal could not be removed: {why}"));
    }
    out
}

/// Restores a priority, treating a process that ended as nothing to restore.
fn restore_priority(actuator: &dyn Actuator, pid: i32, start_us: u64) -> Result<bool, String> {
    let mut ledger = lock(&LEDGER);
    let result = match actuator.set_background(pid, start_us, false) {
        Err(why) if why.contains("no longer running") => Ok(false),
        other => other,
    };
    if ledger
        .get(&pid)
        .is_some_and(|(start, _)| *start == start_us)
    {
        ledger.remove(&pid);
    }
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
    let _act = lock(&ACT);
    let replay = replay_journal(dir, &OsActuator);
    log_replay("shutdown", &replay);
    *lock(&SNAPSHOT) = None;
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
}

/// What the relief's sampler is told about a [`resume_all`] it did not make
/// itself.
#[derive(Debug, Clone, Default)]
struct External {
    generation: u64,
    hold_off: bool,
    reason: String,
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
    let _act = lock(&ACT);
    let mut out = ResumeOutcome::default();
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
        let (frozen, rest): (Vec<JournalEntry>, Vec<JournalEntry>) = journal
            .entries
            .into_iter()
            .partition(|e| e.rung == JournalRung::Freeze);
        for entry in frozen.iter().rev() {
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
}

/// The relief. One per sampler; it lives on the sampler's thread and is
/// ticked only while that sampler holds the writer election.
pub(crate) struct Relief {
    dir: PathBuf,
    actuator: Box<dyn Actuator>,
    probe: Box<dyn Probe>,
    mode: Box<dyn Fn() -> SupervisorMode + Send>,
    trusted: Box<dyn Fn(&Path) -> bool + Send>,
    trigger: Trigger,
    /// Processes this relief put in the background (its ledger share).
    slowed: BTreeMap<i32, u64>,
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
            dir,
            actuator,
            probe,
            mode,
            trusted,
            trigger: Trigger::default(),
            slowed: BTreeMap::new(),
            foreign: BTreeSet::new(),
            frozen: Vec::new(),
            journal: Vec::new(),
            status: ReliefSnapshot::idle(initial),
            publishing: false,
            seen_external,
            slow_since_pass: None,
            recovered: false,
        }
    }

    /// The relief on this machine: real signals, the stored mode, trust from
    /// the grant store. Remembers `dir` for [`shutdown`].
    pub(crate) fn os(dir: PathBuf) -> Self {
        let _ = APP_DIR.set(dir.clone());
        Self::new(
            dir.clone(),
            Box::new(OsActuator),
            Box::new(OsProbe { config: dir }),
            Box::new(crate::tool_config::host_load_supervisor),
            Box::new(repository_trusted),
        )
        .publishing()
    }

    /// This relief's state is what [`snapshot`] serves.
    pub(crate) fn publishing(mut self) -> Self {
        self.publishing = true;
        self
    }

    /// Puts back what a previous GitPulse left (a crash, a kill): called once,
    /// on the tick this sampler wins the writer election.
    pub(crate) fn recover(&mut self) {
        if self.recovered {
            return;
        }
        self.recovered = true;
        let _act = lock(&ACT);
        let replay = replay_journal(&self.dir, &*self.actuator);
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

    /// Takes account of a [`resume_all`] made from another thread: it already
    /// continued the journaled processes, so this only forgets them.
    fn reconcile_external(&mut self) {
        let external = external_of(&self.dir);
        if external.generation == self.seen_external {
            return;
        }
        self.seen_external = external.generation;
        let had = self.frozen.iter().filter(|f| !f.info.advised_only).count();
        self.frozen.clear();
        self.journal.retain(|e| e.rung != JournalRung::Freeze);
        self.trigger.ended();
        if external.hold_off {
            self.trigger.hold_off();
        }
        self.note(
            ActionKind::Resume,
            format!(
                "resumed {had} frozen subtree(s) on request: {}",
                external.reason
            ),
        );
    }

    /// One sample. `table` reads the sessions of the scan this tick just made;
    /// it is called only when something needs the processes.
    pub(crate) fn tick(
        &mut self,
        now: Instant,
        input: &GovernorInput,
        table: &dyn Fn() -> ReliefTable,
    ) {
        if STOPPING.load(Ordering::SeqCst) {
            return;
        }
        self.reconcile_external();
        let mode = (self.mode)();
        self.status.mode = mode;
        if mode == SupervisorMode::Off || !input.enabled {
            self.stand_down(ResumeReason::Off);
            self.status.binding = None;
            self.status.tier = None;
            self.publish();
            return;
        }
        let sample = Sample::of(input);
        self.status.tier = sample.tier.map(|(tier, _)| tier);
        self.status.binding = sample.tier.and_then(|(_, binding)| binding);
        let verdict = self.trigger.observe(now, &sample);
        let _act = lock(&ACT);
        if STOPPING.load(Ordering::SeqCst) {
            return;
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
            self.restore_slowed();
        }

        // Rung B.
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
    fn stand_down(&mut self, reason: ResumeReason) {
        let _act = lock(&ACT);
        if !self.frozen.is_empty() {
            self.resume_frozen(reason);
        }
        if !self.slowed.is_empty() {
            self.restore_slowed();
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

    /// Forgets what has ended: a process that is no longer in the table (by pid
    /// and start time) leaves `slowed`, `foreign`, the ledger and the journal,
    /// so the cap counts live processes and not every one a long build made.
    /// Returns whether the journal changed.
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
            let mut ledger = lock(&LEDGER);
            for (pid, start_us) in &gone {
                self.slowed.remove(pid);
                if ledger.get(pid) == Some(&(*start_us, SlowOwner::Relief)) {
                    ledger.remove(pid);
                }
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
        let stewards: HashSet<(i32, u64)> = lock(&LEDGER)
            .iter()
            .filter(|(_, (_, owner))| *owner == SlowOwner::Steward)
            .map(|(pid, (start_us, _))| (*pid, *start_us))
            .collect();
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
        fresh.truncate(MAX_SLOWED.saturating_sub(self.slowed.len()));
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
        let mut ledger = lock(&LEDGER);
        for (_, m, _) in &fresh {
            match self.actuator.set_background(m.pid, m.start_us, true) {
                Ok(true) => {
                    self.slowed.insert(m.pid, m.start_us);
                    ledger.insert(m.pid, (m.start_us, SlowOwner::Relief));
                    taken += 1;
                }
                Ok(false) => {
                    if self.slowed.contains_key(&m.ppid) {
                        // Inherited from a parent this relief slowed.
                        self.slowed.insert(m.pid, m.start_us);
                        ledger.insert(m.pid, (m.start_us, SlowOwner::Relief));
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
                    if !why.contains("no longer running") {
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

    fn restore_slowed(&mut self) {
        let slowed = std::mem::take(&mut self.slowed);
        let mut restored = 0u32;
        let mut refused: BTreeMap<String, u32> = BTreeMap::new();
        {
            let mut ledger = lock(&LEDGER);
            for (&pid, &start_us) in &slowed {
                if ledger.get(&pid) == Some(&(start_us, SlowOwner::Steward)) {
                    continue;
                }
                match self.actuator.set_background(pid, start_us, false) {
                    Ok(true) => restored += 1,
                    Ok(false) => {}
                    Err(why) if why.contains("no longer running") => {}
                    Err(why) => *refused.entry(why).or_default() += 1,
                }
                if ledger.get(&pid).is_some_and(|(s, _)| *s == start_us) {
                    ledger.remove(&pid);
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
        let picks = match select(table, &*self.probe, &*self.trusted, &busy, room, &mut notes) {
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
                any |= self.freeze(&pick);
            } else {
                self.frozen.push(Frozen {
                    info: frozen_info(&pick, true),
                    session: (pick.session_pid, pick.session_start_us),
                    root: (pick.root.pid, pick.root.start_us),
                    members: Vec::new(),
                });
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

    /// Stops one subtree: journal first, then the root, then the rest.
    fn freeze(&mut self, pick: &Selection) -> bool {
        let session = format!("{}-{}", pick.session_pid, pick.session_start_us);
        let before = self.journal.len();
        for m in &pick.members {
            let entry = self.entry(m.pid, m.start_us, &session, JournalRung::Freeze);
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
        for (n, m) in pick.members.iter().enumerate() {
            match self.actuator.signal(m.pid, m.start_us, Signal::Stop) {
                Ok(Delivery::Delivered) => stopped.push((m.pid, m.start_us)),
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
        let info = frozen_info(pick, false);
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
        self.frozen.push(Frozen {
            info,
            session: (pick.session_pid, pick.session_start_us),
            root: (pick.root.pid, pick.root.start_us),
            members: stopped,
        });
        true
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
        self.trigger.ended();
    }

    /// Stops processes that appeared under a frozen subtree after it was
    /// stopped (forked before the parent was), and lets go of a subtree whose
    /// root ended.
    fn maintain_frozen(&mut self, table: &ReliefTable) {
        let mut ended = Vec::new();
        for at in 0..self.frozen.len() {
            if self.frozen[at].info.advised_only {
                continue;
            }
            let (session, root) = (self.frozen[at].session, self.frozen[at].root);
            let Some(scan) = table
                .sessions
                .iter()
                .find(|s| (s.root.pid, s.root.start_us) == session)
            else {
                ended.push(at);
                continue;
            };
            if !scan.members.iter().any(|m| (m.pid, m.start_us) == root) {
                ended.push(at);
                continue;
            }
            let known: HashSet<i32> = self.frozen[at].members.iter().map(|(p, _)| *p).collect();
            let mut inside: HashSet<i32> = known.clone();
            let mut fresh: Vec<&ReliefProc> = Vec::new();
            // Members are in table order, not tree order: sweep until no more join.
            for _ in 0..MAX_TREE_DEPTH {
                let before = fresh.len();
                for m in &scan.members {
                    if !inside.contains(&m.pid)
                        && inside.contains(&m.ppid)
                        && !m.exempt
                        && !is_git(&m.name)
                    {
                        inside.insert(m.pid);
                        fresh.push(m);
                    }
                }
                if fresh.len() == before {
                    break;
                }
            }
            let room = MAX_SUBTREE_MEMBERS.saturating_sub(self.frozen[at].members.len());
            fresh.truncate(room);
            if fresh.is_empty() {
                continue;
            }
            let session_id = self.frozen[at].info.session_id.clone();
            let before = self.journal.len();
            for m in &fresh {
                let entry = self.entry(m.pid, m.start_us, &session_id, JournalRung::Freeze);
                self.journal.push(entry);
            }
            if let Err(why) = self.sync_journal() {
                self.journal.truncate(before);
                self.fail(None, format!("a new member of a frozen subtree was not stopped: the journal could not be written: {why}"));
                continue;
            }
            let mut added = Vec::new();
            for m in &fresh {
                match self.actuator.signal(m.pid, m.start_us, Signal::Stop) {
                    Ok(Delivery::Delivered) => added.push((m.pid, m.start_us)),
                    Ok(Delivery::Gone) => {}
                    Err(why) => self.fail(
                        Some(m.pid),
                        format!("could not stop {} (pid {}): {why}", m.name, m.pid),
                    ),
                }
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
        }
        for at in ended.into_iter().rev() {
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
                    ResumeReason::Gone.words()
                ),
            );
        }
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
            return;
        }
        let _act = lock(&ACT);
        let replay = replay_journal(&self.dir, &*self.actuator);
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
        let mut ledger = lock(&LEDGER);
        for (&pid, &start_us) in &self.slowed {
            if ledger.get(&pid) == Some(&(start_us, SlowOwner::Relief)) {
                let _ = self.actuator.set_background(pid, start_us, false);
                ledger.remove(&pid);
            }
        }
    }
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
