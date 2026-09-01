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

pub use policy::{
    check_command, check_command_allowing, check_file, render_command, HostScope, PolicyStatus,
    PolicyVerdict,
};
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
    guard_command_allowing(repo_path, argv, &[])
}

/// Guards a command while declaring an allowlist for it.
///
/// The local CI runner is the caller this exists for. It used to bypass the
/// gate entirely — every `npm test` and `cargo clippy` it spawned ran
/// unjudged and unrecorded, which is a privileged unlogged execution path in a
/// product whose claim is to be the trust boundary between agents and the
/// repository. Declaring the step it is about to run lets the harness answer
/// cleanly instead of demoting a `command.not_allowed` on every step.
pub(crate) fn guard_command_allowing(
    repo_path: &str,
    argv: &[&str],
    allowed: &[String],
) -> Result<PolicyVerdict, String> {
    let command = render_command(argv);
    let verdict =
        check_command_allowing(repo_path, &command, scope_for(repo_path).as_ref(), allowed);
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
    let verdict = check_file(repo_path, file_path, op, scope_for(repo_path).as_ref());
    let action = format!("file.{}", if op.is_empty() { "write" } else { op });
    record_gate(repo_path, &action, file_path, None, &verdict);
    gated(verdict)
}

/// The task scope this checkout is working under, if it is bound to one.
///
/// Two lookups, both cheap and both allowed to come back empty:
///
/// 1. the ledger, for the binding recorded when the worktree was bound;
/// 2. DevCouncil's store, for that task's declared scope.
///
/// Returning `None` is the ordinary case — most checkouts are bound to no
/// task — and it reproduces exactly the behaviour that existed before scopes:
/// the ladder stops at `task.absent` and the host posture demotes it.
///
/// A binding that resolves to a task the store cannot produce returns `None`
/// rather than an empty scope. The difference matters: an empty scope
/// authorises *nothing*, so every write in that checkout would be refused as
/// unplanned — a store that is merely absent must not read as a plan that
/// forbids everything.
fn scope_for(repo_path: &str) -> Option<HostScope> {
    let task_id = crate::ledger::bindings::resolve(repo_path, repo_path)
        .ok()
        .flatten()?;
    let scope = crate::tasks::scope(repo_path, &task_id).ok().flatten()?;
    Some(HostScope {
        task_id: scope.id,
        // The union the gate measures against: what the planner authorised
        // plus what the executor argued into its own scope. They stay
        // distinguishable in the store, and the harness reports a write
        // authorised by the second half as `widened` rather than clean.
        planned_files: scope
            .planned_files
            .into_iter()
            .chain(scope.agent_appended_files)
            .collect(),
        forbidden_changes: scope.forbidden_changes,
        worktree: repo_path.to_string(),
        allowed_commands: scope.allowed_commands,
    })
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
        // Taken from the verdict rather than looked up again: the ledger must
        // record the task the gate actually measured against, not the one a
        // second lookup would find now.
        task_id: Some(verdict.task_id.clone()).filter(|t| !t.is_empty()),
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
        let repo_buf = dir
            .path()
            .canonicalize()
            .unwrap_or_else(|_| dir.path().to_path_buf());
        let repo = repo_buf.to_str().expect("utf8 path");

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

#[cfg(test)]
mod scope_tests {
    use super::*;

    /// A `manvi serve` stand-in that answers the handshake, records every
    /// later request, and refuses with a scope verdict.
    #[cfg(unix)]
    const FAKE_RECORDER: &str = r#"#!/bin/sh
reply() {
  id=$(printf '%s' "$1" | sed -n 's/^{"id":"\([0-9]*\)".*/\1/p')
  printf '{"id":"%s","ok":true,"result":%s}\n' "$id" "$2"
}
IFS= read -r line || exit 1
reply "$line" '{"protocol":1,"ops":["hello","policy.check.command","policy.check.file"],"posture":"host"}'
while IFS= read -r line; do
  printf '%s\n' "$line" >> "@REQUESTS@"
  reply "$line" '{"action":"deny","rule":"scope.unplanned","severity":"soft","reason":"outside the plan","target":"docs/elsewhere.md","task_id":"TASK-7","demoted":"","degraded":["repo_map.unavailable"]}'
done
"#;

    /// GitPulse sends the declared scope on the wire, and keeps the task the
    /// harness answers with.
    ///
    /// Hermetic on purpose: it drives a recording fake sidecar rather than
    /// whatever `manvi` happens to be installed. An earlier version of this
    /// test used the real binary and passed against a build with no scope
    /// support at all — the verdict came back `task.absent`, which is not
    /// `Allowed`, so a weak assertion called it a success. The bar here is that
    /// the scope is provably on the wire.
    #[cfg(unix)]
    #[test]
    fn a_bound_worktree_sends_its_scope_and_keeps_the_task() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().to_str().expect("utf8");

        let db = crate::tasks::store_path(repo);
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        let store = dc_store::Store::open(&db).unwrap();
        store
            .connection()
            .execute(
                "INSERT INTO tasks (id, title, description, planned_files_json, status)
                 VALUES ('TASK-7', 'Ledger', '',
                     '[{\"path\":\"src/planned.rs\",\"allowed_change\":\"modify\"}]', 'in_progress')",
                [],
            )
            .unwrap();

        // Unbound, there is nothing to declare — the behaviour every existing
        // checkout keeps.
        assert!(
            scope_for(repo).is_none(),
            "an unbound checkout declares nothing"
        );

        crate::ledger::bindings::bind(repo, repo, "TASK-7").expect("bind");
        let scope = scope_for(repo).expect("a bound checkout declares its scope");
        assert_eq!(scope.task_id, "TASK-7");
        assert_eq!(scope.planned_files, vec!["src/planned.rs"]);
        assert_eq!(scope.worktree, repo);

        // A sidecar that records every request and refuses, so both halves of
        // the round trip are observable.
        let requests = dir.path().join("requests.ndjson");
        let script = dir.path().join("recording-manvi");
        let body = FAKE_RECORDER.replace("@REQUESTS@", &requests.display().to_string());
        std::fs::write(&script, body).expect("write fake sidecar");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        sidecar::set_test_binary(Some(script.to_string_lossy().into_owned()));
        struct Clear;
        impl Drop for Clear {
            fn drop(&mut self) {
                sidecar::set_test_binary(None);
                sidecar::reset();
            }
        }
        let _clear = Clear;
        sidecar::reset();

        let before = crate::ledger::latest_cursor(repo).expect("cursor");
        let refused = guard_file(repo, "docs/elsewhere.md", "modify");
        assert!(
            refused.is_err(),
            "a denied write must not become permission to act"
        );

        // What went onto the wire.
        let sent = std::fs::read_to_string(&requests).unwrap_or_default();
        assert!(!sent.is_empty(), "no policy request reached the sidecar");
        let request: serde_json::Value =
            serde_json::from_str(sent.lines().next().unwrap()).expect("the request is JSON");
        let params = &request["params"];
        assert_eq!(
            params["scope"]["task_id"], "TASK-7",
            "the declared scope did not reach the harness: {params}"
        );
        assert_eq!(params["scope"]["planned_files"][0], "src/planned.rs");
        assert_eq!(params["scope"]["worktree"], repo);

        // And what came back was kept rather than dropped.
        let events = crate::ledger::tail(repo, before, 10).expect("tail");
        let row = events.last().expect("a row");
        assert_eq!(
            row.task_id.as_deref(),
            Some("TASK-7"),
            "the ledger row must name the task the gate measured against"
        );
        assert_eq!(row.outcome, "blocked", "a refused write is blocked, not ok");
        let verdict: PolicyVerdict =
            serde_json::from_str(row.verdict_json.as_deref().unwrap()).expect("verdict");
        assert_eq!(
            verdict.task_id, "TASK-7",
            "this end used to drop the task id"
        );
        assert_eq!(verdict.rule, "scope.unplanned");
        assert_eq!(verdict.status, PolicyStatus::Blocked);
    }

    /// A binding whose task the store cannot produce declares nothing.
    ///
    /// The alternative — an empty scope — authorises no file at all, so every
    /// write in the checkout would be refused as unplanned. A missing store
    /// must not read as a plan that forbids everything.
    #[test]
    fn a_binding_without_a_task_declares_no_scope() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().to_str().expect("utf8");
        crate::ledger::bindings::bind(repo, repo, "TASK-DOES-NOT-EXIST").expect("bind");
        assert!(
            scope_for(repo).is_none(),
            "a task the store cannot produce must not become an empty plan"
        );
    }

    /// Agent-appended scope reaches the gate, so a write it authorises is
    /// permitted — and the harness reports it as `widened`, not clean.
    #[test]
    fn agent_appended_scope_is_part_of_what_the_gate_measures() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().to_str().expect("utf8");
        let db = crate::tasks::store_path(repo);
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        let store = dc_store::Store::open(&db).unwrap();
        store
            .connection()
            .execute(
                "INSERT INTO tasks (id, title, description, planned_files_json,
                     agent_appended_planned_files_json, status)
                 VALUES ('TASK-8', 'Widened', '',
                     '[{\"path\":\"src/a.rs\",\"allowed_change\":\"modify\"}]',
                     '[{\"path\":\"src/b.rs\",\"allowed_change\":\"modify\"}]', 'in_progress')",
                [],
            )
            .unwrap();
        crate::ledger::bindings::bind(repo, repo, "TASK-8").expect("bind");

        let scope = scope_for(repo).expect("scope");
        assert!(
            scope.planned_files.contains(&"src/b.rs".to_string()),
            "appended scope must reach the gate: {:?}",
            scope.planned_files
        );
        assert!(scope.planned_files.contains(&"src/a.rs".to_string()));
    }
}
