//! `runs.send_to_agent`: a person's confirmed message, put back into the agent
//! that did the work.
//!
//! A CI run fails on the branch an agent pushed, and the agent has walked away
//! or is idle. The person reads the failure in GitPulse and wants the agent to
//! hear it. There are four ways that can happen, and which one applies is a
//! fact about the attempt, not a choice:
//!
//! * **mailbox** — the attempt's Claude Code session is live. The text is left
//!   in its mailbox (`crate::agent_mailbox`) and the `asyncRewake` `Stop` hook
//!   (`gitpulse-hook deliver`) wakes the session with it.
//! * **resume** — the attempt has ended and Claude Code saved its conversation.
//!   The renderer opens the conversation in a new tab with the text as its
//!   first prompt (`resumeTaskConversation`); this host only says it can.
//! * **managed** — a managed run is live. The text goes to Manvi as the run's
//!   next turn (`work.runs.managed.continue`, [`super::managed_run::follow_up_with`]).
//! * **unavailable** — none of the above is true, and why.
//!
//! Two calls, one method. Without `confirm` it sends nothing: it validates the
//! text and says which route a send would take and why, which is what the
//! preview shows. With `confirm` it sends, but only along the route the person
//! was shown (`expect_route`); if the attempt changed in between, it refuses
//! with `route_changed` rather than send somewhere the person did not approve.
//! There is no automatic sending in this version: every send is a person's
//! confirm of text they could read.
//!
//! The answer never rounds a weaker fact up. `queued` is not `delivered`,
//! `picked_up` is not `read`, an old Manvi is `unsupported` and not a failure
//! of the run, and a check that could not run is its own reason.

use super::{conversation, managed_run, query, WorkbenchError, WorkbenchState};
use crate::agent_mailbox::{self, Delivery, Mailbox, MailboxError};
use serde::Deserialize;
use serde_json::{json, Value};
use std::time::SystemTime;

/// Longest request id, matching the mailbox's file-name part.
const MAX_REQUEST_ID: usize = 64;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    /// The attempt (run) the message is for, unless `peer_id` + `repo_path`
    /// address a live peer directly.
    #[serde(default)]
    id: String,
    /// Optional direct peer target in `peers.json` (used with `repo_path`).
    #[serde(default)]
    peer_id: Option<String>,
    /// Checkout path owning `peers.json` when `peer_id` is supplied.
    #[serde(default)]
    repo_path: Option<String>,
    /// Exactly the text the person saw.
    text: String,
    /// Fresh for each press of Confirm; a retry of the same press reuses it.
    request_id: String,
    #[serde(default)]
    confirm: bool,
    /// The route the person was shown. Required with `confirm`.
    #[serde(default)]
    expect_route: Option<String>,
}

/// Which way a message would go.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Route {
    Mailbox {
        session: String,
        listening: bool,
        provider: String,
    },
    /// The conversation can be resumed; carries `conversation::judge`'s answer.
    Resume {
        found: Value,
    },
    Managed {
        /// Manvi's reading of the run: `parked` or `running`.
        state: String,
        turn: i64,
        max_turns: i64,
    },
    Unavailable {
        reason: String,
    },
}

