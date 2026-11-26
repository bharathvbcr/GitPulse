//! The load verdict in `command-gate`: refuse a heavy command the host cannot
//! take right now, and say so when it could not tell.
//!
//! # Why the hook
//!
//! On 2026-11-14 this Mac hung with swap at 0.9x RAM and a load of 120 on 18
//! cores. The sessions doing it were Claude desktop sessions GitPulse only
//! observes, so launch admission never touched them. The one piece of GitPulse
//! that runs inside every one of them is this hook.
//!
//! # What it reads, and what it never does
//!
//! It reads the app's published verdict from `host-load.v1`
//! ([`PublishedVerdict`]) and nothing else about the host, plus, once a command
//! is known to be in the table, the pressure-relief supervisor's
//! `host-relief.v1.json` (see "Paused builds" below). It never samples the
//! machine and never re-derives a threshold: whether a launch is admitted is
//! the governor's decision, made once, and this refuses exactly what that
//! refuses. The setting that decides what to do with it is read from
//! `tools.json` by the hook itself, after a command has been found heavy.
//!
//! # Disk headroom
//!
//! One thing is read live rather than from the verdict: free disk space on the
//! volume the command writes to (the `cwd` the hook was given, not the hook
//! process's own: for an orchestrated child terminal the hook runs inside the
//! GitPulse app). A coverage build (any `cargo llvm-cov` in the line that
//! builds) needs [`DiskFacts::coverage_headroom_bytes`] free before it starts,
//! and any heavy build needs the disk rule's severe floor: on 2026-11-15 a
//! coverage build took the disk from about 150 GiB to nothing in about fifteen
//! minutes, and the published verdict is two seconds old by the time a build
//! that writes 10 GiB a minute reads it. Each is its own input with a mode
//! (refuse, shed = say so, off) from the settings, under the hook's `hook_mode`
//! like every other refusal here. A reading that could not be taken is said
//! once per session, never a pass.
//!
//! The two master switches bind it as they bind the load check:
//! `host_load.enabled` off means nothing is measured and it says nothing, and
//! `host_load.govern_launches` off means it advises and never refuses.
//!
//! # The answers
//!
//! | The command | The verdict | The hook |
//! |---|---|---|
//! | not in the table | any | nothing, zero output |
//! | heavy | not admitted, sensor fresh or stale-recent | **refuse** (`enforce`) or say the same as context (`advise`) |
//! | heavy | elevated, admitted | one line of advice |
//! | heavy | clear | nothing |
//! | heavy | none: older writer, sensor stale-old or dead, no snapshot, switched off | one line saying it was **not** load-checked, never a refusal |
//! | in the table as advise-only | elevated or severe | one line of advice, never a refusal |
//! | any classified command | the relief supervisor paused a tree for this session | the paused command named, "resumes automatically", do not retry or run in parallel; inside the reason when the command is refused |
//! | any classified command | the supervisor only slowed agent processes | one line of advice |
//! | any classified command | the relief supervisor ended a tree for this session (rung C) | "ended by memory protection", must not be re-run until pressure clears; only while the status is fresh and the episode lasts |
//! | any classified command | the relief status is stale or unreadable | one line saying it could **not** be checked, once per session; never "nothing is paused" |
//! | any classified command | no relief status, or one at rest | nothing |
//!
//! # Paused builds
//!
//! A tree is "this session's" when its `terminal_session_id` is the terminal
//! this hook runs under (`GITPULSE_SESSION_ID`). Only when either side does not
//! say which terminal it is, the root of the worktree the call runs in must
//! equal the tree's `repo_path` exactly: a sibling worktree under the same
//! repository is another session. An advised-only entry is what Act would have paused and is
//! never reported as paused. The relief note is silent with the gate set to Off
//! and for the heavy lane's holder, like everything else in this stage.
//!
//! It never emits `allow`: an unearned allow would override the person's own
//! permission rules. A check that could not run says so; it does not look like
//! one that ran and passed.
//!
//! # Bypass mode
//!
//! `under_bypass` waives the guard and gate refusals, because bypass is the
//! host's "stop asking me" and those refusals are permission questions. This
//! refusal is not one: it protects the machine from every session at once, and
//! the sessions the 2026-11-14 hang came from run in bypass. It is enforced
//! there, and `hook_mode` is how a person turns it down.
//!
//! # The loop breaker
//!
//! A model that is refused and tries again is the failure to avoid, so the
//! fifth refusal of the same command in one session inside ten minutes says
//! so, loudly, and the person is told. It grants nothing.

use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use super::advisory_once::{marker_dir, private_dir, prune, PRUNE_SCAN};
use super::heavy_commands::{classify, Classified, Hit};
use super::{
    decision, fnv1a, HookInput, HookOutput, HookSpecificOutput, PermissionDecision, PRE_TOOL_USE,
};
use crate::sysload::relief_status::{self, ReliefOutcome, ReliefStaleCause, ReliefStatus};
use crate::sysload::{
    Binding, InputMode, PublishedVerdict, Reading, SensorReport, SensorState, Snapshot, Tier,
};
use crate::tool_config::HostLoadHookMode;

/// Identical refusals, in one session, that make the next one a stop.
pub(super) const LOOP_AT: usize = 5;

/// How far back [`LOOP_AT`] counts.
pub(super) const LOOP_WINDOW: Duration = Duration::from_secs(10 * 60);

/// How far ahead of `now` a marker's stamp may be and still count.
const CLOCK_SKEW: Duration = Duration::from_secs(60);

/// The most a message carries. The host caps a hook's text at 10,000
/// characters; this stays far under it and never echoes a long command.
const MAX_TEXT: usize = 1_500;

/// What the verdict side of the world says, read once per heavy command.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Availability {
    /// The app's held verdict, from a snapshot a reader may act on.
    Verdict {
        verdict: PublishedVerdict,
        state: SensorState,
        age_ms: Option<u64>,
    },
    /// Why there is none. Said to the model; never a reason to refuse.
    Unavailable(String),
}

