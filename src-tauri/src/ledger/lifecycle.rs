//! A session's lifecycle, as the ledger keeps it: `session.state` rows.
//!
//! The hook bridge hears what an agent session is doing (a prompt submitted, a
//! tool finished, a permission asked for, a turn finished) and, until a row was
//! written, forgot it as soon as the banner decision was made. A run's
//! timeline needs it: how long an agent worked, waited for a person, or sat
//! idle is only knowable between two observed changes.
//!
//! One row is written per *change of state*, not per report: `PostToolUse`
//! arrives after every tool call and must not become a row each time, so the
//! owner of the coalescing ([`state_of`], used by the intake) names the state
//! each event leaves a session in and a row is written only when that state
//! differs from the last one written for the session.
//!
//! The row's `session_id` is the terminal session's (the key the hook reports
//! under); `detail_json` names the event and its state, and the run it belongs
//! to when the session is a task attempt. The agent's own transcript rows are
//! filed under the agent's session id; the run record bridges the two, and no
//! row is rewritten to join them.

use super::{ActorKind, Draft, EventStamp, Outcome};

/// The action a lifecycle change is recorded under.
pub const ACTION: &str = "session.state";

/// Every event a lifecycle row can name: the bridge's vocabulary
/// (`alerts::bridge::EVENTS`), which a test holds this list to, and the
/// frontend's `SESSION_STATE_EVENTS`, which a contract test holds it to.
pub const EVENTS: [&str; 12] = [
    "permission_prompt",
    "idle_prompt",
    "agent_needs_input",
    "agent_completed",
    "elicitation_dialog",
    "elicitation_url_dialog",
    "error",
    "permission_request",
    "turn_finished",
    "prompt_submitted",
    "tool_finished",
    "session_ended",
];

/// What the session is doing once `event` has happened, or `None` for a name
/// that is not an event.
///
/// * `working`: the agent has the turn (a prompt was submitted; a tool ran).
/// * `waiting_you`: it is blocked on a person, or stopped on an error.
/// * `idle`: it finished its turn and is not asking for anything.
/// * `ended`: the session is gone.
pub fn state_of(event: &str) -> Option<&'static str> {
    Some(match event {
        "prompt_submitted" | "tool_finished" => "working",
        "permission_prompt"
        | "permission_request"
        | "idle_prompt"
        | "agent_needs_input"
        | "elicitation_dialog"
        | "elicitation_url_dialog"
        | "error" => "waiting_you",
        "turn_finished" | "agent_completed" => "idle",
        "session_ended" => "ended",
        _ => return None,
    })
}

/// One change of a session's state, as the intake saw it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    /// The terminal session the hook reported under.
    pub session: String,
    /// The event that caused it, one of [`EVENTS`].
    pub event: &'static str,
    /// The state it left the session in ([`state_of`]).
    pub state: &'static str,
    /// When this process was told, in epoch milliseconds. The hook's own stamp
    /// is monotonic, not a wall clock, so the row is dated by receipt.
    pub at_ms: u64,
    /// Which agent said so ("Claude Code", "Codex").
    pub agent: String,
}

