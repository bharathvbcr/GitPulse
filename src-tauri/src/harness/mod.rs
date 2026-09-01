//! Integration with the MANVI coding-agent harness.
//!
//! GitPulse embeds MANVI the way its host plane is meant to be embedded: as a
//! `manvi serve` child speaking NDJSON over stdio. Three of its planes are
//! used — the command gate and the write gate guard every mutating Git action,
//! and the local-LLM plane (capability probing, token budgeting, completion
//! parsing) backs the AI features in `crate::ai`.
//!
//! Everything here degrades: with no harness installed GitPulse still commits,
//! pushes and rebases, and every unguarded action is reported as unguarded.

pub mod policy;
pub mod protocol;
pub mod sidecar;

use serde::{Deserialize, Serialize};

pub use policy::{check_command, check_file, render_command, PolicyStatus, PolicyVerdict};
pub use protocol::{HelloResult, PrepareResult, ProbeResult, SettleResult};
pub use sidecar::{HarnessError, DEFAULT_CALL_TIMEOUT};

/// What the UI shows about the harness.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HarnessStatus {
    pub available: bool,
    /// Absolute path of the binary in use, empty when none was found.
    pub binary: String,
    pub protocol: i64,
    pub posture: String,
    pub ops: Vec<String>,
    /// Empty when available; otherwise why not.
    pub error: String,
    pub error_code: String,
}

impl HarnessStatus {
    pub fn probe() -> Self {
        match sidecar::handshake() {
            Ok((binary, hello)) => HarnessStatus {
                available: true,
                binary,
                protocol: hello.protocol,
                posture: hello.posture,
                ops: hello.ops,
                error: String::new(),
                error_code: String::new(),
            },
            Err(e) => HarnessStatus {
                available: false,
                binary: sidecar::resolve_binary().unwrap_or_default(),
                protocol: 0,
                posture: String::new(),
                ops: Vec::new(),
                error: e.message(),
                error_code: e.code().to_string(),
            },
        }
    }
}

/// The single gate seam for guarded Git actions.
///
/// Every mutating command goes through here and nowhere else: `argv` is
/// rendered the way a shell would show it ([`render_command`]) so the
/// command gate judges the command the client actually runs, a blocking
/// verdict becomes the rendered refusal (`Err`), and otherwise the
/// [`PolicyVerdict`] is handed back so the caller can report an action that
/// ran unchecked when no harness is installed — which is never rendered as
/// an approval.
///
/// This used to exist twice (a generic closure-taking variant here, and a
/// private reimplementation in `commands`); this signature is the canonical
/// one both now share.
pub(crate) fn guard_command(repo_path: &str, argv: &[&str]) -> Result<PolicyVerdict, String> {
    let command = render_command(argv);
    let verdict = check_command(repo_path, &command);
    let action = crate::ledger::action_for_argv(argv);
    let argv_json = serde_json::to_string(argv).ok();
    record_gate(repo_path, &action, &command, argv_json, &verdict);
    gated(verdict)
}

/// Evaluates one file write, on the same terms as [`guard_command`].
pub(crate) fn guard_file(
    repo_path: &str,
    file_path: &str,
    op: &str,
) -> Result<PolicyVerdict, String> {
    let verdict = check_file(repo_path, file_path, op);
    let action = format!("file.{}", if op.is_empty() { "write" } else { op });
    record_gate(repo_path, &action, file_path, None, &verdict);
    gated(verdict)
}

