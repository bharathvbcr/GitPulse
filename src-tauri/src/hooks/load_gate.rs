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
//! ([`PublishedVerdict`]) and nothing else about the host. It never samples the
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
    pub read_free: fn(&Path) -> Option<u64>,
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

/// What the real world says. Called only for a command that is in the table.
pub(super) fn gather_live(cwd: &str) -> Gathered {
    let mode = crate::tool_config::host_load_hook_mode();
    let live = |availability, exempt, disk| Gathered {
        mode,
        exempt,
        availability,
        state_dir: marker_dir(),
        now: SystemTime::now(),
        disk,
        read_free: real_free,
    };
    if mode == HostLoadHookMode::Off {
        return live(
            Availability::Unavailable(String::new()),
            false,
            DiskFacts::from_settings(None),
        );
    }
    let exempt = std::env::var_os("MAC_HEAVY_LABEL").is_some_and(|label| !label.is_empty());
    let availability = match crate::tool_config::default_config_dir() {
        None => {
            Availability::Unavailable("there is no per-user config directory (HOME unset)".into())
        }
        Some(dir) => {
            let path = dir.join(crate::sysload::SNAPSHOT_FILE_NAME);
            let (report, snapshot) = crate::sysload::report(&path, crate::sysload::Clock::now());
            availability_of(&report, snapshot.as_ref())
        }
    };
    live(
        availability,
        exempt,
        DiskFacts::from_settings(live_free_bytes(
            cwd,
            std::env::current_dir().ok(),
            real_free,
        )),
    )
}

/// The real reader of [`Gathered::read_free`].
fn real_free(path: &Path) -> Option<u64> {
    crate::sysload::free_bytes(path).value()
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
    read: impl Fn(&Path) -> Option<u64>,
) -> Option<u64> {
    if cwd.is_empty() {
        return process_dir.and_then(|dir| read(&dir));
    }
    let dir = Path::new(cwd);
    dir.is_absolute().then(|| read(dir)).flatten()
}

/// Free space on a build directory's volume: that of its nearest ancestor
/// that can be read, since the directory itself may not exist yet.
fn free_at_or_above(dir: &Path, read: impl Fn(&Path) -> Option<u64>) -> Option<u64> {
    dir.ancestors().find_map(read)
}

/// The free space a heavy line is judged on: the smallest reading among the
/// volume it runs in and the build directories it names literally. A volume
/// that cannot be read adds nothing; none readable is `None`.
fn effective_free(cwd_free: Option<u64>, classified: &Classified, g: &Gathered) -> Option<u64> {
    classified
        .build_dirs
        .iter()
        .filter_map(|dir| free_at_or_above(Path::new(dir), g.read_free))
        .chain(cwd_free)
        .min()
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

/// The load stage of `command-gate`. Silent for everything that is not in the
/// table, and for `off`; otherwise the refusal or the advice described above.
///
/// `gather` runs only once the command is known to be in the table, so every
/// other call pays for the table lookup and nothing else.
pub(super) fn judge(input: &HookInput, gather: impl FnOnce() -> Gathered) -> HookOutput {
    if input.command.is_empty() {
        return HookOutput::silent();
    }
    let classified = classify(&input.command);
    if classified.is_empty() {
        return HookOutput::silent();
    }
    let gathered = gather();
    judge_classified(input, &classified, &gathered)
}

fn judge_classified(input: &HookInput, classified: &Classified, g: &Gathered) -> HookOutput {
    if g.mode == HostLoadHookMode::Off || g.exempt {
        return HookOutput::silent();
    }
    let Some(heavy) = classified.heavy.as_ref() else {
        if classified.unreadable {
            let why = "it could not read the command line (an unterminated quote, or longer than it reads)";
            return match first_notice(g, input, why) {
                Notice::Repeat => HookOutput::silent(),
                notice => advice(&format!(
                    "GitPulse did NOT load-check this command: {why}.{}",
                    notice.suffix()
                )),
            };
        }
        return advise_rows(classified, &g.availability);
    };
    let disk = disk_check(input, classified, heavy, g);
    if let DiskCall::Refuse(output) = disk {
        return output;
    }
    let verdict_answer = verdict_answer(input, classified, heavy, g);
    match disk {
        DiskCall::Say(text) => fold(verdict_answer, advice(&text)),
        _ => verdict_answer,
    }
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
    let Some(free) = effective_free(d.free_bytes, classified, g) else {
        let why = "free disk space could not be read";
        return match first_notice(g, input, why) {
            Notice::Repeat => DiskCall::Nothing,
            notice => DiskCall::Say(format!(
                "GitPulse did NOT check free disk space for this heavy command ({}): it could not be read. \
                 A full disk can hang this Mac; check df before a build that writes a lot.{}",
                heavy.label,
                notice.suffix()
            )),
        };
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
        return DiskCall::Nothing;
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
        Availability::Unavailable(why) => match first_notice(g, input, why) {
            Notice::Repeat => HookOutput::silent(),
            notice => advice(&format!(
                "GitPulse did NOT load-check this heavy command ({}): {why}. The host may be \
                 overloaded; run it once, not alongside other builds{}.{}",
                heavy.label,
                cap_sentence(heavy),
                notice.suffix()
            )),
        },
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
            advice(&lines.join("\n"))
        }
        Availability::Verdict { .. } => HookOutput::silent(),
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
    let (reason, notice) = match repeats {
        Some(n) if n >= LOOP_AT => (
            format!(
                "STOP: you are looping. GitPulse has refused this same heavy command ({}) {n} times in the last {} minutes because {why}. \
                 Do not run it again, and do not run any other build or test command, until {}. \
                 Tell the user {} and ask them to {} or change the command gate in GitPulse Settings > Agents > Host load. \
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
                 If the work cannot wait, tell the user: they can set the command gate to Advise or Off in GitPulse Settings > Agents > Host load.",
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
