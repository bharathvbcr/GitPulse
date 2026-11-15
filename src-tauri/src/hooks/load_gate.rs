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
    Binding, PublishedVerdict, Reading, SensorReport, SensorState, Snapshot, Tier,
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
pub(super) fn gather_live() -> Gathered {
    let mode = crate::tool_config::host_load_hook_mode();
    let live = |availability, exempt| Gathered {
        mode,
        exempt,
        availability,
        state_dir: marker_dir(),
        now: SystemTime::now(),
    };
    if mode == HostLoadHookMode::Off {
        return live(Availability::Unavailable(String::new()), false);
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
    live(availability, exempt)
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
            return advice(
                "GitPulse did NOT load-check this command: it could not read the command line \
                 (an unterminated quote, or longer than it reads).",
            );
        }
        return advise_rows(classified, &g.availability);
    };
    match &g.availability {
        Availability::Unavailable(why) => advice(&format!(
            "GitPulse did NOT load-check this heavy command ({}): {why}. The host may be \
             overloaded; run it once, not alongside other builds{}.",
            heavy.label,
            cap_sentence(heavy)
        )),
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
                 Do not run it again, and do not run any other build or test command, until the load has cleared. \
                 Tell the user the host is overloaded and ask them to free memory or change the command gate in GitPulse Settings > Agents > Host load. \
                 This grants nothing.",
                heavy.label,
                LOOP_WINDOW.as_secs() / 60
            ),
            Some(format!(
                "GitPulse refused the same heavy command {n} times in {} minutes in one session: the agent appears to be retrying in a loop while the host is overloaded.",
                LOOP_WINDOW.as_secs() / 60
            )),
        ),
        _ => (
            format!(
                "GitPulse refused this heavy command ({}) because {why}. \
                 What to do: wait a few minutes, then run it again. Do not retry in a loop, and do not work around this by running the same work through another build tool, a wrapper script or a subshell{}. \
                 If the work cannot wait, tell the user: they can set the command gate to Advise or Off in GitPulse Settings > Agents > Host load.",
                heavy.label,
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

/// The resource that bound the verdict, in words. Exhaustive on purpose: a new
/// [`Binding`] must be given words here before it compiles.
fn binding_phrase(binding: Option<Binding>) -> &'static str {
    match binding {
        None => "no single resource is named",
        Some(Binding::MemoryPressure) => "the kernel reports memory pressure",
        Some(Binding::CompressorThrash) => "the memory compressor is thrashing",
        Some(Binding::CompressorSegments) => "the memory compressor is nearly out of segments",
        Some(Binding::Swap) => "swap in use is above GitPulse's limit for it",
        Some(Binding::LowDisk) => "free disk space is below GitPulse's floor",
        Some(Binding::Thermal) => "the Mac is thermally throttled",
        Some(Binding::CpuLoad) => "the CPU load average is above GitPulse's limit for it",
        Some(Binding::Battery) => "the Mac is running on battery",
    }
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
