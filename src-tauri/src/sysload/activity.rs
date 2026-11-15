//! Which agent sessions are idle, and whether one may be parked.
//!
//! # Why not SIGSTOP
//!
//! Stopping an idle agent's process frees no memory and saves no CPU it was
//! not already saving, breaks the model API stream and the MCP servers'
//! stdio it holds open, and can freeze it while it holds a build slot or a
//! git `index.lock`. So an idle session is **parked**: its terminal is closed
//! and its conversation stays resumable (`claude --resume <id>`), which does
//! give its memory back.
//!
//! # What idle means
//!
//! Every signal below must be read and must agree. A session is [`SessionActivity::Idle`]
//! only when
//!
//! * the agent has been waiting on the person (its hook reported it needs
//!   them, or finished) for at least `idle_after`,
//! * its tree is using under [`CPU_IDLE_MAX`] of one core, and that was read,
//! * where GitPulse holds its terminal, the terminal has been quiet for at
//!   least `idle_after`, and
//! * it neither holds nor waits for a build slot.
//!
//! A required signal that could not be read makes it [`SessionActivity::Unknown`],
//! never idle and never busy: a terminal that has been quiet proves nothing
//! on its own (an agent thinks silently), and an agent that reports no state
//! (Codex, a session without GitPulse's hooks) cannot be called idle.
//!
//! # Parking is a click, never a rule
//!
//! Only a session GitPulse launched, whose conversation id is known and
//! resumable, can be parked; an observed session never can. [`park`] asks a
//! native dialog the page cannot click, checks again after the answer, and is
//! not reachable from the agent server.

use serde::Serialize;

use super::{AgentKind, Reading, Why};
use crate::alerts::bridge::{self, Need};

/// A full core is 1,000,000,000 CPU nanoseconds per second; a tree under 5 % of
/// one is not working.
pub const CPU_IDLE_MAX: u64 = 50_000_000;

/// Whether the agent says it is waiting on the person.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Waiting {
    /// For this many milliseconds.
    Since(u64),
    /// It reports no state, and why that is not a state.
    Unknown(String),
}