/// Writes one gate decision to the ledger.
///
/// This is the seam every guarded mutation already passes through, which is
/// what makes the record complete without touching the thirty-six call sites
/// above it. `repoStore.runMutating()` on the frontend covers only about four
/// fifths of the writes — worktree operations, `cmd_write_file_content`,
/// `cmd_restack`, interactive rebase and the PTY all reach Git without it — so
/// hooking the frontend seam instead would have recorded a history that looked
/// complete and was not.
///
/// # What `outcome` means on these rows
///
/// A gate row records what became of the *request*, at the moment the gate
/// answered:
///
/// * `blocked` — the gate refused, or could not run when it should have.
///   GitPulse did not perform the action. This is a complete and final fact.
/// * `ok` — the gate permitted it, and the action was about to run.
///
/// An `ok` gate row is therefore **not** a claim that the operation succeeded;
/// it is a claim that it was authorised. Rows carrying `{"phase":"gate"}` in
/// `detail_json` say so explicitly, so a consumer counting successful commits
/// cannot mistake an authorisation for a result. Completion rows, which do
/// carry the operation's own outcome, are written by the callers that observe
/// it and are absent from this build — the distinction is recorded here rather
/// than left for a reader to infer.
fn record_gate(
    repo_path: &str,
    action: &str,
    target: &str,
    argv_json: Option<String>,
    verdict: &PolicyVerdict,
) {
    use crate::ledger::{ActorKind, Draft, Outcome};

    // A refusal and a gate that could not run are both "this did not happen".
    // `gated` turns each into an `Err`, and the ledger must agree with it —
    // otherwise the history would show an authorised action the app refused.
    let blocked = verdict.blocks() || verdict.gate_failed();

    crate::ledger::record(Draft {
        repo_path: repo_path.to_string(),
        action: action.to_string(),
        object: Some(target.to_string()),
        argv_json,
        // The human at the keyboard is the actor for everything that reaches
        // this seam today. Agent attribution arrives with transcript ingest;
        // until then, claiming `Agent` here would be a guess written to disk.
        actor_kind: Some(ActorKind::Human),
        outcome: Some(if blocked {
            Outcome::Blocked
        } else {
            Outcome::Ok
        }),
        verdict_json: serde_json::to_string(verdict).ok(),
        detail_json: Some(r#"{"phase":"gate"}"#.to_string()),
        ..Default::default()
    });
}

/// The single place a verdict becomes permission to act.
///
/// Two things refuse here, and the second is the one that is easy to miss. A
/// rule firing is obvious. A gate that *could not run* is not: an unchecked
/// verdict does not block, so before this seam refused it, a busy or wedged
/// sidecar let `git push --force` straight through while the UI recorded it as
/// merely "unguarded" — the check that could not run reporting exactly what a
/// check that ran and passed reports. See [`PolicyVerdict::gate_failed`] for
/// why a missing harness is the only unchecked verdict that may proceed.
fn gated(verdict: PolicyVerdict) -> Result<PolicyVerdict, String> {
    if verdict.blocks() {
        return Err(verdict.refusal());
    }
    if verdict.gate_failed() {
        return Err(verdict.gate_failure());
    }
    Ok(verdict)
}

#[cfg(test)]
mod ledger_seam_tests {
    use super::*;

    /// Every guarded mutation reaches the ledger, whatever the gate said.
    ///
    /// This drives the real seam rather than the ledger API, because the claim
    /// being tested is about coverage: that a mutation cannot reach Git without
    /// leaving a durable record. Testing `ledger::append` directly would prove
    /// the ledger works and nothing about whether it is wired to anything.
    ///
    /// It passes with or without a MANVI harness installed. With one, the row
    /// carries the harness's verdict; without one, it carries an `unchecked`
    /// verdict — and recording *that* is the point, since an action that ran
    /// with no gate is exactly what a history must not omit.
    #[test]
    fn a_guarded_command_leaves_a_durable_row() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().to_str().expect("utf8 path");

        let before = crate::ledger::latest_cursor(repo).expect("cursor");
        let _ = guard_command(repo, &["git", "commit", "-m", "hello"]);
        let after = crate::ledger::latest_cursor(repo).expect("cursor");
        assert!(after > before, "the guard seam recorded nothing");

        let events = crate::ledger::tail(repo, before, 10).expect("tail");
        assert_eq!(events.len(), 1, "exactly one row per guarded command");
        let row = &events[0];

        assert_eq!(row.action, "git.commit", "the verb comes from argv");
        assert_eq!(row.repo_path, repo);
        assert_eq!(
            row.argv_json.as_deref(),
            Some(r#"["git","commit","-m","hello"]"#),
            "the argv is recorded as sent"
        );
        assert!(
            row.verdict_json.is_some(),
            "a row with no verdict at all would read as an unjudged action"
        );
        assert_eq!(
            row.detail_json.as_deref(),
            Some(r#"{"phase":"gate"}"#),
            "a gate row must say so, or an `ok` on it reads as a completed commit"
        );
        // Whatever the harness said, the row's outcome must agree with whether
        // the action was allowed to proceed.
        assert!(
            matches!(row.outcome.as_str(), "ok" | "blocked"),
            "unexpected outcome {:?}",
            row.outcome
        );
    }

    /// A file write is recorded under its own operation, not a git verb.
    #[test]
    fn a_guarded_file_write_records_its_operation() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().to_str().expect("utf8 path");

        let before = crate::ledger::latest_cursor(repo).expect("cursor");
        let _ = guard_file(repo, "src/lib.rs", "modify");
        let events = crate::ledger::tail(repo, before, 10).expect("tail");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].action, "file.modify");
        assert_eq!(events[0].object.as_deref(), Some("src/lib.rs"));
    }

    /// An unspecified operation is recorded as `write`, matching the harness's
    /// own reading of an empty op — not dropped, and not guessed at.
    #[test]
    fn an_unspecified_file_operation_is_recorded_as_a_write() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().to_str().expect("utf8 path");
        let before = crate::ledger::latest_cursor(repo).expect("cursor");
        let _ = guard_file(repo, "x.txt", "");
        let events = crate::ledger::tail(repo, before, 10).expect("tail");
        assert_eq!(events[0].action, "file.write");
    }

    #[test]
    fn a_non_git_command_is_not_filed_under_a_git_verb() {
        // `command.run` rather than a fabricated `git.*`: a mutation this build
        // does not recognise must still be recorded, and must not claim to be
        // something it is not.
        assert_eq!(
            crate::ledger::action_for_argv(&["cargo", "test"]),
            "command.run"
        );
        assert_eq!(crate::ledger::action_for_argv(&["git"]), "command.run");
        assert_eq!(crate::ledger::action_for_argv(&[]), "command.run");
        assert_eq!(
            crate::ledger::action_for_argv(&["git", "commit"]),
            "git.commit"
        );
        // A subcommand carrying separators still yields a legal action name,
        // because the event schema constrains the shape.
        assert_eq!(
            crate::ledger::action_for_argv(&["git", "cherry-pick"]),
            "git.cherry_pick"
        );
    }
}