/// Everything the decision needs from outside the command line.
#[derive(Debug, Clone)]
pub(super) struct Gathered {
    pub mode: HostLoadHookMode,
    /// The heavy lane's holder runs this call (`MAC_HEAVY_LABEL`).
    pub exempt: bool,
    pub availability: Availability,
    /// Where the loop breaker keeps its markers.
    pub state_dir: PathBuf,
    pub now: SystemTime,
    pub disk: DiskFacts,
    /// Free bytes on the volume holding a path, `None` when it cannot be read;
    /// the real reader, or a stand-in a test injects. It reads the volumes a
    /// line names as its build directory, which are known only once the line
    /// has been classified.
    pub read_free: fn(&Path) -> Free,
    /// What the lease wrapper does for a build in this session.
    pub lease: LeaseSource,
    /// Active Manvi system steward critical slowdown lease, when in force for
    /// this repository and not exempt (`MAC_HEAVY_LABEL`).
    pub steward_throttle: Option<crate::harness::steward::ActiveThrottleRecord>,
    /// What the pressure-relief supervisor published (`host-relief.v1.json`).
    /// `Absent` is the normal state until a supervisor has ever written one.
    pub relief: ReliefOutcome,
    /// The GitPulse terminal this hook runs under (`GITPULSE_SESSION_ID`), the
    /// key a paused tree is matched to a caller by. Injected, never read from
    /// the environment inside the judge.
    pub terminal_session: Option<String>,
    /// The root of the worktree or checkout the call's `cwd` is in: what a
    /// paused tree's `repo_path` is matched against when a terminal id is
    /// missing on either side.
    pub session_root: Option<PathBuf>,
}

/// Where the facts about the build lease come from.
#[derive(Debug, Clone)]
pub(super) enum LeaseSource {
    /// Read from the lease directory and the environment, once a leased build
    /// is found.
    Live,
    /// Handed in, for tests.
    #[cfg(test)]
    Fixed(LeaseFacts),
}

/// What the hook says about a leased build besides its load.
#[derive(Debug, Clone, Default)]
pub(super) struct LeaseFacts {
    /// Leases are on and this platform has them. Off says nothing at all.
    pub on: bool,
    /// Why this session's builds do not go through the wrapper, or `None` when
    /// they do.
    pub not_gated: Option<String>,
    /// Who holds the slots when every one is held.
    pub full: Option<String>,
    /// Why no build may start at all: the governor's load refuses new work
    /// (Severe, or launches not admitted). Not the same as every slot held.
    pub refusing: Option<String>,
    /// How long a build waits for a slot before it starts without one.
    pub wait_max_s: u64,
    /// How long it keeps waiting while load refuses, before it gives up.
    pub wait_hard_s: u64,
}

#[cfg(unix)]
fn live_lease_facts(input: &HookInput, availability: &Availability) -> LeaseFacts {
    use crate::sysload::lease::{self, capacity, Dir, Load, Own};
    let settings = crate::tool_config::host_load_leases();
    let config = crate::tool_config::default_config_dir();
    let on = settings.mode == crate::tool_config::LeaseMode::On;
    if !on {
        return LeaseFacts::default();
    }
    // Gated means verified: the session-start hook wrote the PATH line to
    // CLAUDE_ENV_FILE and recorded that it did. GITPULSE_LEASE_BIN alone says
    // only how the process was started, and a shell startup file can still put
    // another cargo ahead of the lease folder.
    let gated = config
        .as_deref()
        .is_some_and(|c| super::lease_wrapper::session_has_path(c, &input.session_id));
    let not_gated = (!gated).then(|| {
        "the session-start hook has not confirmed that this session's PATH starts with GitPulse's lease folder (it records that when it writes the folder to CLAUDE_ENV_FILE); a shell startup file may also put another cargo ahead of it. Keep GitPulse's session-start hook enabled".to_string()
    });
    let load = match availability {
        Availability::Verdict { verdict, .. } => Load::Verdict(*verdict),
        Availability::Unavailable(why) => Load::Unknown(why.clone()),
    };
    let cap = capacity(settings.slots, &load);
    let refusing = cap.refuses.then(|| {
        cap.note
            .clone()
            .unwrap_or_else(|| "the host load is too high".to_string())
    });
    let full = (|| {
        if cap.refuses {
            return None;
        }
        let dir = Dir::open(&lease::lease_dir(config.as_deref()?)).ok()?;
        let board = dir.board(&Own::default());
        if !board.unreadable.is_empty() || board.occupied() < cap.slots as usize {
            return None;
        }
        let who: Vec<String> = board
            .holders
            .iter()
            .take(4)
            .map(|h| match &h.record {
                Some(r) => format!("{} in {} (pid {})", r.label, r.cwd, h.pid),
                None => format!("pid {}", h.pid),
            })
            .collect();
        Some(who.join("; "))
    })();
    LeaseFacts {
        on,
        not_gated,
        full,
        refusing,
        wait_max_s: settings.wait_max_s,
        wait_hard_s: settings.wait_hard_s,
    }
}

#[cfg(not(unix))]
fn live_lease_facts(_input: &HookInput, _availability: &Availability) -> LeaseFacts {
    LeaseFacts::default()
}

/// The lines about a leased build: once per session that it is not gated, and
/// every time that it will queue.
fn lease_lines(
    input: &HookInput,
    heavy: &Hit,
    availability: &Availability,
    g: &Gathered,
) -> Vec<String> {
    if !heavy.row.lease {
        return Vec::new();
    }
    let facts = match &g.lease {
        #[cfg(test)]
        LeaseSource::Fixed(facts) => facts.clone(),
        LeaseSource::Live => live_lease_facts(input, availability),
    };
    if !facts.on {
        return Vec::new();
    }
    let mut lines = Vec::new();
    if let Some(why) = &facts.not_gated {
        match first_notice(g, input, why) {
            Notice::Repeat => {}
            notice => lines.push(format!(
                "GitPulse did NOT lease-gate this build ({}): {why}. Builds in this session are not queued behind other sessions' builds.{}",
                heavy.label,
                notice.suffix()
            )),
        }
    }
    if let Some(why) = &facts.refusing {
        lines.push(format!(
            "Host load is refusing new builds ({why}), so this build will wait for it to clear for up to {} minutes and then give up without building. A Bash call does not wait that long: run it in the background, or try again later.",
            facts.wait_hard_s / 60
        ));
    } else if let Some(who) = &facts.full {
        lines.push(format!(
            "Every build slot is held ({who}), so this will queue for up to {} minutes and then start without a slot. A Bash call does not wait that long: run it in the background.",
            facts.wait_max_s / 60
        ));
    }
    lines
}

/// What reading one location's free space found. A missing path is told from
/// an unreadable one because only a missing path says nothing about the volume:
/// the nearest ancestor that exists is read in its place, while an unreadable
/// one is "could not read" and is never replaced by its ancestor's volume.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Free {
    Bytes(u64),
    Missing,
    Unreadable,
}

