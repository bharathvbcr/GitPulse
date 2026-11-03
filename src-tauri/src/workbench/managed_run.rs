//! Native authority for managed coding sessions. The renderer supplies only a
//! saved attempt identity; checkout and process evidence come from this host.

use super::{process_birth, query, terminal_launch, WorkbenchError, WorkbenchState};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Identity {
    id: String,
}

/// Managed-run identities are shorter than the general record identifier.
const MAX_MANAGED_ID_BYTES: usize = 100;

fn valid_id(value: &str) -> bool {
    super::is_record_id_within(value, MAX_MANAGED_ID_BYTES)
}

fn parse(input: &str) -> Result<String, WorkbenchError> {
    if input.len() > 1024 {
        return Err(WorkbenchError::new(
            "invalid_input",
            "Managed control exceeds 1 KiB.",
        ));
    }
    let input: Identity = serde_json::from_str(input)
        .map_err(|e| WorkbenchError::new("invalid_input", e.to_string()))?;
    if !valid_id(&input.id) {
        return Err(WorkbenchError::new(
            "invalid_input",
            "Select a saved managed attempt.",
        ));
    }
    Ok(input.id)
}

fn saved(state: &WorkbenchState, id: &str) -> Result<Value, WorkbenchError> {
    let row = state.with_store(|store| query(store, "runs.get", &json!({"id":id}).to_string()))?;
    let managed = row["item"]["provider"]
        .as_str()
        .is_some_and(super::terminal_command::is_managed_provider);
    if row["item"]["kind"] != "managed" || !managed {
        return Err(WorkbenchError::new(
            "run_kind_mismatch",
            "This attempt belongs to another execution adapter.",
        ));
    }
    Ok(row)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Prepared {
    protocol_version: u32,
    id: String,
    owner_id: String,
    session_id: String,
    process_id: u32,
    phase: String,
}

pub(super) fn launch(state: &WorkbenchState, input: &str) -> Result<Value, WorkbenchError> {
    // Refused rather than waited for: a managed launch blocks for the
    // provider's whole startup, and the caller can retry.
    let _launch = state.0.managed_launches.try_enter(&parse(input)?)?;
    launch_with(
        state,
        input,
        |op, params| state.worker_call_current(op, params),
        process_birth::read,
    )
}

/// Hands back the repository when a preparation died before the harness took
/// ownership, and says why in terms that are true.
///
/// A refused preparation used to leave the stored attempt sitting at
/// `prepared`. Nothing ever moved it: the harness had not claimed it, so the
/// harness would never finish it, and the store counts a prepared attempt as
/// occupying the repository's single run slot until its five-minute expiry. So
/// one failed launch locked the repository out of *every* further attempt —
/// and then the row stayed on screen as "Prepared" forever, a phantom the
/// panel had stopped polling because it had expired.
///
/// Only an unclaimed row is cancelled. Once `owner_id` is set the live Manvi
/// session owns that attempt and its own failure path resolves it; cancelling
/// underneath it would race a process that is still holding a provider.
fn release_unclaimed(state: &WorkbenchState, id: &str, error: WorkbenchError) -> WorkbenchError {
    let error = attribute(state, error);
    let Ok(row) = saved(state, id) else {
        return error;
    };
    if row["item"]["state"] != "prepared" || !row["item"]["owner_id"].is_null() {
        return error;
    }
    let Some(revision) = row["item"]["revision"].as_i64() else {
        return error;
    };
    let cancel = json!({
        "id": id,
        "request_id": format!("managed_release_{id}"),
        "expected_revision": revision,
    });
    match state.with_store(|store| query(store, "runs.cancel", &cancel.to_string())) {
        Ok(_) => error,
        // The attempt is still real and still holds the slot. Saying so beats
        // a clean-looking error that leaves the reader wondering why the next
        // launch is refused as "repository busy".
        Err(cancel) => WorkbenchError::new(
            &error.code,
            format!(
                "{} The prepared attempt could not be released either ({}), so this repository \
                 stays busy until it expires; use Cancel preparation on the run below.",
                error.message, cancel.message
            ),
        ),
    }
}

/// Names what GitPulse could and could not check about this harness.
///
/// A build that never published its adapter set cannot be asked which
/// providers it drives, so a refusal from it may be about the provider rather
/// than the attempt — and its wording will describe whichever providers that
/// build happens to ship. Without this note the reader is handed a sentence
/// about Codex for a Claude launch and no way to tell that the real answer is
/// "this harness is too old to say".
fn attribute(state: &WorkbenchState, error: WorkbenchError) -> WorkbenchError {
    use crate::harness::protocol::ManagedAdapter;
    let unknown = matches!(
        state
            .worker_handshake()
            .map(|hello| hello.managed_adapter("")),
        Ok(ManagedAdapter::Unknown { .. })
    );
    if !unknown {
        return error;
    }
    WorkbenchError::new(
        &error.code,
        format!(
            "{} This Manvi build does not report which managed adapters it has, so GitPulse \
             could not check the provider before launching; the wording above may describe a \
             different provider than the one you chose. Updating Manvi is the usual fix.",
            error.message
        ),
    )
}

fn launch_with(
    state: &WorkbenchState,
    input: &str,
    mut call: impl FnMut(&str, Value) -> Result<Value, WorkbenchError>,
    observe: impl FnOnce(u32) -> Result<String, String>,
) -> Result<Value, WorkbenchError> {
    let id = parse(input)?;
    let row = saved(state, &id)?;
    if row["item"]["state"] != "prepared" && row["item"]["state"] != "starting" {
        // Retrying a lost result reads the same attempt; it never starts another.
        return Ok(row);
    }
    // A refusal here, before Manvi is asked, hands the repository back like
    // any other: left at `prepared`, the row would hold it until expiry.
    terminal_launch::revalidate(&row).map_err(|error| release_unclaimed(state, &id, error))?;
    // Stable for this attempt across UI reloads. The store forbids repeating a
    // consumed claim; only the owning live Manvi session can recover this call.
    let prepared = match call(
        "work.runs.managed.prepare",
        json!({"id":id,"request_id":format!("managed_launch_{id}"),"expected_revision":terminal_launch::PREPARED_REVISION}),
    ) {
        Ok(prepared) => prepared,
        Err(error) => return Err(release_unclaimed(state, &id, error)),
    };
    let activation = (|| {
        let prepared: Prepared = serde_json::from_value(prepared)
            .map_err(|e| WorkbenchError::new("protocol_error", e.to_string()))?;
        if prepared.protocol_version != 2
            || prepared.id != id
            || !valid_id(&prepared.owner_id)
            || !valid_id(&prepared.session_id)
            || prepared.process_id == 0
            || prepared.phase != "awaiting_activation"
        {
            return Err(WorkbenchError::new(
                "protocol_error",
                "The managed provider returned an unexpected preparation receipt.",
            ));
        }
        let current = saved(state, &id)?;
        if current["item"]["state"] != "starting"
            || current["item"]["owner_id"] != prepared.owner_id
            || current["item"]["session_id"] != prepared.session_id
        {
            return Err(WorkbenchError::new(
                "revision_conflict",
                "Managed ownership changed before activation.",
            ));
        }
        terminal_launch::revalidate(&current)?;
        let birth = observe(prepared.process_id)
            .map_err(|e| WorkbenchError::new("process_unavailable", e))?;
        call(
            "work.runs.managed.activate",
            json!({"id":id,"owner_id":prepared.owner_id,"session_id":prepared.session_id,"process_id":prepared.process_id,"process_start":birth}),
        )?;
        saved(state, &id)
    })();
    match activation {
        Ok(result) => Ok(result),
        Err(error) => {
            // Preparation started a helper. A failed native observation must
            // close that exact session, never leave an unactivated model host.
            match call("work.runs.managed.stop", json!({"id":id})) {
                Ok(_) => Err(error),
                Err(stop) => Err(WorkbenchError::new(
                    "outcome_uncertain",
                    format!(
                        "{} Stopping the prepared process also failed: {}",
                        error.message, stop.message
                    ),
                )),
            }
        }
    }
}

pub(super) fn stop(state: &WorkbenchState, input: &str) -> Result<Value, WorkbenchError> {
    let id = parse(input)?;
    let row = saved(state, &id)?;
    if !["starting", "running", "unresolved"]
        .iter()
        .any(|s| row["item"]["state"] == *s)
    {
        return Err(WorkbenchError::new(
            "invalid_state",
            "This managed attempt has no active process to stop.",
        ));
    }
    state.worker_call_current("work.runs.managed.stop", json!({"id":id}))
}

/// The op Manvi serves for another turn of a managed run.
pub(crate) const OP_CONTINUE: &str = "work.runs.managed.continue";

/// What `work.runs.managed.continue` takes: the run, and for a follow-up the
/// request id (so a retry never sends twice) and the message. Without a message
/// it is a status read. Kept as the one place these keys are written, so
/// `scripts/serve-ops-contract.test.ts` can hold them to
/// `contracts/serve.ops.schema.json`.
pub(crate) fn continue_params(id: &str, request_id: Option<&str>, message: Option<&str>) -> Value {
    let mut params = json!({"id": id});
    if let Some(request_id) = request_id {
        params["request_id"] = json!(request_id);
    }
    if let Some(message) = message {
        params["message"] = json!(message);
    }
    params
}

/// Manvi's refusals that a retry can cure, and nothing else: a turn is still
/// in flight, follow-ups came too fast, or the delivery's outcome was not known
/// yet (the same request id then returns it and never sends twice).
const RETRYABLE: [&str; 3] = ["E_TURN_IN_FLIGHT", "E_CONTINUE_RATE", "E_CONTINUE_PENDING"];

/// Every state a run can report between turns.
const FOLLOW_UP_STATES: [&str; 5] = ["delivered", "parked", "running", "unresolved", "ended"];

/// A follow-up's answer, as the renderer reads it.
///
/// Three kinds of answer are a result and not an error, because none of them
/// says anything is wrong with the run: Manvi answered (`ok: true`, its
/// `state`), Manvi cannot take follow-ups here (`state: "unsupported"`, with
/// `cause`), or Manvi refused this one (`state: "refused"`, with its `code` and
/// whether a retry can help). A call that did not reach an answer — Manvi down,
/// a timeout — stays an error: it is not evidence about the run.
pub(crate) fn follow_up_answer(
    outcome: Result<Value, WorkbenchError>,
) -> Result<Value, WorkbenchError> {
    match outcome {
        Ok(result) => {
            let state = result["state"].as_str().unwrap_or_default();
            let whole = |key: &str| result[key].as_i64().filter(|n| *n >= 0);
            if !FOLLOW_UP_STATES.contains(&state)
                || whole("turn").is_none()
                || whole("max_turns").is_none()
            {
                return Err(WorkbenchError::new(
                    "protocol_error",
                    "Manvi answered a follow-up with a state GitPulse does not know.",
                ));
            }
            let mut answer = json!({
                "ok": true,
                "state": state,
                "turn": result["turn"],
                "max_turns": result["max_turns"],
            });
            if let Some(request_id) = result["request_id"].as_str() {
                answer["request_id"] = json!(request_id);
            }
            if let Some(deadline) = whole("idle_deadline") {
                answer["idle_deadline"] = json!(deadline);
            }
            Ok(answer)
        }
        // Manvi did not advertise the op: an older build. Said as what it is.
        Err(error) if error.code == "unsupported_operation" => Ok(json!({
            "ok": false,
            "state": "unsupported",
            "cause": "manvi_too_old",
            "reason": "This Manvi version cannot take follow-ups. Update Manvi to send one to a running managed agent.",
            "retryable": false,
        })),
        Err(error) if error.code == "E_CONTINUE_UNSUPPORTED" => Ok(json!({
            "ok": false,
            "state": "unsupported",
            "cause": "not_prepared",
            "reason": format!("This run cannot take follow-ups: {}", error.message),
            "retryable": false,
        })),
        Err(error) if error.code.starts_with("E_") => Ok(json!({
            "ok": false,
            "state": "refused",
            "code": error.code,
            "reason": error.message,
            "retryable": RETRYABLE.contains(&error.code.as_str()),
        })),
        Err(error) => Err(error),
    }
}

/// Reads a live managed run's follow-up state, or delivers one follow-up to it,
/// through `call` (the profile worker's `work.runs.managed.continue`; a test's
/// stand-in for it). `message: None` is the status read and sends nothing.
/// `send_to_agent` is the one caller: it owns the route this belongs to.
pub(super) fn follow_up_with(
    state: &WorkbenchState,
    id: &str,
    request_id: Option<&str>,
    message: Option<&str>,
    call: impl FnOnce(&str, Value) -> Result<Value, WorkbenchError>,
) -> Result<Value, WorkbenchError> {
    if !valid_id(id) {
        return Err(WorkbenchError::new(
            "invalid_input",
            "Select a saved managed attempt.",
        ));
    }
    if message.is_some() != request_id.is_some() {
        return Err(WorkbenchError::new(
            "invalid_input",
            "A follow-up names its request id, and a status read names none.",
        ));
    }
    if let Some(request_id) = request_id {
        if !valid_id(request_id) {
            return Err(WorkbenchError::new(
                "invalid_input",
                "A request id is letters, digits, - or _.",
            ));
        }
    }
    if let Some(message) = message {
        crate::agent_mailbox::validate_message(message)
            .map_err(|e| WorkbenchError::new("invalid_input", e.to_string()))?;
    }
    let row = saved(state, id)?;
    if !["starting", "running"]
        .iter()
        .any(|s| row["item"]["state"] == *s)
    {
        return Err(WorkbenchError::new(
            "invalid_state",
            "This managed attempt has no running process to send a follow-up to.",
        ));
    }
    follow_up_answer(call(OP_CONTINUE, continue_params(id, request_id, message)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::git_cli::git_global;
    use std::sync::Arc;

    #[cfg(unix)]
    #[test]
    #[ignore = "requires explicit built Manvi/dcstore and installed Codex; sends one read-only model turn"]
    fn installed_managed_codex_crosses_native_host_and_store_without_accepting_task() {
        installed_managed_provider_crosses_native_host_and_store("codex");
    }

    /// The same crossing for Claude Code.
    ///
    /// A separate `#[ignore]`d test rather than a loop inside one, because the
    /// two need different binaries present and each should be runnable — and
    /// reportable — on its own. Both call the same body, so the assertions
    /// about what a managed run must never do cannot drift apart.
    #[cfg(unix)]
    #[test]
    #[ignore = "requires explicit built Manvi/dcstore and installed Claude Code; sends one read-only model turn"]
    fn installed_managed_claude_crosses_native_host_and_store_without_accepting_task() {
        installed_managed_provider_crosses_native_host_and_store("claude");
    }

    #[cfg(unix)]
    fn installed_managed_provider_crosses_native_host_and_store(provider: &str) {
        use crate::harness::sidecar::{bind_test_binary, test_serial};
        use std::os::unix::fs::PermissionsExt;
        let serial = test_serial();
        let binary = std::fs::canonicalize(
            std::env::var("GITPULSE_WORKBENCH_TEST_MANVI").expect("Supply Manvi"),
        )
        .unwrap();
        let store = std::fs::canonicalize(
            std::env::var("GITPULSE_WORKBENCH_TEST_DCSTORE").expect("Supply dcstore"),
        )
        .unwrap();
        let (variable, supply) = match provider {
            "codex" => ("GITPULSE_WORKBENCH_TEST_CODEX", "MANVI_CODEX_BINARY"),
            "claude" => ("GITPULSE_WORKBENCH_TEST_CLAUDE", "MANVI_CLAUDE_BINARY"),
            other => panic!("no managed adapter for {other}"),
        };
        let agent = std::fs::canonicalize(
            std::env::var(variable).unwrap_or_else(|_| panic!("Supply {provider} via {variable}")),
        )
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let quote =
            |p: &std::path::Path| format!("'{}'", p.to_str().unwrap().replace('\'', "'\\''"));
        let wrapper = dir.path().join("managed-manvi");
        std::fs::write(
            &wrapper,
            format!(
                "#!/bin/sh\nexport MANVI_STORE_BINARY={}\nexport {supply}={}\nexec {} \"$@\"\n",
                quote(&store),
                quote(&agent),
                quote(&binary)
            ),
        )
        .unwrap();
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();
        let _binary = bind_test_binary(&serial, wrapper.to_str().unwrap());
        let host = WorkbenchState(Arc::new(super::super::Inner {
            path: Some(dir.path().join("profile.sqlite")),
            ..Default::default()
        }));
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let root = dir.path().join("checkout");
            git_global(&["init", root.to_str().unwrap()]).unwrap();
            crate::test_support::trust_repo(&root);
            host.register(root.to_str().unwrap(), "repo", "register")
                .unwrap();
            host.request("items.put", &json!({"id":"task","request_id":"task","expected_revision":0,"title":"Verify the managed response path","description":"Reply with MANVI_NATIVE_RUN_OK only. Do not call tools or change files.","repository_ids":["repo"],"primary_repository_id":"repo"}).to_string()).unwrap();
            host.request("runs.prepare_managed", &json!({"id":"run","request_id":"prepare","task_id":"task","source_revision":1,"repository_id":"repo","repository_revision":1,"provider":provider,"permission_mode":"inspect","repo_path":root}).to_string()).unwrap();
            let active = host
                .request("runs.launch_managed", r#"{"id":"run"}"#)
                .unwrap();
            assert_eq!(active["item"]["state"], "running");
            let session = active["item"]["session_id"].clone();
            assert!(active["item"]["process_start"]
                .as_str()
                .is_some_and(|v| !v.is_empty()));
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(55);
            let result = loop {
                let row = host.request("runs.get", r#"{"id":"run"}"#).unwrap();
                if row["item"]["state"] != "running" {
                    break row;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "managed run did not finish"
                );
                std::thread::sleep(std::time::Duration::from_millis(100));
            };
            assert_eq!(
                result["item"]["state"], "exited",
                "{}",
                result["item"]["reason"]
            );
            assert_eq!(
                result["item"]["provider_state"], "completed",
                "{}",
                result["item"]
            );
            assert_eq!(result["item"]["session_id"], session);
            assert!(result["item"]["output"]
                .as_str()
                .unwrap()
                .contains("MANVI_NATIVE_RUN_OK"));
            // The configuration the adapter verified, not one it assumed. The
            // build identity can only have come from the provider's own
            // handshake, so this is what separates "a model answered" from "a
            // fixture answered"; the sandbox line is here because Claude Code
            // has none and a managed run must not imply otherwise.
            let configuration = result["item"]["effective_configuration"]
                .as_str()
                .expect("the run recorded no effective configuration");
            let configuration: Value = serde_json::from_str(configuration).unwrap();
            // Symlink-resolved, because the adapter resolves the checkout
            // before handing it to the provider and reports what it resolved.
            let resolved = std::fs::canonicalize(&root).unwrap();
            assert_eq!(configuration["cwd"], resolved.to_str().unwrap());
            let agent = configuration["provider_user_agent"].as_str().unwrap();
            match provider {
                "claude" => {
                    assert!(
                        agent.starts_with("claude-code/") && agent.len() > "claude-code/".len(),
                        "provider identity was {agent}"
                    );
                    assert_eq!(configuration["approvalPolicy"], "plan");
                    // Claude Code has no OS sandbox. `inspect` is enforced by
                    // its `plan` permission mode, and this record must not
                    // imply a confinement that does not exist.
                    assert_eq!(configuration["sandbox"]["type"], "none");
                    assert_eq!(configuration["sandbox"]["networkAccess"], true);
                    // Empty on purpose, and this asserts the limit rather than
                    // hiding it: the CLI names its model only on a frame that
                    // exists once a turn is running, which is after this record
                    // is written and the store has made it immutable. What the
                    // record carries is what could be verified beforehand.
                    assert_eq!(configuration["model"], "");
                }
                _ => {
                    assert!(!agent.is_empty());
                    assert_eq!(configuration["sandbox"]["type"], "readOnly");
                }
            }
            assert_eq!(
                host.request("runs.launch_managed", r#"{"id":"run"}"#)
                    .unwrap(),
                result
            );
            let task = host.request("items.get", r#"{"id":"task"}"#).unwrap();
            assert_eq!(task["item"]["revision"], 1);
            assert_eq!(task["item"]["status"], "inbox");
            assert_eq!(
                std::fs::read_dir(root).unwrap().count(),
                1,
                "provider changed the empty checkout"
            );
        }));
        host.shutdown();
        if let Err(panic) = outcome {
            std::panic::resume_unwind(panic);
        }
    }

    /// Every provider with a managed adapter reaches launch, and one without
    /// is refused as belonging to another adapter.
    ///
    /// Parameterised rather than written for one provider, because `saved` is
    /// the gate that decides which stored attempts this native host will drive
    /// — and a gate named after a single provider is exactly how the lane
    /// stayed shut for the other one.
    #[test]
    fn managed_controls_accept_every_provider_with_an_adapter_and_refuse_the_rest() {
        for (provider, kind, expected) in [
            // Both managed adapters must get past `saved` and be refused only
            // because the attempt has no process running yet.
            ("codex", "managed", "invalid_state"),
            ("claude", "managed", "invalid_state"),
            // A terminal attempt belongs to a different adapter whichever
            // provider it names, and must never reach the managed controls.
            ("claude", "external_terminal", "run_kind_mismatch"),
            ("grok", "external_terminal", "run_kind_mismatch"),
        ] {
            let label = format!("{provider}/{kind}");
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().join("repo");
            git_global(&["init", root.to_str().unwrap()]).unwrap();
            crate::test_support::trust_repo(&root);
            let state = WorkbenchState(Arc::new(super::super::Inner {
                path: Some(dir.path().join("profile.sqlite")),
                ..Default::default()
            }));
            state
                .register(root.to_str().unwrap(), "repo", "register")
                .unwrap();
            state.request("items.put", &json!({"id":"task","request_id":"task","expected_revision":0,"title":"Inspect repository","repository_ids":["repo"],"primary_repository_id":"repo"}).to_string()).unwrap();
            let method = if kind == "managed" {
                "runs.prepare_managed"
            } else {
                "runs.prepare_terminal"
            };
            state.request(method, &json!({"id":"run","request_id":"prepare","task_id":"task","source_revision":1,"repository_id":"repo","repository_revision":1,"provider":provider,"permission_mode":"inspect","repo_path":root}).to_string()).unwrap_or_else(|e| panic!("{label}: {} {}", e.code, e.message));
            // `runs.stop_managed` and not `runs.launch_managed`: both read the
            // same `saved` gate, but only stop reaches its verdict without
            // asking the harness to start a model session — which, on a
            // machine that has one installed, a gate test would really do.
            let error = state
                .request("runs.stop_managed", r#"{"id":"run"}"#)
                .err()
                .unwrap_or_else(|| panic!("{label}: stop reported success"));
            assert_eq!(error.code, expected, "{label}: {}", error.message);
        }
    }

    #[test]
    fn managed_launch_uses_native_birth_and_retries_without_another_activation() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        git_global(&["init", root.to_str().unwrap()]).unwrap();
        crate::test_support::trust_repo(&root);
        let state = WorkbenchState(Arc::new(super::super::Inner {
            path: Some(dir.path().join("profile.sqlite")),
            ..Default::default()
        }));
        state
            .register(root.to_str().unwrap(), "repo", "register")
            .unwrap();
        state.request("items.put", &json!({"id":"task","request_id":"task","expected_revision":0,"title":"Inspect repository","repository_ids":["repo"],"primary_repository_id":"repo"}).to_string()).unwrap();
        let prepare = json!({"id":"run","request_id":"prepare","task_id":"task","source_revision":1,"repository_id":"repo","repository_revision":1,"provider":"codex","permission_mode":"inspect","repo_path":root});
        let ready = state
            .request("runs.prepare_managed", &prepare.to_string())
            .unwrap();
        assert_eq!(ready["item"]["kind"], "managed");
        let pid = std::process::id();
        let birth = process_birth::read(pid).unwrap();
        let mut activations = 0;
        let result = launch_with(&state, r#"{"id":"run"}"#, |op, params| {
            match op {
                "work.runs.managed.prepare" => {
                    assert_eq!(params["request_id"], "managed_launch_run");
                    state.with_store(|store| query(store, "runs.claim", &json!({"id":"run","request_id":"claim","expected_revision":1,"kind":"managed","owner_id":"owner","session_id":"session"}).to_string()))?;
                    Ok(json!({"protocol_version":2,"id":"run","owner_id":"owner","session_id":"session","process_id":pid,"phase":"awaiting_activation"}))
                }
                "work.runs.managed.activate" => {
                    activations += 1;
                    assert_eq!(params["process_start"], birth);
                    state.with_store(|store| query(store, "runs.started", &json!({"id":"run","request_id":"started","expected_revision":2,"owner_id":"owner","session_id":"session","process_id":params["process_id"],"process_start":params["process_start"]}).to_string()))
                }
                _ => panic!("unexpected operation {op}"),
            }
        }, process_birth::read).unwrap();
        assert_eq!(result["item"]["state"], "running");
        assert_eq!(activations, 1);
        let replay = launch_with(
            &state,
            r#"{"id":"run"}"#,
            |_, _| panic!("retry started a provider"),
            |_| panic!("retry observed another process"),
        )
        .unwrap();
        assert_eq!(result, replay);
        assert_eq!(
            state.request("items.get", r#"{"id":"task"}"#).unwrap()["item"]["status"],
            "inbox"
        );
    }

    /// Builds a registered repository, a task, and one prepared managed
    /// attempt — the state a reader is in the instant before a launch.
    fn prepared_attempt(
        dir: &std::path::Path,
        provider: &str,
    ) -> (WorkbenchState, std::path::PathBuf) {
        let root = dir.join("repo");
        git_global(&["init", root.to_str().unwrap()]).unwrap();
        crate::test_support::trust_repo(&root);
        let state = WorkbenchState(Arc::new(super::super::Inner {
            path: Some(dir.join("profile.sqlite")),
            ..Default::default()
        }));
        state
            .register(root.to_str().unwrap(), "repo", "register")
            .unwrap();
        state.request("items.put", &json!({"id":"task","request_id":"task","expected_revision":0,"title":"Inspect repository","repository_ids":["repo"],"primary_repository_id":"repo"}).to_string()).unwrap();
        state.request("runs.prepare_managed", &json!({"id":"run","request_id":"prepare","task_id":"task","source_revision":1,"repository_id":"repo","repository_revision":1,"provider":provider,"permission_mode":"inspect","repo_path":root}).to_string()).unwrap();
        (state, root)
    }

    /// A preparation the harness refuses must hand the repository back.
    ///
    /// It did not. The attempt was stored before the harness was asked, the
    /// refusal returned straight to the caller, and the row stayed at
    /// `prepared` — which the store counts as occupying this repository's
    /// single run slot until a five-minute expiry. Nothing would ever move it:
    /// the harness had not claimed it, so the harness would never finish it.
    /// One refused launch therefore locked the repository out of *every*
    /// further attempt, including the corrected one the reader was about to
    /// make, and answered them all with "this repository already has a
    /// prepared, active or unresolved run".
    ///
    /// The assertion that matters is the last one: not merely that the row
    /// changed state, but that a *new* attempt can be prepared straight away.
    #[test]
    fn a_refused_preparation_hands_the_repository_back_instead_of_holding_it() {
        let dir = tempfile::tempdir().unwrap();
        let (state, root) = prepared_attempt(dir.path(), "claude");
        let refusal = launch_with(
            &state,
            r#"{"id":"run"}"#,
            |op, _| {
                assert_eq!(op, "work.runs.managed.prepare");
                Err(WorkbenchError::new(
                    "unsupported_operation",
                    "a managed attempt must name a provider with a managed adapter",
                ))
            },
            |_| panic!("a refused preparation observed a process"),
        )
        .expect_err("a refused preparation reported success");
        assert_eq!(refusal.code, "unsupported_operation");

        let row = state.request("runs.get", r#"{"id":"run"}"#).unwrap();
        assert_eq!(
            row["item"]["state"], "cancelled",
            "an unclaimed attempt survived its own refusal: {row}"
        );

        // The point of all of it: the next attempt is not refused as busy.
        state.request("runs.prepare_managed", &json!({"id":"run2","request_id":"prepare2","task_id":"task","source_revision":1,"repository_id":"repo","repository_revision":1,"provider":"claude","permission_mode":"inspect","repo_path":root}).to_string())
            .expect("the repository was still held by a refused preparation");
    }

    /// A checkout that moved before launch hands the repository back too.
    ///
    /// The re-check ahead of Manvi returned its refusal with a bare `?`, so
    /// the row stayed `prepared` and held the slot until it expired; the
    /// retry sweep never touches `prepared`, and every re-preparation in that
    /// checkout was refused as busy — the lock-out the refusal path above
    /// was written to end, reached by another door.
    #[test]
    fn a_checkout_that_moved_before_launch_hands_the_repository_back() {
        let dir = tempfile::tempdir().unwrap();
        let (state, root) = prepared_attempt(dir.path(), "claude");
        git_global(&[
            "-C",
            root.to_str().unwrap(),
            "symbolic-ref",
            "HEAD",
            "refs/heads/elsewhere",
        ])
        .unwrap();
        let refusal = launch_with(
            &state,
            r#"{"id":"run"}"#,
            |_, _| panic!("a moved checkout reached Manvi"),
            |_| panic!("a moved checkout observed a process"),
        )
        .expect_err("a moved checkout launched");
        assert_eq!(refusal.code, "checkout_changed");
        let row = state.request("runs.get", r#"{"id":"run"}"#).unwrap();
        assert_eq!(
            row["item"]["state"], "cancelled",
            "a refused launch kept holding the checkout: {row}"
        );
    }

    /// Only an *unclaimed* attempt is released.
    ///
    /// Once the harness sets `owner_id` there is a live Manvi session holding
    /// a provider against that row, and its own failure path resolves it.
    /// Cancelling underneath it would race a running process and turn a
    /// recoverable attempt into a lost one, so a failure after the claim
    /// leaves the row exactly where its owner can still find it.
    #[test]
    fn an_attempt_already_claimed_is_left_to_the_session_that_owns_it() {
        let dir = tempfile::tempdir().unwrap();
        let (state, _) = prepared_attempt(dir.path(), "codex");
        let failure = launch_with(
            &state,
            r#"{"id":"run"}"#,
            |op, _| {
                assert_eq!(op, "work.runs.managed.prepare");
                // Claims, then dies — a provider that started and failed.
                state.with_store(|store| query(store, "runs.claim", &json!({"id":"run","request_id":"claim","expected_revision":1,"kind":"managed","owner_id":"owner","session_id":"session"}).to_string()))?;
                Err(WorkbenchError::new("process_unavailable", "the provider exited during startup"))
            },
            |_| panic!("a failed preparation observed a process"),
        )
        .expect_err("a failed preparation reported success");
        assert_eq!(failure.code, "process_unavailable");
        assert_eq!(
            state.request("runs.get", r#"{"id":"run"}"#).unwrap()["item"]["state"],
            "starting",
            "a claimed attempt was cancelled out from under its owning session"
        );
    }

    #[test]
    fn incompatible_or_unobservable_preparation_is_stopped_without_activation() {
        for case in ["old-host", "future-host", "wrong-phase", "missing-process"] {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().join("repo");
            git_global(&["init", root.to_str().unwrap()]).unwrap();
            crate::test_support::trust_repo(&root);
            let state = WorkbenchState(Arc::new(super::super::Inner {
                path: Some(dir.path().join("profile.sqlite")),
                ..Default::default()
            }));
            state
                .register(root.to_str().unwrap(), "repo", "register")
                .unwrap();
            state.request("items.put", &json!({"id":"task","request_id":"task","expected_revision":0,"title":"Inspect repository","repository_ids":["repo"],"primary_repository_id":"repo"}).to_string()).unwrap();
            state.request("runs.prepare_managed", &json!({"id":"run","request_id":"prepare","task_id":"task","source_revision":1,"repository_id":"repo","repository_revision":1,"provider":"codex","permission_mode":"inspect","repo_path":root}).to_string()).unwrap();
            let pid = std::process::id();
            let mut stopped = 0;
            let error = launch_with(&state, r#"{"id":"run"}"#, |op, params| match op {
                "work.runs.managed.prepare" => {
                    state.with_store(|store| query(store, "runs.claim", &json!({"id":"run","request_id":"claim","expected_revision":1,"kind":"managed","owner_id":"owner","session_id":"session"}).to_string()))?;
                    let mut receipt = json!({"protocol_version":2,"id":"run","owner_id":"owner","session_id":"session","process_id":pid,"phase":"awaiting_activation"});
                    match case {
                        "old-host" => {
                            receipt.as_object_mut().unwrap().remove("protocol_version");
                            receipt["phase"] = json!("ready");
                        }
                        "future-host" => receipt["protocol_version"] = json!(3),
                        "wrong-phase" => receipt["phase"] = json!("ready"),
                        _ => (),
                    }
                    Ok(receipt)
                }
                "work.runs.managed.stop" => {
                    stopped += 1;
                    assert_eq!(params, json!({"id":"run"}));
                    Ok(json!({"ok":true}))
                }
                _ => panic!("unsafe activation for {case}: {op}"),
            }, |observed| {
                if case == "old-host" { return Ok("fixture-birth".into()); }
                assert_eq!(case, "missing-process", "incompatible host reached process observation");
                assert_eq!(observed,pid);
                Err("Process no longer exists".into())
            }).unwrap_err();
            assert_eq!(stopped, 1, "unactivated helper was not stopped for {case}");
            assert_eq!(
                error.code,
                if case == "missing-process" {
                    "process_unavailable"
                } else {
                    "protocol_error"
                }
            );
            assert_eq!(saved(&state, "run").unwrap()["item"]["state"], "starting");
        }
    }

    #[test]
    fn managed_controls_reject_renderer_process_identity_before_any_storage_or_spawn() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("absent/profile.sqlite");
        let state = WorkbenchState(Arc::new(super::super::Inner {
            path: Some(path.clone()),
            ..Default::default()
        }));
        for input in [
            "null",
            "{}",
            r#"{"id":"run","process_id":123}"#,
            r#"{"id":"run","id":"other"}"#,
            r#"{"id":"run","skip_permissions":true}"#,
        ] {
            for method in ["runs.launch_managed", "runs.stop_managed"] {
                assert_eq!(
                    state.request(method, input).unwrap_err().code,
                    "invalid_input"
                );
            }
        }
        assert!(!path.parent().unwrap().exists());
        assert!(state.0.worker.lock().unwrap().is_none());
    }

    fn refused(code: &str) -> Result<Value, WorkbenchError> {
        Err(WorkbenchError::new(code, "Manvi said no"))
    }

    #[test]
    fn manvi_answers_become_results_the_panel_can_read() {
        let delivered = follow_up_answer(Ok(json!({
            "state": "delivered", "request_id": "r1", "turn": 2, "max_turns": 3, "idle_deadline": 1_700_000_600_i64,
            "future_field": "ignored",
        })))
        .unwrap();
        assert_eq!(
            delivered,
            json!({"ok": true, "state": "delivered", "request_id": "r1", "turn": 2, "max_turns": 3, "idle_deadline": 1_700_000_600_i64})
        );
        let parked =
            follow_up_answer(Ok(json!({"state": "parked", "turn": 1, "max_turns": 3}))).unwrap();
        assert_eq!(parked["state"], "parked");
        assert!(parked.get("request_id").is_none() && parked.get("idle_deadline").is_none());
        for state in ["running", "unresolved", "ended"] {
            let answer =
                follow_up_answer(Ok(json!({"state": state, "turn": 0, "max_turns": 1}))).unwrap();
            assert_eq!(answer["ok"], true, "{state}");
        }
    }

    #[test]
    fn an_answer_that_is_not_one_manvi_gives_is_a_protocol_error_not_a_result() {
        for bad in [
            json!({}),
            json!({"state": "parked"}),
            json!({"state": "parked", "turn": -1, "max_turns": 2}),
            json!({"state": "parked", "turn": "1", "max_turns": 2}),
            json!({"state": "flying", "turn": 1, "max_turns": 2}),
            json!(null),
        ] {
            let error = follow_up_answer(Ok(bad.clone())).expect_err(&bad.to_string());
            assert_eq!(error.code, "protocol_error", "{bad}");
        }
    }

    /// The case this exists for: an installed Manvi that predates the op.
    /// `ProfileConnection` refuses a call the handshake did not list with
    /// `unsupported_operation`; that is a fact about Manvi, not a failed run.
    #[test]
    fn a_manvi_without_the_op_is_a_truthful_unsupported_state_and_not_an_error() {
        let answer = follow_up_answer(refused("unsupported_operation")).unwrap();
        assert_eq!(answer["ok"], false);
        assert_eq!(answer["state"], "unsupported");
        assert_eq!(answer["cause"], "manvi_too_old");
        assert_eq!(answer["retryable"], false);
        assert!(answer["reason"]
            .as_str()
            .unwrap()
            .contains("Manvi version cannot take follow-ups"));
        // A run prepared without `max_continuations` is a different truth.
        let unprepared = follow_up_answer(refused("E_CONTINUE_UNSUPPORTED")).unwrap();
        assert_eq!(unprepared["state"], "unsupported");
        assert_eq!(unprepared["cause"], "not_prepared");
        assert_ne!(unprepared["reason"], answer["reason"]);
    }

    #[test]
    fn only_the_three_curable_refusals_are_retryable() {
        for code in ["E_TURN_IN_FLIGHT", "E_CONTINUE_RATE", "E_CONTINUE_PENDING"] {
            let answer = follow_up_answer(refused(code)).unwrap();
            assert_eq!(
                (answer["state"].as_str(), answer["retryable"].clone()),
                (Some("refused"), json!(true)),
                "{code}"
            );
            assert_eq!(answer["code"], code);
        }
        for code in [
            "E_RUN_ENDED",
            "E_DEPENDENCY",
            "E_CONTINUE_LIMIT",
            "E_CONTINUE_UNRESOLVED",
            "E_SOMETHING_NEW",
        ] {
            let answer = follow_up_answer(refused(code)).unwrap();
            assert_eq!(
                (answer["state"].as_str(), answer["retryable"].clone()),
                (Some("refused"), json!(false)),
                "{code}"
            );
        }
    }

    #[test]
    fn a_call_that_never_got_an_answer_stays_an_error() {
        for code in [
            "timeout",
            "unavailable",
            "busy",
            "not_installed",
            "protocol_error",
        ] {
            let error = follow_up_answer(refused(code)).expect_err(code);
            assert_eq!(error.code, code);
        }
    }

    #[test]
    fn the_params_name_exactly_the_keys_the_contract_declares() {
        assert_eq!(continue_params("run", None, None), json!({"id": "run"}));
        assert_eq!(
            continue_params("run", Some("r1"), Some("hello")),
            json!({"id": "run", "request_id": "r1", "message": "hello"})
        );
    }

    /// A follow-up reaches Manvi only for a live managed attempt, with a valid
    /// message and request id, and carries exactly what the person approved.
    #[test]
    fn a_follow_up_is_sent_only_for_a_live_managed_run_with_a_valid_message() {
        let dir = tempfile::tempdir().unwrap();
        let (state, _) = prepared_attempt(dir.path(), "claude");
        let never =
            |_: &str, _: Value| -> Result<Value, WorkbenchError> { panic!("reached Manvi") };
        // Prepared: no process yet.
        assert_eq!(
            follow_up_with(&state, "run", Some("r1"), Some("hi"), never)
                .unwrap_err()
                .code,
            "invalid_state"
        );
        state.with_store(|store| query(store, "runs.claim", &json!({"id":"run","request_id":"claim","expected_revision":1,"kind":"managed","owner_id":"owner","session_id":"session"}).to_string())).unwrap();
        for (request, message) in [
            (None, Some("hi")),
            (Some("r1"), None),
            (Some("r 1"), Some("hi")),
            (Some("r1"), Some("")),
            (Some("r1"), Some("   ")),
            (Some("r1"), Some("bell \u{7}")),
        ] {
            let error = follow_up_with(&state, "run", request, message, never).unwrap_err();
            assert_eq!(error.code, "invalid_input", "{request:?} {message:?}");
        }
        let too_long = "x".repeat(crate::agent_mailbox::MAX_MESSAGE_BYTES + 1);
        assert_eq!(
            follow_up_with(&state, "run", Some("r1"), Some(&too_long), never)
                .unwrap_err()
                .code,
            "invalid_input"
        );
        assert_eq!(
            follow_up_with(&state, "no/such", None, None, never)
                .unwrap_err()
                .code,
            "invalid_input"
        );

        let mut seen = None;
        let answer = follow_up_with(
            &state,
            "run",
            Some("r1"),
            Some("CI failed.\nSee the run."),
            |op, params| {
                seen = Some((op.to_owned(), params));
                Ok(json!({"state": "delivered", "request_id": "r1", "turn": 2, "max_turns": 3}))
            },
        )
        .unwrap();
        assert_eq!(answer["state"], "delivered");
        assert_eq!(
            seen,
            Some((
                "work.runs.managed.continue".to_owned(),
                json!({"id": "run", "request_id": "r1", "message": "CI failed.\nSee the run."})
            ))
        );
        // The status read sends no message and no request id.
        let mut read = None;
        follow_up_with(&state, "run", None, None, |_, params| {
            read = Some(params);
            Ok(json!({"state": "parked", "turn": 1, "max_turns": 3}))
        })
        .unwrap();
        assert_eq!(read, Some(json!({"id": "run"})));
    }

    #[test]
    fn a_terminal_attempt_is_never_sent_a_managed_follow_up() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        git_global(&["init", root.to_str().unwrap()]).unwrap();
        crate::test_support::trust_repo(&root);
        let state = WorkbenchState(Arc::new(super::super::Inner {
            path: Some(dir.path().join("profile.sqlite")),
            ..Default::default()
        }));
        state
            .register(root.to_str().unwrap(), "repo", "register")
            .unwrap();
        state.request("items.put", &json!({"id":"task","request_id":"task","expected_revision":0,"title":"Inspect repository","repository_ids":["repo"],"primary_repository_id":"repo"}).to_string()).unwrap();
        state.request("runs.prepare_terminal", &json!({"id":"run","request_id":"prepare","task_id":"task","source_revision":1,"repository_id":"repo","repository_revision":1,"provider":"claude","permission_mode":"inspect","repo_path":root}).to_string()).unwrap();
        let error = follow_up_with(&state, "run", Some("r1"), Some("hi"), |_, _| {
            panic!("reached Manvi")
        })
        .unwrap_err();
        assert_eq!(error.code, "run_kind_mismatch");
    }
}
