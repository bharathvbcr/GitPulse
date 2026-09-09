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

fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 100
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
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
    if row["item"]["kind"] != "managed" || row["item"]["provider"] != "codex" {
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
    let _launch = state.0.managed_launch.try_lock().map_err(|_| {
        WorkbenchError::new(
            "busy",
            "A managed launch is already being prepared. Retry shortly.",
        )
    })?;
    launch_with(
        state,
        input,
        |op, params| state.worker_call(op, params),
        process_birth::read,
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
    terminal_launch::revalidate(&row)?;
    // Stable for this attempt across UI reloads. The store forbids repeating a
    // consumed claim; only the owning live Manvi session can recover this call.
    let prepared = call(
        "work.runs.managed.prepare",
        json!({"id":id,"request_id":format!("managed_launch_{id}"),"expected_revision":1}),
    )?;
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
    state.worker_call("work.runs.managed.stop", json!({"id":id}))
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
        use crate::harness::sidecar::{set_test_binary, test_serial};
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
        let codex = std::fs::canonicalize(
            std::env::var("GITPULSE_WORKBENCH_TEST_CODEX").expect("Supply Codex"),
        )
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let quote =
            |p: &std::path::Path| format!("'{}'", p.to_str().unwrap().replace('\'', "'\\''"));
        let wrapper = dir.path().join("managed-manvi");
        std::fs::write(&wrapper, format!("#!/bin/sh\nexport MANVI_STORE_BINARY={}\nexport MANVI_CODEX_BINARY={}\nexec {} \"$@\"\n",quote(&store),quote(&codex),quote(&binary))).unwrap();
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();
        set_test_binary(&serial, Some(wrapper.to_str().unwrap().into()));
        let host = WorkbenchState(Arc::new(super::super::Inner {
            path: Some(dir.path().join("profile.sqlite")),
            ..Default::default()
        }));
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let root = dir.path().join("checkout");
            git_global(&["init", root.to_str().unwrap()]).unwrap();
            host.register(root.to_str().unwrap(), "repo", "register")
                .unwrap();
            host.request("items.put", &json!({"id":"task","request_id":"task","expected_revision":0,"title":"Verify the managed response path","description":"Reply with MANVI_NATIVE_RUN_OK only. Do not call tools or change files.","repository_ids":["repo"],"primary_repository_id":"repo"}).to_string()).unwrap();
            host.request("runs.prepare_managed", &json!({"id":"run","request_id":"prepare","task_id":"task","source_revision":1,"repository_id":"repo","repository_revision":1,"provider":"codex","permission_mode":"inspect","repo_path":root}).to_string()).unwrap();
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
            assert_eq!(result["item"]["provider_state"], "completed");
            assert_eq!(result["item"]["session_id"], session);
            assert!(result["item"]["output"]
                .as_str()
                .unwrap()
                .contains("MANVI_NATIVE_RUN_OK"));
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
        set_test_binary(&serial, None);
        if let Err(panic) = outcome {
            std::panic::resume_unwind(panic);
        }
    }

    #[test]
    fn managed_launch_uses_native_birth_and_retries_without_another_activation() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        git_global(&["init", root.to_str().unwrap()]).unwrap();
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

    #[test]
    fn incompatible_or_unobservable_preparation_is_stopped_without_activation() {
        for case in ["old-host", "future-host", "wrong-phase", "missing-process"] {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().join("repo");
            git_global(&["init", root.to_str().unwrap()]).unwrap();
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
        assert!(state.0.worker.get().is_none());
    }
}