/// What the disk checks need, read once per heavy command.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct DiskFacts {
    /// Free bytes on the volume the command runs in; `None` when unreadable.
    pub free_bytes: Option<u64>,
    /// The disk rule's severe floor, and what its mode makes of a breach.
    pub floor_bytes: u64,
    pub floor_mode: InputMode,
    /// The headroom a coverage build needs, and its mode.
    pub coverage_headroom_bytes: u64,
    pub coverage_mode: InputMode,
    /// `host_load.enabled`: whether GitPulse measures the host at all. Off,
    /// the disk checks say nothing, as the load check does.
    pub measuring: bool,
    /// `host_load.govern_launches`: whether GitPulse refuses anything for
    /// host pressure. Off, the disk checks advise and never refuse.
    pub refusing: bool,
}

impl DiskFacts {
    /// The facts the settings say, for `free_bytes` free. The numbers are the
    /// resolved ones: the automatic floors already follow this machine's memory.
    fn from_settings(free_bytes: Option<u64>) -> Self {
        let settings = crate::tool_config::host_load_settings();
        let thresholds = settings.thresholds;
        let gib = |g: f64| (g * (1u64 << 30) as f64) as u64;
        Self {
            free_bytes,
            floor_bytes: gib(thresholds.disk.severe_at),
            floor_mode: thresholds.disk.mode,
            coverage_headroom_bytes: gib(thresholds.disk_guard.coverage.headroom_gib),
            coverage_mode: thresholds.disk_guard.coverage.mode,
            measuring: settings.enabled,
            refusing: settings.govern_launches,
        }
    }
}

/// The one line the SessionStart brief carries about host load, read from the
/// published snapshot: never a sample of the host.
pub(super) fn brief_line_live() -> String {
    let Some(dir) = crate::tool_config::default_config_dir() else {
        return "host load: UNKNOWN (there is no per-user config directory)".into();
    };
    let path = dir.join(crate::sysload::SNAPSHOT_FILE_NAME);
    let (report, snapshot) = crate::sysload::report(&path, crate::sysload::Clock::now());
    brief_line(&report, snapshot.as_ref())
}

/// [`brief_line_live`] over a report someone already read.
pub(super) fn brief_line(report: &SensorReport, snapshot: Option<&Snapshot>) -> String {
    let sessions = |snapshot: Option<&Snapshot>| match snapshot {
        Some(s) => {
            let count = |name: &str, reading: &Reading<crate::sysload::SessionCount>| match reading
            {
                Reading::Value(c) => {
                    format!("{name} {} launched / {} observed", c.launched, c.observed)
                }
                Reading::Unavailable(_) => format!("{name} unknown"),
            };
            format!(
                "sessions: {}, {}",
                count("Claude", &s.claude),
                count("Codex", &s.codex)
            )
        }
        None => "sessions: unknown".to_string(),
    };
    let line = match availability_of(report, snapshot) {
        Availability::Verdict { verdict, state, .. } => {
            let state = match state {
                SensorState::Fresh => "sensor fresh",
                _ => "sensor stale, reading recent",
            };
            let host = match verdict.tier {
                Tier::Clear => "clear".to_string(),
                tier => format!("{}, {}", tier_word(tier), binding_phrase(verdict.binding)),
            };
            let gate = if verdict.admit {
                ""
            } else {
                "; the governor is not admitting launches, and the command gate refuses heavy build and test commands"
            };
            format!("host load: {host} ({state}){gate}; {}", sessions(snapshot))
        }
        Availability::Unavailable(why) => format!(
            "host load: UNKNOWN ({why}); heavy commands are not load-checked; {}",
            sessions(snapshot)
        ),
    };
    bounded(line)
}

fn is_heavy_lane_exempt() -> bool {
    std::env::var_os("MAC_HEAVY_LABEL").is_some_and(|label| !label.is_empty())
}

/// Active Manvi steward critical slowdown record for `cwd`, unless this call
/// runs inside the heavy lane (`MAC_HEAVY_LABEL`). On a normal host where
/// `<config>/steward/active.json` does not exist, this is a single `NotFound`
/// `symlink_metadata` check and opens nothing.
pub(super) fn live_steward_throttle(
    cwd: &str,
) -> Option<crate::harness::steward::ActiveThrottleRecord> {
    if is_heavy_lane_exempt() {
        return None;
    }
    crate::harness::steward::active_throttle_for_cwd(cwd)
}

/// What the real world says. Called only for a command that is in the table.
pub(super) fn gather_live(cwd: &str) -> Gathered {
    let mode = crate::tool_config::host_load_hook_mode();
    let exempt = is_heavy_lane_exempt();
    let steward_throttle = if exempt {
        None
    } else {
        crate::harness::steward::active_throttle_for_cwd(cwd)
    };
    let live = |availability, exempt, disk, steward_throttle, relief: ReliefOutcome| {
        // The worktree root is looked up only when something is paused, so a
        // call on a calm machine pays for no extra directory walk.
        let session_root = match &relief {
            ReliefOutcome::Fresh { status } if status.paused_count() > 0 => {
                crate::engine::find_git_root(Path::new(cwd))
            }
            _ => None,
        };
        Gathered {
            mode,
            exempt,
            availability,
            state_dir: marker_dir(),
            now: SystemTime::now(),
            disk,
            read_free: real_free,
            lease: LeaseSource::Live,
            steward_throttle,
            relief,
            terminal_session: std::env::var(crate::alerts::bridge::SESSION_ENV)
                .ok()
                .filter(|id| !id.is_empty()),
            session_root,
        }
    };
    if mode == HostLoadHookMode::Off && steward_throttle.is_none() {
        return live(
            Availability::Unavailable(String::new()),
            exempt,
            DiskFacts::from_settings(None),
            None,
            ReliefOutcome::Absent,
        );
    }
    live(
        availability_live(),
        exempt,
        DiskFacts::from_settings(live_free_bytes(
            cwd,
            std::env::current_dir().ok(),
            real_free,
        )),
        steward_throttle,
        relief_live(),
    )
}

/// The supervisor's published status, read once per classified command. No
/// config directory is a read that could not be made, never an empty file.
fn relief_live() -> ReliefOutcome {
    match relief_status::default_dir() {
        Some(dir) => relief_status::read_status(&dir),
        None => ReliefOutcome::Unreadable {
            reason: "there is no per-user config directory (HOME unset)".into(),
        },
    }
}