impl Route {
    pub(super) fn name(&self) -> &'static str {
        match self {
            Self::Mailbox { .. } => "mailbox",
            Self::Resume { .. } => "resume",
            Self::Managed { .. } => "managed",
            Self::Unavailable { .. } => "unavailable",
        }
    }

    /// The route, and why, as the preview reads it.
    fn preview(&self) -> Value {
        match self {
            Self::Mailbox {
                session,
                listening,
                provider,
            } => json!({
                "route": "mailbox",
                "session_id": session,
                "listening": listening,
                "reason": if provider == "agy" {
                    "This agent's Antigravity session is live. The message is left in its GitPulse mailbox and injected at its next hook boundary (or when delivered from its terminal banner)."
                } else if *listening {
                    "This agent's Claude Code session is live and a GitPulse hook is waiting on it. The message is handed to the hook and shown to the agent as a reminder."
                } else {
                    "No GitPulse hook is waiting on this agent's Claude Code session right now, and GitPulse cannot see why (it may be mid-turn, idle too long, or running a hook build without deliver). The message is held for two hours and is shown only if a hook on this session picks it up."
                },
            }),
            Self::Resume { found } => {
                let agent = if found["provider"] == "agy" {
                    "Antigravity"
                } else {
                    "Claude Code"
                };
                json!({
                    "route": "resume",
                    "session_id": found["session_id"],
                    "cwd": found["cwd"],
                    "permission_mode": found["permission_mode"],
                    "reason": format!(
                        "This attempt has ended and {agent} saved its conversation. Sending resumes it in a new tab, in the attempt's own permission mode, with this message as its first prompt."
                    ),
                })
            }
            Self::Managed {
                state,
                turn,
                max_turns,
            } => json!({
                "route": "managed",
                "managed_state": state,
                "turn": turn,
                "max_turns": max_turns,
                "reason": if state == "parked" {
                    format!("This managed run is between turns. The message goes to it as its next turn (turn {} of at most {max_turns}).", turn + 1)
                } else {
                    "This managed run is in the middle of a turn. It can take a follow-up once that turn ends; sending now will be refused, and you can send again.".to_owned()
                },
            }),
            Self::Unavailable { reason } => json!({
                "route": "unavailable",
                "reason": reason,
            }),
        }
    }
}

fn unavailable(reason: impl Into<String>) -> Route {
    Route::Unavailable {
        reason: reason.into(),
    }
}

/// Which route a message to this attempt would take.
///
/// `listening` says whether a hook is waiting on a session; `resumable` is
/// `conversation::judge` for the run (called only for an ended Claude or Antigravity attempt);
/// `managed_status` is the status read of a live managed run (called only for
/// one). Each is a closure so a test can answer them and a route that needs
/// none of them asks none: no check is made that the answer would not use.
pub(super) fn decide(
    run: &Value,
    listening: impl Fn(&str) -> bool,
    resumable: impl FnOnce() -> Value,
    managed_status: impl FnOnce() -> Result<Value, WorkbenchError>,
) -> Route {
    let state = run["state"].as_str().unwrap_or_default();
    let live = matches!(state, "starting" | "running");
    let ended = matches!(state, "exited" | "failed" | "cancelled");
    let provider = run["provider"].as_str().unwrap_or_default();
    match (run["kind"].as_str(), live, ended) {
        (Some("managed"), true, _) => match managed_status() {
            Ok(answer) if answer["ok"] == true => match answer["state"].as_str() {
                Some(managed @ ("parked" | "running")) => Route::Managed {
                    state: managed.to_owned(),
                    turn: answer["turn"].as_i64().unwrap_or(0),
                    max_turns: answer["max_turns"].as_i64().unwrap_or(0),
                },
                other => unavailable(format!(
                    "This managed run no longer takes follow-ups (it reports {}).",
                    other.unwrap_or("an unknown state")
                )),
            },
            Ok(answer) => unavailable(
                answer["reason"]
                    .as_str()
                    .unwrap_or("This managed run cannot take a follow-up.")
                    .to_owned(),
            ),
            // A call that got no answer says nothing about the run, and is
            // said as that.
            Err(error) => unavailable(format!(
                "Manvi could not be asked whether this run takes follow-ups, so nothing was checked: {}",
                error.message
            )),
        },
        (Some("managed"), false, true) => resume_or(provider, resumable),
        (Some("external_terminal"), true, _) => {
            if !matches!(provider, "claude" | "agy") {
                return unavailable(
                    "Only a Claude Code or Antigravity session can be sent a message from GitPulse; this agent is not Claude Code or Antigravity.",
                );
            }
            match session_of(run) {
                Some(session) => Route::Mailbox {
                    listening: listening(&session),
                    session,
                    provider: provider.to_owned(),
                },
                None if provider == "agy" => unavailable(
                    "This attempt has no Antigravity conversation id yet, so there is no session to leave a message for.",
                ),
                None => unavailable(
                    "This attempt has no Claude Code session id, so there is no session to leave a message for.",
                ),
            }
        }
        (Some("external_terminal"), false, true) => {
            // A conversation resumed from this attempt keeps the attempt's
            // session id. If a hook is waiting on it, it is live right now and
            // the message belongs there, not in a second resume.
            if matches!(provider, "claude" | "agy") {
                if let Some(session) = session_of(run) {
                    if listening(&session) {
                        return Route::Mailbox {
                            session,
                            listening: true,
                            provider: provider.to_owned(),
                        };
                    }
                }
            }
            resume_or(provider, resumable)
        }
        (_, false, false) => unavailable(match state {
            "prepared" => "This attempt has not started, so there is no agent to send a message to.",
            "unresolved" => "This attempt's outcome is unresolved, so GitPulse cannot say whether an agent is still running to hear a message.",
            _ => "This attempt is not in a state that can take a message.",
        }),
        _ => unavailable("This attempt is not in a state that can take a message."),
    }
}