/// What one session's classification reads.
#[derive(Debug, Clone)]
pub struct Inputs {
    pub waiting: Waiting,
    pub cpu_rate: Reading<u64>,
    /// How long GitPulse's terminal for this session has been quiet. `None`
    /// is a session GitPulse holds no terminal for.
    pub pty_quiet_ms: Option<Reading<u64>>,
    /// The session holds, or waits for, a build slot.
    pub holds_lease: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum SessionActivity {
    Busy { why: String },
    Idle { since_ms: u64, why: String },
    Unknown { why: String },
}

fn why_unreadable(why: Why) -> String {
    match why {
        Why::NotYet => "it has not been measured twice yet".into(),
        Why::Unreadable => "the operating system would not say".into(),
        Why::Unsupported => "this platform cannot say".into(),
        Why::Failed => "reading it failed".into(),
    }
}

/// The one decision. Order matters only for which reason is named first.
pub fn classify(inputs: &Inputs, idle_after_ms: u64) -> SessionActivity {
    if inputs.holds_lease {
        return SessionActivity::Busy {
            why: "it holds or waits for a build slot".into(),
        };
    }
    let waiting_ms = match &inputs.waiting {
        Waiting::Unknown(why) => return SessionActivity::Unknown { why: why.clone() },
        Waiting::Since(ms) => *ms,
    };
    if waiting_ms < idle_after_ms {
        return SessionActivity::Busy {
            why: format!("it has waited only {} s", waiting_ms / 1000),
        };
    }
    match inputs.cpu_rate {
        Reading::Unavailable(why) => {
            return SessionActivity::Unknown {
                why: format!("its CPU use is unknown: {}", why_unreadable(why)),
            }
        }
        Reading::Value(rate) if rate >= CPU_IDLE_MAX => {
            return SessionActivity::Busy {
                why: format!("its processes use {}% of a core", rate / 10_000_000),
            }
        }
        Reading::Value(_) => {}
    }
    let mut since = waiting_ms;
    match inputs.pty_quiet_ms {
        None => {}
        Some(Reading::Unavailable(why)) => {
            return SessionActivity::Unknown {
                why: format!(
                    "its terminal's quiet time is unknown: {}",
                    why_unreadable(why)
                ),
            }
        }
        Some(Reading::Value(quiet)) if quiet < idle_after_ms => {
            return SessionActivity::Busy {
                why: format!("its terminal printed {} s ago", quiet / 1000),
            }
        }
        Some(Reading::Value(quiet)) => since = since.min(quiet),
    }
    SessionActivity::Idle {
        since_ms: since,
        why: "the agent is waiting on you, its processes are quiet and so is its terminal".into(),
    }
}

/// What the agent's last report says about whether it waits: from the hook
/// event GitPulse's notifier holds standing for its terminal session.
pub fn waiting_from_standing(
    kind: AgentKind,
    standing_event: Option<&str>,
    standing_age_ms: u64,
) -> Waiting {
    if !matches!(kind, AgentKind::Claude | AgentKind::Antigravity) {
        return Waiting::Unknown(format!(
            "{} reports no waiting state to GitPulse",
            kind.name()
        ));
    }
    match standing_event.and_then(bridge::event).map(|e| e.need) {
        Some(Need::Ask | Need::Finished) => Waiting::Since(standing_age_ms),
        Some(Need::Error | Need::Clear) | None => Waiting::Unknown(
            "it has made no waiting report: it is working, or its hooks report nothing".into(),
        ),
    }
}

/// Whether a session may be parked, and the reason it may not. Never
/// "parkable" for an observed session or one with no resumable conversation.
pub fn park_blocker(
    kind: AgentKind,
    terminal_session: Option<&str>,
    conversation: Option<(&str, &str)>,
    cwd: Option<&str>,
) -> Option<String> {
    if terminal_session.is_none() {
        return Some("GitPulse did not launch it, so it will not close it".into());
    }
    let provider = match kind {
        AgentKind::Claude => "claude",
        AgentKind::Antigravity => "agy",
        _ => {
            return Some(format!(
                "{} sessions cannot be resumed from GitPulse",
                kind.name()
            ))
        }
    };
    let Some((agent, conversation_id)) = conversation else {
        return Some(
            "its conversation id has not been reported, so it could not be resumed".into(),
        );
    };
    if agent != provider {
        return Some("the conversation reported for it belongs to another agent".into());
    }
    crate::workbench::terminal_command::resume(
        provider,
        conversation_id,
        std::path::Path::new(cwd.unwrap_or("")),
    )
    .err()
    .map(|why| format!("it could not be resumed: {why}"))
}

/// One agent session in the report.
#[derive(Debug, Clone, Serialize)]
pub struct IdleSession {
    pub terminal_session: Option<String>,
    pub kind: AgentKind,
    pub pid: i32,
    pub start_us: u64,
    pub cwd: Option<String>,
    pub footprint: Reading<u64>,
    pub activity: SessionActivity,
    /// Why this session cannot be parked, or `None` when it can be (it is
    /// also idle: parking an unknown or busy session is refused again at the
    /// click).
    pub park_blocked: Option<String>,
}

/// What Health shows about idle sessions.
#[derive(Debug, Clone, Serialize)]
pub struct IdleReport {
    /// Largest footprint first; idle sessions before the rest.
    pub sessions: Vec<IdleSession>,
    pub idle: u32,
    pub unknown: u32,
    pub idle_after_s: u64,
    pub park_after_s: u64,
    /// Why the process scan could not be read, when it could not.
    pub error: Option<String>,
}

/// Everything about one candidate session the report reads outside the scan.
#[derive(Debug, Clone, Default)]
pub struct Facts {
    pub standing_event: Option<String>,
    pub standing_age_ms: u64,
    /// The terminal's quiet time, when GitPulse holds the terminal.
    pub pty_quiet_ms: Option<Option<u64>>,
    pub conversation: Option<(String, String)>,
    pub holds_lease: bool,
    /// The build-slot state could not be read, so "holds no slot" is unknown.
    pub lease_unreadable: bool,
}

/// The report over a scan, with `facts_of` supplying the rest per session.
pub fn build(
    sessions: &[super::AgentSession],
    mut facts_of: impl FnMut(&super::AgentSession) -> Facts,
    idle_after_s: u64,
    park_after_s: u64,
) -> IdleReport {
    let mut rows: Vec<IdleSession> = sessions
        .iter()
        .map(|s| {
            let facts = facts_of(s);
            let terminal = s.terminal_session_id.clone();
            let waiting = if terminal.is_some() {
                waiting_from_standing(
                    s.kind,
                    facts.standing_event.as_deref(),
                    facts.standing_age_ms,
                )
            } else {
                Waiting::Unknown(
                    "GitPulse does not hold its terminal, so it cannot tell how long it has waited"
                        .into(),
                )
            };
            let activity = classify(
                &Inputs {
                    waiting,
                    cpu_rate: s.cpu_rate,
                    pty_quiet_ms: match facts.pty_quiet_ms {
                        None => None,
                        Some(Some(ms)) => Some(Reading::Value(ms)),
                        Some(None) => Some(Reading::Unavailable(Why::Unreadable)),
                    },
                    holds_lease: facts.holds_lease,
                },
                idle_after_s.saturating_mul(1000),
            );
            // A session that may be building cannot be called idle when the
            // slot state is unreadable.
            let activity = match activity {
                SessionActivity::Idle { .. } if facts.lease_unreadable => {
                    SessionActivity::Unknown {
                        why: "whether it holds a build slot could not be read".into(),
                    }
                }
                other => other,
            };
            let park_blocked = park_blocker(
                s.kind,
                terminal.as_deref(),
                facts
                    .conversation
                    .as_ref()
                    .map(|(a, c)| (a.as_str(), c.as_str())),
                s.cwd.as_deref(),
            );
            IdleSession {
                terminal_session: terminal,
                kind: s.kind,
                pid: s.pid,
                start_us: s.start_us,
                cwd: s.cwd.clone(),
                footprint: s.footprint,
                activity,
                park_blocked,
            }
        })
        .collect();
    let rank = |r: &IdleSession| matches!(r.activity, SessionActivity::Idle { .. });
    rows.sort_by_key(|r| {
        (
            std::cmp::Reverse(rank(r)),
            std::cmp::Reverse(match r.footprint {
                Reading::Value(bytes) => bytes,
                Reading::Unavailable(_) => 0,
            }),
        )
    });
    IdleReport {
        idle: u32::try_from(rows.iter().filter(|r| rank(r)).count()).unwrap_or(u32::MAX),
        unknown: u32::try_from(
            rows.iter()
                .filter(|r| matches!(r.activity, SessionActivity::Unknown { .. }))
                .count(),
        )
        .unwrap_or(u32::MAX),
        sessions: rows,
        idle_after_s,
        park_after_s,
        error: None,
    }
}

/// The decision behind a park, separated from the OS so it is tested with
/// closures: check, ask, check again, then act — and on any refusal or
/// decline, do nothing. `check` re-reads the live session and says what it is
/// now; only a session that is idle and parkable passes.
pub fn park(
    check: impl Fn() -> Result<IdleSession, String>,
    confirm: &dyn Fn(&str, &str) -> bool,
    act: impl FnOnce(&IdleSession) -> Result<String, String>,
) -> Result<String, String> {
    let ready = |seen: &IdleSession| -> Result<(), String> {
        if let Some(why) = &seen.park_blocked {
            return Err(format!("this session cannot be parked: {why}"));
        }
        match &seen.activity {
            SessionActivity::Idle { .. } => Ok(()),
            SessionActivity::Busy { why } => Err(format!("this session is not idle: {why}")),
            SessionActivity::Unknown { why } => Err(format!("it is not known to be idle: {why}")),
        }
    };
    let seen = check()?;
    ready(&seen)?;
    let where_ = seen.cwd.as_deref().unwrap_or("an unknown folder");
    let body = format!(
        "Park this {} session in {where_}?\n\nIts terminal closes and its memory is freed. Its conversation is kept, and GitPulse offers to reopen it with its conversation resumed.",
        seen.kind.name()
    );
    if !confirm("Park idle agent session", &body) {
        return Err("parking was not confirmed; nothing was closed".into());
    }
    let now = check()?;
    ready(&now)?;
    act(&now)
}

/// The report for this app's terminals: the sampler's last scan, the
/// notifier's standing reports, each terminal's quiet time, the conversation
/// ids the agents' hooks reported, and the build slots.
pub fn report(state: &crate::terminal::TerminalSessions) -> IdleReport {
    let settings = crate::tool_config::host_load_idle();
    let procs = super::status().procs;
    let (sessions, error) = match procs {
        Some(view) if view.ok => (view.sessions, None),
        Some(view) => (
            Vec::new(),
            Some(
                view.error
                    .unwrap_or_else(|| "the process scan failed".into()),
            ),
        ),
        None => (
            Vec::new(),
            Some("this process does not run the sampler, so there is no session scan".into()),
        ),
    };
    let listing = crate::terminal::list_sessions(state);
    let standing = crate::alerts::standing();
    let (held, lease_unreadable) = lease_sessions();
    let mut report = build(
        &sessions,
        |s| {
            let Some(id) = s.terminal_session_id.as_deref() else {
                return Facts::default();
            };
            let stand = standing.iter().find(|a| a.session == id);
            Facts {
                standing_event: stand.and_then(|a| a.event.clone()),
                standing_age_ms: stand.map_or(0, |a| a.age_ms),
                pty_quiet_ms: Some(match &listing {
                    Ok(listed) => listed.iter().find(|l| l.id == id).and_then(|l| l.quiet_ms),
                    Err(_) => None,
                }),
                conversation: state
                    .agent_meta_for(id)
                    .map(|m| (m.agent, m.conversation_id)),
                holds_lease: held.iter().any(|h| h == id),
                lease_unreadable,
            }
        },
        settings.idle_after_s,
        settings.park_after_s,
    );
    report.error = error;
    report
}

/// The terminal sessions holding or waiting for a build slot, and whether the
/// slot state could not be read.
#[cfg(unix)]
fn lease_sessions() -> (Vec<String>, bool) {
    let Some(config) = crate::tool_config::default_config_dir() else {
        return (Vec::new(), true);
    };
    let dir = match super::lease::Dir::open(&super::lease::lease_dir(&config)) {
        Ok(dir) => dir,
        // No lease directory yet: nobody has ever taken a slot.
        Err(_) if !super::lease::lease_dir(&config).exists() => return (Vec::new(), false),
        Err(_) => return (Vec::new(), true),
    };
    let board = dir.board(&super::lease::Own::default());
    let sessions = board
        .holders
        .iter()
        .filter_map(|h| h.record.as_ref())
        .chain(board.waiters.iter().map(|w| &w.record))
        .filter_map(|r| r.terminal_session.clone())
        .collect();
    // A holder with no record cannot be attributed to a session.
    let unnamed = board.holders.iter().any(|h| h.record.is_none());
    (sessions, !board.unreadable.is_empty() || unnamed)
}

#[cfg(not(unix))]
fn lease_sessions() -> (Vec<String>, bool) {
    (Vec::new(), false)
}

/// Parks one session: the check, the native dialog, the check again, then the
/// terminal is closed, its row is marked for reopening, and the ledger says so.
/// Only ever called from a click (`cmd_host_load_park`).
pub fn park_session(
    state: &crate::terminal::TerminalSessions,
    session_id: &str,
    confirm: &dyn Fn(&str, &str) -> bool,
) -> Result<String, String> {
    park(
        || {
            report(state)
                .sessions
                .into_iter()
                .find(|s| s.terminal_session.as_deref() == Some(session_id))
                .ok_or_else(|| "that session is not running any more".to_string())
        },
        confirm,
        |seen| {
            let repo = crate::terminal::list_sessions(state)
                .ok()
                .and_then(|l| l.into_iter().find(|s| s.id == session_id))
                .map(|s| s.repo);
            crate::terminal::kill_session(state, session_id)?;
            let ledger = state.session_ledger();
            let marked = ledger.mark_parked(session_id);
            let recorded = repo.is_some_and(|repo| {
                crate::ledger::record(crate::ledger::Draft {
                    repo_path: repo,
                    worktree_path: seen.cwd.clone(),
                    actor_kind: Some(crate::ledger::ActorKind::Human),
                    session_id: Some(session_id.to_string()),
                    action: "host_load.park".to_string(),
                    object: Some(session_id.to_string()),
                    outcome: Some(crate::ledger::Outcome::Ok),
                    ..Default::default()
                })
                .is_some()
            });
            Ok(format!(
                "Parked {} in {}. {}{}",
                seen.kind.name(),
                seen.cwd.as_deref().unwrap_or("an unknown folder"),
                if marked {
                    "It is offered for reopening with its conversation resumed."
                } else {
                    "It was closed, but GitPulse has no record of the tab to offer it back from: resume it by hand."
                },
                if recorded {
                    ""
                } else {
                    " The park was not written to the ledger."
                }
            ))
        },
    )
}

#[cfg(test)]
#[path = "activity_tests.rs"]
mod tests;