/// The app's held verdict as a reader may act on it, read from the published
/// snapshot: what the command gate and the lease wrapper both ask.
pub(super) fn availability_live() -> Availability {
    match crate::tool_config::default_config_dir() {
        None => {
            Availability::Unavailable("there is no per-user config directory (HOME unset)".into())
        }
        Some(dir) => {
            let path = dir.join(crate::sysload::SNAPSHOT_FILE_NAME);
            let (report, snapshot) = crate::sysload::report(&path, crate::sysload::Clock::now());
            availability_of(&report, snapshot.as_ref())
        }
    }
}

/// The real reader of [`Gathered::read_free`].
fn real_free(path: &Path) -> Free {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Free::Missing,
        Err(_) => Free::Unreadable,
        Ok(_) => crate::sysload::free_bytes(path)
            .value()
            .map_or(Free::Unreadable, Free::Bytes),
    }
}

/// Free bytes on the volume the command writes to, read now: the working
/// directory the hook was given (`cwd`, which is the session's, and for an
/// orchestrated child terminal is not this process's: the hook then runs inside
/// the GitPulse app).
///
/// A `cwd` that was given and cannot be read (relative, missing, denied) is
/// `None`, never another volume's number: the process's directory, or the
/// snapshot's reading of the volume holding GitPulse's state, would be
/// reported as room the command may not have. Only when no `cwd` was given is
/// the process's own directory the best guess there is.
fn live_free_bytes(
    cwd: &str,
    process_dir: Option<PathBuf>,
    read: impl Fn(&Path) -> Free,
) -> Option<u64> {
    let dir = if cwd.is_empty() {
        process_dir?
    } else if Path::new(cwd).is_absolute() {
        PathBuf::from(cwd)
    } else {
        return None;
    };
    match read(&dir) {
        Free::Bytes(bytes) => Some(bytes),
        Free::Missing | Free::Unreadable => None,
    }
}

/// Free space on a build directory's volume: that of the nearest ancestor
/// that exists, since the directory itself may not yet. An ancestor that exists
/// and cannot be read stops the walk: that is "could not read", not the
/// volume of a directory above it.
fn free_at_or_above(dir: &Path, read: impl Fn(&Path) -> Free) -> Option<u64> {
    for ancestor in dir.ancestors() {
        match read(ancestor) {
            Free::Bytes(bytes) => return Some(bytes),
            Free::Missing => continue,
            Free::Unreadable => return None,
        }
    }
    None
}

/// What the free space of every location a heavy line writes to came to.
struct Room {
    /// The smallest reading among the locations that could be read.
    lowest: Option<u64>,
    /// Every location was read. "Has room" needs this; a location that was
    /// read and is short may still refuse on its own number without it.
    complete: bool,
}

/// The volume the line runs in and the build directories it names literally.
fn room(cwd_free: Option<u64>, classified: &Classified, g: &Gathered) -> Room {
    let mut readings: Vec<Option<u64>> = classified
        .build_dirs
        .iter()
        .map(|dir| free_at_or_above(Path::new(dir), g.read_free))
        .collect();
    readings.push(cwd_free);
    Room {
        lowest: readings.iter().flatten().copied().min(),
        complete: readings.iter().all(Option::is_some),
    }
}

/// What a sensor report and its snapshot allow the hook to act on.
pub(super) fn availability_of(report: &SensorReport, snapshot: Option<&Snapshot>) -> Availability {
    let age_ms = report.age_ms;
    if !report.state.governs() {
        return Availability::Unavailable(match &report.reason {
            Some(reason) => reason.clone(),
            None => format!(
                "the host-load sampler last wrote {}s ago, too long to act on",
                age_ms.unwrap_or(0) / 1000
            ),
        });
    }
    let Some(snapshot) = snapshot else {
        return Availability::Unavailable("there is no readable host-load snapshot".into());
    };
    if !snapshot.enabled {
        return Availability::Unavailable(
            "host-load measurement is switched off (host_load.enabled)".into(),
        );
    }
    match snapshot.verdict {
        Reading::Value(verdict) => Availability::Verdict {
            verdict,
            state: report.state,
            age_ms,
        },
        Reading::Unavailable(why) => Availability::Unavailable(format!(
            "the snapshot carries no governor verdict ({why:?}): an older GitPulse wrote it, or the sampler has not finished a tick"
        )),
    }
}

/// Cooperative pacing delay applied during active Manvi steward critical
/// slowdown, bounded to `1_200 ms` and reserving at least `6s` of
/// [`super::TOTAL_BUDGET`] (`8s`) so downstream `command_gate_with` keeps its
/// full `5s` budget.
///
/// `started` is the hook process's deadline anchor. Without one (the app
/// judging an orchestrated child's input line) nothing bounds the pause and
/// a sleep per line would stall the caller, so nothing is paced.
pub(super) fn pace_duration_for(
    requested_ms: u64,
    started: Option<std::time::Instant>,
) -> Duration {
    let Some(started) = started else {
        return Duration::ZERO;
    };
    let target = Duration::from_millis(requested_ms.clamp(0, 1_200));
    let reserve_for_gate = Duration::from_secs(6);
    let left = super::TOTAL_BUDGET
        .saturating_sub(started.elapsed())
        .saturating_sub(reserve_for_gate);
    target.min(left)
}

fn steward_pace_advice(
    throttle: &crate::harness::steward::ActiveThrottleRecord,
    paced: Duration,
) -> HookOutput {
    let waited = if paced.is_zero() {
        String::new()
    } else {
        format!(" GitPulse paced this command by {}ms.", paced.as_millis())
    };
    advice(&format!(
        "Manvi system steward critical slowdown is active ({} load: {}).{waited} \
         Pause heavy builds, cap concurrency, and avoid rapid command loops until critical host pressure clears.",
        throttle.tier, throttle.reason
    ))
}

/// The load stage of `command-gate`. Silent for everything that is not in the
/// table, and for `off`; otherwise the refusal or the advice described above.
///
/// `gather` runs only once the command is known to be in the table, so every
/// other call pays for the table lookup and nothing else.
#[cfg(test)]
pub(super) fn judge(input: &HookInput, gather: impl FnOnce() -> Gathered) -> HookOutput {
    judge_with_steward_and_pacer(
        input,
        gather,
        || None,
        Some(std::time::Instant::now()),
        |_| {},
    )
}