fn resume_or(provider: &str, resumable: impl FnOnce() -> Value) -> Route {
    if !matches!(provider, "claude" | "agy") {
        return unavailable(
            "This attempt has ended, and only a Claude Code or Antigravity conversation can be resumed from GitPulse.",
        );
    }
    let found = resumable();
    if found["resumable"] == true {
        Route::Resume { found }
    } else {
        unavailable(
            found["reason"]
                .as_str()
                .unwrap_or("This attempt's conversation cannot be resumed.")
                .to_owned(),
        )
    }
}

/// The provider conversation a terminal attempt runs as: `provider_thread_id`
/// once bound (`runs.bind_thread`, used by Antigravity), else for Claude Code
/// the canonical UUID passed as `--session-id` (`run["id"]`), else the peer's
/// bound `provider_session_id` in `peers.json`.
fn session_of(run: &Value) -> Option<String> {
    if let Some(bound) = run["provider_thread_id"]
        .as_str()
        .filter(|id| super::terminal_command::is_canonical_uuid(id))
    {
        return Some(bound.to_ascii_lowercase());
    }
    let provider = run["provider"].as_str().unwrap_or("claude");
    if provider == "claude" {
        if let Some(id) = run["id"]
            .as_str()
            .filter(|id| super::terminal_command::is_canonical_uuid(id))
        {
            return Some(id.to_ascii_lowercase());
        }
    }
    if let (Some(cwd), Some(run_id)) = (run["cwd"].as_str(), run["id"].as_str()) {
        if let Some(peer_sid) = crate::engine::agent_bus::peer_session_id_for_run(cwd, run_id) {
            if super::terminal_command::is_canonical_uuid(&peer_sid) {
                return Some(peer_sid.to_ascii_lowercase());
            }
        }
    }
    None
}

fn invalid(message: impl Into<String>) -> WorkbenchError {
    WorkbenchError::new("invalid_input", message)
}

fn parse(input: &str) -> Result<Request, WorkbenchError> {
    let request: Request = serde_json::from_str(input).map_err(|e| invalid(e.to_string()))?;
    if let Some(ref pid) = request.peer_id {
        let clean = pid.trim();
        if clean.is_empty() || clean.len() > 128 || clean.chars().any(char::is_control) {
            return Err(invalid("Select a valid live peer identity."));
        }
        let repo = request
            .repo_path
            .as_deref()
            .map(str::trim)
            .filter(|p| !p.is_empty() && std::path::Path::new(p).is_absolute())
            .ok_or_else(|| invalid("Addressing a peer directly requires an absolute repo_path."))?;
        crate::repository_trust::require(std::path::Path::new(repo))
            .map_err(|e| WorkbenchError::new("untrusted_repository", e.message()))?;
    } else if !super::is_record_id(&request.id) {
        return Err(invalid("Select a saved attempt."));
    }
    if !super::is_record_id_within(&request.request_id, MAX_REQUEST_ID) {
        return Err(invalid("A request id is 1 to 64 letters, digits, - or _."));
    }
    agent_mailbox::validate_message(&request.text).map_err(|e| invalid(e.to_string()))?;
    if request.confirm && request.expect_route.is_none() {
        return Err(invalid(
            "Confirming a send names the route the person was shown.",
        ));
    }
    Ok(request)
}

pub(super) fn send(state: &WorkbenchState, input: &str) -> Result<Value, WorkbenchError> {
    let mailbox = Mailbox::at_default().map_err(mailbox_error)?;
    send_with(state, input, &mailbox, |op, params| {
        state.worker_call_current(op, params)
    })
}

