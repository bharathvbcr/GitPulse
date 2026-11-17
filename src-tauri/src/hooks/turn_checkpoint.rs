//! The hooks' side of turn checkpoints (`engine::turn_checkpoint`): at the two
//! turn boundaries, ask the running app to take one.
//!
//! This process only decides to ask and only sends a request over the
//! UID-authenticated mailbox; the app writes the ref and the ledger row. It
//! asks only when `turn_checkpoints.enabled` is set in `tools.json`
//! ([`crate::tool_config::TurnCheckpointSettings`]), and when it is not, this
//! returns before any connection, process or ledger row.
//!
//! It never fails the hook, never waits past [`TURN_CHECKPOINT_APP_DEADLINE`],
//! and never prints to the agent: a turn boundary happens every turn, and a
//! warning each time is noise the person cannot act on. What happened is the
//! returned [`Outcome`] (logged at debug) and, for every attempt that reaches
//! the app, the app's ledger row.

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::HookInput;
use crate::engine::turn_checkpoint::TurnEvent;

/// How long a hook waits for the app's answer. Short: the agent is waiting. A
/// capture that outlasts it finishes in the app and is recorded there.
pub(super) const TURN_CHECKPOINT_APP_DEADLINE: Duration = Duration::from_millis(1000);

/// The longest label sent to the app.
#[cfg(target_os = "macos")]
const MAX_LABEL_BYTES: usize = 1000;

/// What one boundary call came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Outcome {
    /// The setting is off (or unreadable): nothing was attempted.
    Disabled,
    /// The app answered: `captured`, `unchanged`, `coalesced`, `skipped` or
    /// `failed`, with its reason. Built only where the app-side recorder exists.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    Answered {
        outcome: String,
        reason: Option<String>,
    },
    /// The request did not reach a checkpoint: no app, no deadline met, an
    /// older app, an unsupported platform. The reason says which.
    NotCaptured(String),
}

/// The turn is starting (`UserPromptSubmit`).
pub(super) fn at_turn_start(input: &HookInput) -> Outcome {
    at_boundary(input, TurnEvent::TurnStart)
}

/// The turn is really ending: a `Stop` that did not continue.
pub(super) fn at_turn_end(input: &HookInput) -> Outcome {
    at_boundary(input, TurnEvent::TurnEnd)
}

fn at_boundary(input: &HookInput, event: TurnEvent) -> Outcome {
    let Some(root) = super::find_git_root(Path::new(&input.cwd)) else {
        return Outcome::Disabled;
    };
    // An untrusted repository is never asked about: no request, no ledger row.
    if crate::repository_trust::require(&root).is_err() {
        return Outcome::Disabled;
    }
    let root = root.to_string_lossy().into_owned();
    let outcome = at_boundary_in(
        profile_dir(&root),
        &root,
        input,
        event,
        TURN_CHECKPOINT_APP_DEADLINE,
        reach_app,
    );
    log::debug!(target: "hooks", "turn checkpoint ({}): {outcome:?}", event.as_str());
    outcome
}

#[cfg(target_os = "macos")]
fn profile_dir(root: &str) -> Option<PathBuf> {
    super::scan_profile(root)
}

#[cfg(not(target_os = "macos"))]
fn profile_dir(_root: &str) -> Option<PathBuf> {
    crate::tool_config::default_config_dir()
}

/// The boundary call with its profile directory, deadline and transport
/// passed in, so a test can see that a disabled setting never reaches `reach`.
fn at_boundary_in<R>(
    dir: Option<PathBuf>,
    root: &str,
    input: &HookInput,
    event: TurnEvent,
    deadline: Duration,
    reach: R,
) -> Outcome
where
    R: FnOnce(PathBuf, AppRequest, Duration) -> Result<serde_json::Value, String> + Send + 'static,
{
    let Some(dir) = dir else {
        return Outcome::Disabled;
    };
    if !crate::tool_config::turn_checkpoint_settings_in(&dir).enabled {
        return Outcome::Disabled;
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (root, input, event, deadline, reach);
        Outcome::NotCaptured("not captured: the app-side recorder is unavailable off macOS".into())
    }
    #[cfg(target_os = "macos")]
    {
        let label = (event == TurnEvent::TurnEnd).then(|| label_of(&input.last_assistant_message));
        let request = crate::desktop::instance::Request::RecordTurnCheckpoint {
            id: super::mailbox_id("turn"),
            cwd: root.to_string(),
            session_id: (!input.session_id.trim().is_empty()).then(|| input.session_id.clone()),
            event: event.as_str().to_string(),
            label: label.filter(|l| !l.is_empty()),
        };
        let answer = super::within_budget(deadline + Duration::from_millis(100), move || {
            reach(dir, request, deadline)
        });
        match answer {
            Some(Ok(reply)) => answered(&reply),
            Some(Err(why)) => Outcome::NotCaptured(not_captured_reason(&why)),
            None => Outcome::NotCaptured("not captured: the app missed its deadline".into()),
        }
    }
}

/// What the hook sends the app. `desktop::instance` exists on macOS only, so
/// elsewhere there is no request to build, and this says so in the type.
#[cfg(target_os = "macos")]
type AppRequest = crate::desktop::instance::Request;
#[cfg(not(target_os = "macos"))]
type AppRequest = std::convert::Infallible;

#[cfg(target_os = "macos")]
fn reach_app(
    dir: PathBuf,
    request: AppRequest,
    deadline: Duration,
) -> Result<serde_json::Value, String> {
    crate::desktop::instance::reach_owner(&dir, deadline)
        .ok_or_else(|| "no GitPulse app is answering".to_string())?
        .record_turn_checkpoint(&request, deadline)
}

#[cfg(not(target_os = "macos"))]
fn reach_app(
    _dir: PathBuf,
    _request: AppRequest,
    _deadline: Duration,
) -> Result<serde_json::Value, String> {
    Err("the app-side recorder is unavailable off macOS".into())
}

/// One redacted, control-free line of the agent's last words, bounded.
#[cfg(target_os = "macos")]
fn label_of(text: &str) -> String {
    let line = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let clean: String = crate::ledger::redact::text(line)
        .chars()
        .filter(|c| !c.is_control())
        .collect();
    let mut end = clean.len().min(MAX_LABEL_BYTES);
    while !clean.is_char_boundary(end) {
        end -= 1;
    }
    clean[..end].to_string()
}

#[cfg(target_os = "macos")]
fn answered(reply: &serde_json::Value) -> Outcome {
    match reply.get("outcome").and_then(serde_json::Value::as_str) {
        Some(outcome) => Outcome::Answered {
            outcome: outcome.to_string(),
            reason: reply
                .get("reason")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
        },
        None => Outcome::NotCaptured("not captured: the app's answer named no outcome".into()),
    }
}

/// An owner that predates the request refuses its `kind` as a malformed
/// frame; say that, rather than a parser's wording.
#[cfg(target_os = "macos")]
fn not_captured_reason(why: &str) -> String {
    if why.contains("unknown variant") {
        "not captured: app does not support checkpoints".to_string()
    } else {
        format!("not captured: {why}")
    }
}

#[cfg(all(test, target_os = "macos"))]
#[path = "turn_checkpoint_tests.rs"]
mod tests;