/// [`judge`] plus the live Manvi steward critical slowdown lease for commands
/// outside `heavy_commands` (such as custom scripts or command loops).
pub(super) fn judge_with_steward(
    input: &HookInput,
    gather: impl FnOnce() -> Gathered,
    unclassified_steward: impl FnOnce() -> Option<crate::harness::steward::ActiveThrottleRecord>,
) -> HookOutput {
    judge_with_steward_and_pacer(
        input,
        gather,
        unclassified_steward,
        super::STARTED.get().copied(),
        |d| {
            if !d.is_zero() {
                std::thread::sleep(d);
            }
        },
    )
}

pub(super) fn judge_with_steward_and_pacer(
    input: &HookInput,
    gather: impl FnOnce() -> Gathered,
    unclassified_steward: impl FnOnce() -> Option<crate::harness::steward::ActiveThrottleRecord>,
    started: Option<std::time::Instant>,
    pace: impl FnOnce(Duration),
) -> HookOutput {
    if input.command.is_empty() {
        return HookOutput::silent();
    }
    let classified = classify(&input.command);
    if classified.is_empty() {
        let Some(throttle) = unclassified_steward() else {
            return HookOutput::silent();
        };
        let delay = pace_duration_for(throttle.pace_ms, started);
        pace(delay);
        return steward_pace_advice(&throttle, delay);
    }
    let gathered = gather();
    judge_classified(input, &classified, &gathered, started, pace)
}

fn judge_classified(
    input: &HookInput,
    classified: &Classified,
    g: &Gathered,
    started: Option<std::time::Instant>,
    pace: impl FnOnce(Duration),
) -> HookOutput {
    let answer = judge_load(input, classified, g, started, pace);
    // The heavy-lane holder and a gate set to Off hear nothing from this stage,
    // the relief note included: it is the same "say nothing" switch.
    if g.exempt || g.mode == HostLoadHookMode::Off {
        return answer;
    }
    match relief_note(input, g) {
        Some(note) => with_note(answer, &note),
        None => answer,
    }
}

/// `note` on `answer`: inside the reason of a refusal, where the model reads
/// it as the cause, and as context otherwise.
fn with_note(answer: HookOutput, note: &str) -> HookOutput {
    if !refuses(&answer) {
        return fold(answer, advice(note));
    }
    let mut answer = answer;
    if let Some(specific) = answer.hook_specific_output.as_mut() {
        let reason = specific
            .permission_decision_reason
            .take()
            .unwrap_or_default();
        specific.permission_decision_reason = Some(format!("{reason}\n{note}"));
    }
    answer
}

/// The longest command echoed into the model's context.
const MAX_ECHOED_COMMAND: usize = 120;

/// What the hook says about the relief supervisor, if anything:
///
/// * a tree paused for this session (matched by terminal session id, or by the
///   session's cwd being inside the tree's repository): which command, that
///   GitPulse's memory protection paused it, that it resumes by itself and must
///   not be retried or run alongside;
/// * only a slowdown: one line of advice;
/// * a status that is stale or could not be read: said once per session that
///   it could not be checked, never that nothing is paused;
/// * absent, or a rest status: nothing.
fn relief_note(input: &HookInput, g: &Gathered) -> Option<String> {
    match &g.relief {
        ReliefOutcome::Absent => None,
        ReliefOutcome::Fresh { status } => relief_text(status, g),
        ReliefOutcome::Stale { age_ms, cause, .. } => unchecked_relief(
            input,
            g,
            // The kind, never the age: the age changes every second and would
            // make this a new notice, and a new marker file, on every command.
            &format!("stale:{}", cause.code()),
            &match cause {
                ReliefStaleCause::Age => format!(
                    "the status was last written {}s ago, so the supervisor may no longer be running",
                    age_ms / 1000
                ),
                ReliefStaleCause::WriterGone => {
                    "the process that wrote the status is no longer running".to_string()
                }
                ReliefStaleCause::ClockAhead => {
                    "the status is dated ahead of this machine's clock".to_string()
                }
            },
        ),
        ReliefOutcome::Unreadable { reason } => unchecked_relief(input, g, "unreadable", reason),
    }
}

fn unchecked_relief(input: &HookInput, g: &Gathered, kind: &str, why: &str) -> Option<String> {
    let why = clip(&crate::alerts::text::sanitize(why), 200);
    match first_notice(g, input, &format!("relief: {kind}")) {
        Notice::Repeat => None,
        notice => Some(format!(
            "GitPulse could NOT check whether its memory protection has paused a build or test ({why}). \
             Do not assume nothing is paused: a command that hangs without output may be.{}",
            notice.suffix()
        )),
    }
}

fn relief_text(status: &ReliefStatus, g: &Gathered) -> Option<String> {
    let is_mine = |tree: &relief_status::FrozenTree| {
        match (&tree.terminal_session_id, &g.terminal_session) {
            // Both sides know their terminal: it decides, and a different one
            // is a different session, whatever directory it works in.
            (Some(theirs), Some(ours)) => theirs == ours,
            // One side does not say: the worktree or checkout root the call
            // runs in is the only other evidence, and only an exact match is.
            // A sibling worktree under the same repository is not this one.
            _ => g
                .session_root
                .as_deref()
                .is_some_and(|root| same_root(root, Path::new(&tree.repo_path))),
        }
    };
    // A tree the supervisor ended in this Severe episode: the build is gone,
    // and the agent must not start it again into the same shortage. Only a
    // status that is Fresh gets here, and the supervisor forgets what it ended
    // when the episode does, so this says nothing once pressure has cleared.
    if let Some(tree) = status.ended.iter().rev().find(|tree| is_mine(tree)) {
        let command = clip(
            &crate::alerts::text::sanitize(&tree.command),
            MAX_ECHOED_COMMAND,
        );
        return Some(format!(
            "GitPulse's memory protection ended `{command}` (pid {}) because {}. \
             The build was ended by memory protection: it did not finish, and it must not be re-run (or another build started in parallel) until memory pressure clears.",
            tree.pid,
            status.binding.map_or("memory is nearly exhausted", relief_status::ReliefBinding::phrase)
        ));
    }
    let mine = status.paused().find(|tree| is_mine(tree));
    if let Some(tree) = mine {
        let command = clip(
            &crate::alerts::text::sanitize(&tree.command),
            MAX_ECHOED_COMMAND,
        );
        return Some(format!(
            "GitPulse's memory protection paused `{command}` (pid {}) because {}. \
             It will resume automatically. Do not retry it, and do not re-run it or another build in parallel; wait for it to resume.",
            tree.pid,
            status.binding.map_or("memory is nearly exhausted", relief_status::ReliefBinding::phrase)
        ));
    }
    // `slowed` counts processes really slowed, so Advise mode (0) says nothing.
    if status.slowed > 0 {
        return Some(format!(
            "GitPulse's memory protection has lowered the priority of {} agent process(es) because {}, so builds and tests run slower. Run one at a time.",
            status.slowed,
            status.binding.map_or("memory is nearly exhausted", relief_status::ReliefBinding::phrase)
        ));
    }
    None
}