pub(super) fn send_with(
    state: &WorkbenchState,
    input: &str,
    mailbox: &Mailbox,
    call: impl FnMut(&str, Value) -> Result<Value, WorkbenchError>,
) -> Result<Value, WorkbenchError> {
    let claude_configs = conversation::claude_config_dirs();
    let agy_homes = conversation::antigravity_homes();
    send_with_homes(state, input, mailbox, &claude_configs, &agy_homes, call)
}

pub(super) fn send_with_homes(
    state: &WorkbenchState,
    input: &str,
    mailbox: &Mailbox,
    claude_configs: &conversation::ClaudeConfigDirs,
    agy_homes: &[std::path::PathBuf],
    mut call: impl FnMut(&str, Value) -> Result<Value, WorkbenchError>,
) -> Result<Value, WorkbenchError> {
    let request = parse(input)?;
    let (id, route) = if let (Some(ref peer_target), Some(ref repo_path)) =
        (&request.peer_id, &request.repo_path)
    {
        match crate::engine::agent_bus::resolve_target_peer(repo_path, peer_target) {
            Ok(peer) => {
                if peer.record.surface == "managed" && peer.liveness == "live" {
                    if let Some(run_id) = peer.record.run_id.clone() {
                        let managed = match managed_run::follow_up_with(
                            state,
                            &run_id,
                            None,
                            None,
                            &mut call,
                        ) {
                            Ok(answer) if answer["ok"] == true => match answer["state"].as_str() {
                                Some(st @ ("parked" | "running")) => Route::Managed {
                                    state: st.to_owned(),
                                    turn: answer["turn"].as_i64().unwrap_or(0),
                                    max_turns: answer["max_turns"].as_i64().unwrap_or(0),
                                },
                                other => unavailable(format!(
                                    "This managed run no longer takes follow-ups (it reports {}).",
                                    other.unwrap_or("an unknown state")
                                )),
                            },
                            Ok(answer) => unavailable(
                                answer["reason"]
                                    .as_str()
                                    .unwrap_or("This managed run cannot take a follow-up.")
                                    .to_owned(),
                            ),
                            Err(error) => unavailable(format!(
                                "Manvi could not be asked whether this run takes follow-ups, so nothing was checked: {}",
                                error.message
                            )),
                        };
                        (run_id, managed)
                    } else {
                        (
                            String::new(),
                            unavailable("This managed peer has no associated run id."),
                        )
                    }
                } else if peer.liveness == "live" {
                    let route = match peer.record.provider_session_id.as_deref() {
                        Some(sid)
                            if matches!(peer.record.provider.as_str(), "claude" | "agy")
                                && super::terminal_command::is_canonical_uuid(sid) =>
                        {
                            let session = sid.to_ascii_lowercase();
                            Route::Mailbox {
                                listening: mailbox.listening(&session, SystemTime::now()),
                                session,
                                provider: peer.record.provider.clone(),
                            }
                        }
                        _ => unavailable(
                            "This peer has no active Claude Code or Antigravity session id to leave a message for.",
                        ),
                    };
                    (peer.record.run_id.unwrap_or_default(), route)
                } else {
                    (
                        peer.record.run_id.unwrap_or_default(),
                        unavailable(format!(
                            "This peer is not currently live (state: {}).",
                            peer.liveness
                        )),
                    )
                }
            }
            Err(err) => (
                String::new(),
                unavailable(err.into_command_error().message().to_owned()),
            ),
        }
    } else {
        let row = state.with_store(|store| {
            query(store, "runs.get", &json!({"id": request.id}).to_string())
        })?;
        let run = &row["item"];
        let id = request.id.clone();
        let route = decide(
            run,
            |session| mailbox.listening(session, SystemTime::now()),
            || {
                let checkout_exists = run["cwd"]
                    .as_str()
                    .is_some_and(|cwd| std::path::Path::new(cwd).is_dir());
                let provider = run["provider"].as_str().unwrap_or("claude");
                conversation::judge(run, checkout_exists, |session| {
                    if provider == "agy" {
                        conversation::search_antigravity_homes(agy_homes, session)
                    } else {
                        conversation::search_claude_configs(claude_configs, session)
                    }
                })
            },
            || managed_run::follow_up_with(state, &id, None, None, &mut call),
        );
        (id, route)
    };
    let mut answer = route.preview();
    answer["ok"] = json!(true);
    answer["sent"] = json!(false);
    if !request.confirm {
        return Ok(answer);
    }
    let expected = request.expect_route.unwrap_or_default();
    if expected != route.name() {
        return Err(WorkbenchError::new(
            "route_changed",
            format!(
                "This message would now go by {} and not by {expected}, which is what you were shown. This press sent nothing; read the preview again.",
                route.name()
            ),
        ));
    }
    match route {
        Route::Unavailable { reason } => Err(WorkbenchError::new("unavailable", reason)),
        Route::Mailbox { session, .. } => {
            let deposit = mailbox
                .deposit(
                    &session,
                    &request.request_id,
                    &request.text,
                    SystemTime::now(),
                )
                .map_err(mailbox_error)?;
            answer["sent"] = json!(true);
            answer["status"] = json!(match mailbox.status(&session, &request.request_id) {
                Delivery::Claimed => "picked_up",
                Delivery::Pending | Delivery::Absent => "queued",
            });
            answer["request_id"] = json!(deposit.request_id);
            answer["expires_at_ms"] = json!(deposit.expires_at_ms);
            answer["duplicate"] = json!(deposit.duplicate);
            Ok(answer)
        }
        // The renderer opens the tab; the host has only said it can.
        Route::Resume { .. } => {
            answer["action"] = json!("open_resume");
            Ok(answer)
        }
        Route::Managed { .. } => {
            let outcome = managed_run::follow_up_with(
                state,
                &id,
                Some(&request.request_id),
                Some(&request.text),
                &mut call,
            )?;
            let (sent, status, reason) = managed_delivery(&outcome);
            answer["sent"] = sent;
            answer["status"] = json!(status);
            for key in [
                "retryable",
                "code",
                "turn",
                "max_turns",
                "idle_deadline",
                "request_id",
                "cause",
            ] {
                if !outcome[key].is_null() {
                    answer[key] = outcome[key].clone();
                }
            }
            answer["reason"] = json!(reason);
            Ok(answer)
        }
    }
}