/// Writes `change` as a `session.state` row in `repository`'s ledger.
///
/// `repository` is the repository the terminal registry says the session was
/// opened for, never a path the report named. `run_id` is the task attempt the
/// session belongs to, when it is one. Returns the row's id, or `None` when
/// the ledger refused it (counted by the ledger's own status, as every dropped
/// append is).
pub fn record(repository: &str, run_id: Option<&str>, change: &Change) -> Option<i64> {
    let address = super::bindings::repository_address(repository).ok();
    let ledger_repo = address
        .as_ref()
        .map_or_else(|| repository.to_string(), |address| address.anchor.clone());
    let worktree_path = address
        .as_ref()
        .and_then(|address| (address.worktree != address.anchor).then(|| address.worktree.clone()));
    let task_id = address.and_then(|address| {
        super::bindings::resolve(&address.anchor, &address.worktree)
            .ok()
            .flatten()
    });
    let mut detail = serde_json::json!({
        "source": "hook",
        "event": change.event,
        "state": change.state,
    });
    if let Some(run) = run_id {
        detail["run_id"] = run.into();
    }
    let stamp = EventStamp {
        occurred_at: super::ids::iso8601_utc(change.at_ms),
        // Seen by this process when it was told, not read from the agent's clock.
        occurred_at_basis: "ingest".to_string(),
        task_basis: if task_id.is_some() {
            "bound"
        } else {
            "unbound"
        }
        .to_string(),
    };
    super::record(Draft {
        repo_path: ledger_repo,
        worktree_path,
        actor_kind: Some(ActorKind::Agent),
        actor_id: Some(change.agent.clone()),
        session_id: Some(change.session.clone()),
        task_id,
        action: ACTION.to_string(),
        object: Some(change.event.to_string()),
        outcome: Some(Outcome::Ok),
        detail_json: Some(detail.to_string()),
        event_stamp: Some(stamp),
        ..Draft::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_event_has_a_state_and_nothing_else_does() {
        for event in EVENTS {
            assert!(state_of(event).is_some(), "{event}");
        }
        for other in ["", "stop", "PostToolUse", "tool_started", "session_start"] {
            assert_eq!(state_of(other), None, "{other}");
        }
    }

    #[test]
    fn the_states_are_the_ones_the_timeline_draws() {
        assert_eq!(state_of("prompt_submitted"), Some("working"));
        assert_eq!(state_of("tool_finished"), Some("working"));
        assert_eq!(state_of("permission_request"), Some("waiting_you"));
        assert_eq!(state_of("error"), Some("waiting_you"));
        assert_eq!(state_of("turn_finished"), Some("idle"));
        assert_eq!(state_of("session_ended"), Some("ended"));
    }

    #[test]
    fn the_list_is_the_bridges_vocabulary() {
        let bridge: Vec<&str> = crate::alerts::bridge::EVENTS
            .iter()
            .map(|e| e.name)
            .collect();
        let mut mine = EVENTS.to_vec();
        let mut theirs = bridge;
        mine.sort_unstable();
        theirs.sort_unstable();
        assert_eq!(mine, theirs);
    }

    fn change(event: &'static str, state: &'static str, at_ms: u64) -> Change {
        Change {
            session: "term-1".to_string(),
            event,
            state,
            at_ms,
            agent: "Claude Code".to_string(),
        }
    }

    #[test]
    fn a_change_is_a_row_dated_by_receipt_with_the_event_and_the_run() {
        let dir = tempfile::tempdir().expect("temp repository");
        let path = dir.path().canonicalize().expect("canonical path");
        crate::test_support::git_in(&path, &["init", "-b", "main"]);
        std::fs::write(path.join("seed.txt"), "seed").unwrap();
        crate::test_support::git_in(&path, &["add", "seed.txt"]);
        crate::test_support::git_in(&path, &["commit", "-m", "seed"]);
        let repo = path.to_string_lossy().into_owned();
        // 2026-09-01T12:00:00.000Z
        let at = super::super::ids::parse_event_time("2026-09-01T12:00:00.000Z").unwrap();
        let id = record(
            &repo,
            Some("run-7"),
            &change("permission_request", "waiting_you", at),
        )
        .expect("recorded");
        let rows = super::super::tail(&repo, 0, 10).unwrap();
        let row = rows.iter().find(|r| r.id == id).expect("the row");
        assert_eq!(row.action, ACTION);
        assert_eq!(row.session_id.as_deref(), Some("term-1"));
        assert_eq!(row.object.as_deref(), Some("permission_request"));
        assert_eq!(row.actor_kind, "agent");
        let detail: serde_json::Value =
            serde_json::from_str(row.detail_json.as_deref().unwrap()).unwrap();
        assert_eq!(detail["event"], "permission_request");
        assert_eq!(detail["state"], "waiting_you");
        assert_eq!(detail["run_id"], "run-7");
        assert_eq!(detail["occurred_at"], "2026-09-01T12:00:00.000Z");
        assert_eq!(detail["occurred_at_basis"], "ingest");
    }
}