/// Whether two paths name the same directory: the same path, or the same
/// directory once symlinks are resolved (a checkout reached through one).
fn same_root(a: &Path, b: &Path) -> bool {
    if b.as_os_str().is_empty() {
        return false;
    }
    a == b
        || matches!(
            (a.canonicalize(), b.canonicalize()),
            (Ok(a), Ok(b)) if a == b
        )
}

/// The first `max` characters of `text`, with an ellipsis when cut.
fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut cut: String = text.chars().take(max).collect();
    cut.push('…');
    cut
}

fn judge_load(
    input: &HookInput,
    classified: &Classified,
    g: &Gathered,
    started: Option<std::time::Instant>,
    pace: impl FnOnce(Duration),
) -> HookOutput {
    if g.exempt {
        return HookOutput::silent();
    }
    let steward = g.steward_throttle.as_ref();
    if g.mode == HostLoadHookMode::Off && steward.is_none() {
        return HookOutput::silent();
    }
    let Some(heavy) = classified.heavy.as_ref() else {
        let base = if g.mode == HostLoadHookMode::Off {
            HookOutput::silent()
        } else if classified.unreadable {
            let why = "it could not read the command line (an unterminated quote, or longer than it reads)";
            match first_notice(g, input, why) {
                Notice::Repeat => HookOutput::silent(),
                notice => advice(&format!(
                    "GitPulse did NOT load-check this command: {why}.{}",
                    notice.suffix()
                )),
            }
        } else {
            advise_rows(classified, &g.availability)
        };
        return match steward {
            Some(throttle) => {
                let delay = pace_duration_for(throttle.pace_ms, started);
                pace(delay);
                fold(base, steward_pace_advice(throttle, delay))
            }
            None => base,
        };
    };
    let mut enforced;
    let effective_g = if steward.is_some() && g.mode != HostLoadHookMode::Enforce {
        enforced = g.clone();
        enforced.mode = HostLoadHookMode::Enforce;
        &enforced
    } else {
        g
    };
    let disk = disk_check(input, classified, heavy, effective_g);
    if let DiskCall::Refuse(output) = disk {
        return output;
    }
    let verdict_answer = verdict_answer(input, classified, heavy, effective_g);
    let answer = match disk {
        DiskCall::Say(text) => fold(verdict_answer, advice(&text)),
        _ => verdict_answer,
    };
    if let Some(throttle) = steward {
        if !refuses(&answer) {
            let is_disk = matches!(throttle.binding.as_str(), "low_disk" | "disk_filling");
            let why = format!(
                "Manvi system steward critical slowdown is active ({} load: {})",
                throttle.tier, throttle.reason
            );
            return fold(answer, deny(input, heavy, &why, is_disk, effective_g));
        }
    }
    answer
}

/// What the disk checks make of a heavy command.
enum DiskCall {
    Nothing,
    /// Said as context, refusing nothing.
    Say(String),
    Refuse(HookOutput),
}

fn gib(bytes: u64) -> String {
    format!("{:.1} GiB", bytes as f64 / (1u64 << 30) as f64)
}

/// Whether the free space on the volume allows a heavy command to start: a
/// coverage build needs its headroom and any heavy build the severe floor.
///
/// Both master switches bind it as they bind the load check: with measurement
/// off it says nothing, and with launch refusal off it advises and never
/// refuses, whatever each rule's own mode says.
fn disk_check(input: &HookInput, classified: &Classified, heavy: &Hit, g: &Gathered) -> DiskCall {
    let d = &g.disk;
    if !d.measuring {
        return DiskCall::Nothing;
    }
    let covers = classified.coverage_build && d.coverage_mode != InputMode::Off;
    let floors = d.floor_mode != InputMode::Off;
    if !covers && !floors {
        return DiskCall::Nothing;
    }
    let refuses = |mode: InputMode| d.refusing && mode == InputMode::Refuse;
    let room = room(d.free_bytes, classified, g);
    let unread = || {
        let why = "free disk space could not be read";
        match first_notice(g, input, why) {
            Notice::Repeat => DiskCall::Nothing,
            notice => DiskCall::Say(format!(
                "GitPulse did NOT check free disk space for this heavy command ({}): it could not be read for every place it writes to. \
                 A full disk can hang this Mac; check df before a build that writes a lot.{}",
                heavy.label,
                notice.suffix()
            )),
        }
    };
    let Some(free) = room.lowest else {
        return unread();
    };
    let mut reasons = Vec::new();
    let mut refuse = false;
    if covers && free < d.coverage_headroom_bytes {
        reasons.push(format!(
            "a coverage build writes tens of GiB of instrumented output and only {} is free, under the {} GitPulse requires before one starts",
            gib(free),
            gib(d.coverage_headroom_bytes)
        ));
        refuse |= refuses(d.coverage_mode);
    }
    if floors && free < d.floor_bytes {
        reasons.push(format!(
            "free disk space is {}, below GitPulse's {} floor for starting a heavy build",
            gib(free),
            gib(d.floor_bytes)
        ));
        refuse |= refuses(d.floor_mode);
    }
    if reasons.is_empty() {
        // "Has room" is said only when every location was read.
        return if room.complete {
            DiskCall::Nothing
        } else {
            unread()
        };
    }
    let why = reasons.join("; and ");
    if refuse {
        return DiskCall::Refuse(deny(input, heavy, &why, true, g));
    }
    DiskCall::Say(format!(
        "Free disk space is low for this heavy command ({}): {why}. Free disk space first (remove build output such as cargo target directories, or run cargo llvm-cov clean).",
        heavy.label
    ))
}