/// What a managed follow-up's answer says about delivery, as `(sent, status,
/// reason)`. `sent` is `true` only for a message Manvi wrote to the provider and
/// `false` only for an answer that is known to have written nothing; for an
/// outcome Manvi has not settled (`pending`), cannot settle (`unresolved`), or
/// that this build cannot read (`unconfirmed`, or a state a message send should
/// not have been answered with) it is `null`: unknown, never "not sent".
fn managed_delivery(outcome: &Value) -> (Value, String, String) {
    let state = outcome["state"].as_str().unwrap_or_default();
    let reason = outcome["reason"].as_str().unwrap_or_default().to_owned();
    match state {
        "delivered" => (
            json!(true),
            "delivered".into(),
            "Delivered to the managed run as its next turn. That the new turn has begun is shown on the run.".into(),
        ),
        "refused" | "unsupported" => (json!(false), state.into(), reason),
        "pending" | "unconfirmed" => (Value::Null, state.into(), reason),
        // An error answer carries its own words; an answered state does not.
        "unresolved" => (
            Value::Null,
            state.into(),
            if reason.is_empty() {
                managed_run::UNRESOLVED_REASON.to_owned()
            } else {
                reason
            },
        ),
        // Manvi answered a message with a run state instead of a delivery.
        // Whether the message was written is not something that answer says.
        other => (
            Value::Null,
            "unconfirmed".into(),
            format!(
                "Manvi answered with the run's state ({other}) and not with a delivery, so it is not known whether this message reached the agent."
            ),
        ),
    }
}

fn mailbox_error(error: MailboxError) -> WorkbenchError {
    let code = match &error {
        MailboxError::Invalid(_) => "invalid_input",
        MailboxError::Full(_) => "mailbox_full",
        MailboxError::Insecure(_) => "mailbox_unsafe",
        MailboxError::Io(_) => "mailbox_error",
    };
    WorkbenchError::new(code, error.to_string())
}

#[cfg(test)]
mod tests;