/// What the app's published verdict says about a heavy command.
fn verdict_answer(
    input: &HookInput,
    classified: &Classified,
    heavy: &Hit,
    g: &Gathered,
) -> HookOutput {
    match &g.availability {
        Availability::Unavailable(why) => {
            let mut lines = lease_lines(input, heavy, &g.availability, g);
            match first_notice(g, input, why) {
                Notice::Repeat => {}
                notice => lines.insert(
                    0,
                    format!(
                        "GitPulse did NOT load-check this heavy command ({}): {why}. The host may be \
                         overloaded; run it once, not alongside other builds{}.{}",
                        heavy.label,
                        cap_sentence(heavy),
                        notice.suffix()
                    ),
                ),
            }
            if lines.is_empty() {
                HookOutput::silent()
            } else {
                advice(&lines.join("\n"))
            }
        }
        Availability::Verdict {
            verdict,
            state,
            age_ms,
        } if !verdict.admit => refusal(input, heavy, verdict, *state, *age_ms, g),
        Availability::Verdict { verdict, .. } if verdict.tier >= Tier::Elevated => {
            let mut lines = vec![format!(
                "Host load is {} ({}). {} is heavy: run it once, not alongside other builds{}.",
                tier_word(verdict.tier),
                binding_phrase(verdict.binding),
                heavy.label,
                cap_sentence(heavy)
            )];
            lines.extend(row_advice(classified));
            lines.extend(lease_lines(input, heavy, &g.availability, g));
            advice(&lines.join("\n"))
        }
        Availability::Verdict { .. } => {
            let lines = lease_lines(input, heavy, &g.availability, g);
            if lines.is_empty() {
                HookOutput::silent()
            } else {
                advice(&lines.join("\n"))
            }
        }
    }
}

/// The advise-only rows, when the host is already under load.
fn advise_rows(classified: &Classified, availability: &Availability) -> HookOutput {
    match availability {
        Availability::Verdict { verdict, .. } if verdict.tier >= Tier::Elevated => {
            let lines = row_advice(classified);
            if lines.is_empty() {
                HookOutput::silent()
            } else {
                advice(&lines.join("\n"))
            }
        }
        _ => HookOutput::silent(),
    }
}

fn row_advice(classified: &Classified) -> Vec<String> {
    classified
        .advise
        .iter()
        .map(|hit| format!("{} ({}): {}", hit.label, hit.row.id, hit.row.advice))
        .collect()
}

fn refusal(
    input: &HookInput,
    heavy: &Hit,
    verdict: &PublishedVerdict,
    state: SensorState,
    age_ms: Option<u64>,
    g: &Gathered,
) -> HookOutput {
    let stale = match (state, age_ms) {
        (SensorState::StaleRecent, Some(age)) => format!(" (the reading is {}s old)", age / 1000),
        _ => String::new(),
    };
    let why = format!(
        "the host is under {} load: {}{stale}",
        tier_word(verdict.tier),
        binding_phrase(verdict.binding)
    );
    let disk = matches!(
        verdict.binding,
        Some(Binding::LowDisk | Binding::DiskFilling)
    );
    deny(input, heavy, &why, disk, g)
}

/// The refusal (or, in Advise, the same words as context) for a heavy command,
/// for `why`. `disk` says the cause is disk space, which changes what the model
/// is told to do: waiting does not free disk, deleting build output does.
fn deny(input: &HookInput, heavy: &Hit, why: &str, disk: bool, g: &Gathered) -> HookOutput {
    if g.mode == HostLoadHookMode::Advise {
        return advice(&format!(
            "GitPulse would refuse this heavy command ({}) because {why}, but the command gate \
             is set to Advise, so it is not refused. Wait and run it later rather than now{}.",
            heavy.label,
            cap_sentence(heavy)
        ));
    }
    let repeats = deny_count(g, input);
    // Under a steward slowdown the gate is enforced whatever its own setting,
    // so the switch that lifts the refusal is the steward's, not the gate's.
    let user_switch = if g.steward_throttle.is_some() {
        "switch off \"Slow agents when host load is critical\" in GitPulse Settings > Agents > Manvi system steward"
    } else {
        "set the command gate to Advise or Off in GitPulse Settings > Agents > Host load"
    };
    let (reason, notice) = match repeats {
        Some(n) if n >= LOOP_AT => (
            format!(
                "STOP: you are looping. GitPulse has refused this same heavy command ({}) {n} times in the last {} minutes because {why}. \
                 Do not run it again, and do not run any other build or test command, until {}. \
                 Tell the user {} and ask them to {} or {user_switch}. \
                 This grants nothing.",
                heavy.label,
                LOOP_WINDOW.as_secs() / 60,
                if disk { "disk space has been freed" } else { "the load has cleared" },
                if disk { "the disk is nearly full" } else { "the host is overloaded" },
                if disk { "free disk space" } else { "free memory" },
            ),
            Some(format!(
                "GitPulse refused the same heavy command {n} times in {} minutes in one session: the agent appears to be retrying in a loop while the host is overloaded.",
                LOOP_WINDOW.as_secs() / 60
            )),
        ),
        _ => (
            format!(
                "GitPulse refused this heavy command ({}) because {why}. \
                 What to do: {}. Do not retry in a loop, and do not work around this by running the same work through another build tool, a wrapper script or a subshell{}. \
                 If the work cannot wait, tell the user: they can {user_switch}.",
                heavy.label,
                if disk {
                    "free disk space first (remove build output such as cargo target directories, or run cargo llvm-cov clean), then run it again; waiting alone will not free any"
                } else {
                    "wait a few minutes, then run it again"
                },
                cap_sentence(heavy)
            ),
            None,
        ),
    };
    HookOutput {
        hook_specific_output: Some(decision(
            PRE_TOOL_USE,
            PermissionDecision::Deny,
            bounded(reason),
        )),
        system_message: notice,
    }
}

/// How many distinct tool calls of this session this command has been refused
/// for in [`LOOP_WINDOW`], this one included, or `None` when it cannot be known
/// (which is a plain refusal, never an allow).
fn deny_count(g: &Gathered, input: &HookInput) -> Option<usize> {
    count_denials(
        &g.state_dir,
        &input.session_id,
        &input.tool_use_id,
        &input.command,
        g.now,
    )
}

/// Whether a "did NOT load-check" notice has been said already.
enum Notice {
    /// Said before in this session for this reason: say nothing now.
    Repeat,
    /// Said now, and will not be again in this session.
    Once,
    /// Said now, and may be said again: there is no session to count it
    /// against, or the marker could not be kept. Never silence.
    Always,
}

impl Notice {
    fn suffix(&self) -> &'static str {
        match self {
            Self::Once => " This will not repeat for this session unless the reason changes.",
            Self::Repeat | Self::Always => "",
        }
    }
}

/// An exclusive marker per (session, reason) in the same private directory as
/// the loop breaker's, so a notice that cannot change is said once rather than
/// on every heavy command. Every way of not knowing (no session id, a directory
/// that cannot be made private, a marker that cannot be written) says it
/// again: a repeat is the safe direction to be wrong in, and silence is the
/// failure this notice exists to prevent.
fn first_notice(g: &Gathered, input: &HookInput, reason: &str) -> Notice {
    if !plain_token(&input.session_id) || private_dir(&g.state_dir).is_err() {
        return Notice::Always;
    }
    let name = format!("loadnote-{}-{:016x}", input.session_id, fnv1a(reason));
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(g.state_dir.join(name))
    {
        Ok(_) => {
            prune(&g.state_dir, g.now);
            Notice::Once
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Notice::Repeat,
        Err(_) => Notice::Always,
    }
}

fn plain_token(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 128
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// One marker per refused tool call, created exclusively, in the same private
/// directory the advisory dedupe uses. Counting markers rather than rewriting a
/// counter means two copies of the hook answering one call (the plugin's and a
/// settings entry's) leave one marker, and a marker nobody can corrupt: there
/// is nothing in it to read.
pub(super) fn count_denials(
    dir: &Path,
    session: &str,
    tool_use_id: &str,
    command: &str,
    now: SystemTime,
) -> Option<usize> {
    if !plain_token(session) || !plain_token(tool_use_id) {
        return None;
    }
    private_dir(dir).ok()?;
    let key = format!("loopdeny-{session}-{:016x}-", fnv1a(command.trim()));
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dir.join(format!("{key}{tool_use_id}")))
    {
        Ok(_) => prune(dir, now),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => return None,
    }
    let entries = std::fs::read_dir(dir).ok()?;
    let mut count = 0;
    for entry in entries.take(PRUNE_SCAN).flatten() {
        if !entry.file_name().to_string_lossy().starts_with(&key) {
            continue;
        }
        // A symlink or a directory under a marker's name is not a marker.
        let Ok(meta) = std::fs::symlink_metadata(entry.path()) else {
            continue;
        };
        // A marker stamped a moment after `now` (the clock moved, or `now`
        // was taken before the file was made) is this call's own; one stamped
        // far ahead is not a marker.
        let recent =
            meta.modified()
                .ok()
                .is_some_and(|modified| match now.duration_since(modified) {
                    Ok(age) => age <= LOOP_WINDOW,
                    Err(ahead) => ahead.duration() <= CLOCK_SKEW,
                });
        if meta.is_file() && recent {
            count += 1;
        }
    }
    Some(count)
}

/// What a person reads for each tier.
fn tier_word(tier: Tier) -> &'static str {
    match tier {
        Tier::Clear => "no",
        Tier::Elevated => "elevated",
        Tier::Severe => "severe",
    }
}

/// The resource that bound the verdict, in words: [`Binding::cause_phrase`],
/// the one owner of how a cause is worded.
fn binding_phrase(binding: Option<Binding>) -> &'static str {
    binding.map_or("no single resource is named", Binding::cause_phrase)
}

fn cap_sentence(hit: &Hit) -> String {
    hit.row
        .cap
        .map(|cap| format!(". Capped form: {cap}"))
        .unwrap_or_default()
}

fn bounded(mut text: String) -> String {
    if text.chars().count() > MAX_TEXT {
        text = text.chars().take(MAX_TEXT).collect();
    }
    text
}

/// Context for the model with no decision attached.
fn advice(text: &str) -> HookOutput {
    HookOutput {
        hook_specific_output: Some(HookSpecificOutput {
            hook_event_name: PRE_TOOL_USE.to_string(),
            permission_decision: None,
            permission_decision_reason: None,
            additional_context: Some(bounded(format!("GitPulse host load: {text}"))),
        }),
        system_message: None,
    }
}

fn refuses(output: &HookOutput) -> bool {
    super::signal_guard::refuses(output)
}

fn context_of(output: &HookOutput) -> Option<String> {
    output
        .hook_specific_output
        .as_ref()
        .and_then(|specific| specific.additional_context.clone())
}

fn joined(a: Option<String>, b: Option<String>) -> Option<String> {
    match (a, b) {
        (Some(a), Some(b)) => Some(format!("{a}\n{b}")),
        (a, b) => a.or(b),
    }
}

/// `rest` (the guard and the gate, already combined) with the load stage's
/// answer folded in last.
///
/// A refusal here wins over a question and a notice, and keeps a refusal from
/// the gate in its reason; advice never replaces a decision, it joins the
/// context. `signal_guard::combine` keeps its first argument's decision whenever
/// that has one, so an advice-only output must not be passed to it first: this
/// is why the load answer is computed before the gate and folded in after it.
pub(super) fn fold(rest: HookOutput, load: HookOutput) -> HookOutput {
    if load.is_silent() {
        return rest;
    }
    if rest.is_silent() {
        return load;
    }
    let message = joined(rest.system_message.clone(), load.system_message.clone());
    let context = joined(context_of(&rest), context_of(&load));
    if refuses(&load) {
        let own = load
            .hook_specific_output
            .as_ref()
            .and_then(|specific| specific.permission_decision_reason.clone())
            .unwrap_or_default();
        let reason = match rest
            .hook_specific_output
            .as_ref()
            .filter(|_| refuses(&rest))
            .and_then(|specific| specific.permission_decision_reason.clone())
        {
            Some(other) => format!("{own}\nAlso refused: {other}"),
            None => own,
        };
        return HookOutput {
            hook_specific_output: Some(HookSpecificOutput {
                hook_event_name: PRE_TOOL_USE.to_string(),
                permission_decision: Some(PermissionDecision::Deny),
                permission_decision_reason: Some(reason),
                additional_context: context,
            }),
            system_message: message,
        };
    }
    let mut chosen = rest;
    chosen.system_message = message;
    let specific = chosen
        .hook_specific_output
        .get_or_insert_with(|| HookSpecificOutput {
            hook_event_name: PRE_TOOL_USE.to_string(),
            ..Default::default()
        });
    specific.additional_context = context;
    chosen
}

#[cfg(test)]
#[path = "load_gate_tests.rs"]
mod tests;
