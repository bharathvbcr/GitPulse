//! Native profile adapter. Manvi owns every schema, query and transaction.
//! Data reads never start a model, watcher or execution lease holder. Resuming
//! saved automatic work uses an explicit, bounded wake request to the Go host.

use dc_store::Store;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use tauri::{Emitter, State};

mod managed_run;
pub(crate) mod notifications;
mod process_birth;
mod terminal_command;
mod terminal_launch;
mod terminal_run;

const MAX_IN_FLIGHT: usize = 8;
const MAX_INPUT: usize = 256 * 1024;

#[derive(Debug, Serialize)]
pub struct WorkbenchError {
    pub code: String,
    pub message: String,
}

impl WorkbenchError {
    fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

impl From<dc_store::workbench::Error> for WorkbenchError {
    fn from(error: dc_store::workbench::Error) -> Self {
        Self::new(error.code, error.message)
    }
}

#[derive(Default)]
struct Inner {
    store: Mutex<Option<Store>>,
    closed: AtomicBool,
    in_flight: AtomicUsize,
    // Only tests supply a path. IPC cannot select a repository lease database.
    path: Option<PathBuf>,
    notifications: OnceLock<notifications::Coordinator>,
    worker:
        OnceLock<Result<crate::harness::sidecar::ProfileConnection, crate::harness::HarnessError>>,
    managed_launch: Mutex<()>,
}

#[derive(Clone, Default)]
pub struct WorkbenchState(Arc<Inner>);

struct Reservation(Arc<Inner>);
impl Drop for Reservation {
    fn drop(&mut self) {
        self.0.in_flight.fetch_sub(1, Ordering::AcqRel);
    }
}

impl WorkbenchState {
    fn check_open(&self) -> Result<(), WorkbenchError> {
        if self.0.closed.load(Ordering::Acquire) {
            return Err(WorkbenchError::new(
                "closed",
                "The task host is shutting down.",
            ));
        }
        Ok(())
    }
    fn profile_path(&self) -> Result<PathBuf, WorkbenchError> {
        let path = match &self.0.path {
            Some(path) => path.clone(),
            None => crate::tool_config::default_config_dir()
                .ok_or_else(|| {
                    WorkbenchError::new(
                        "store_error",
                        "Cannot resolve the GitPulse profile directory.",
                    )
                })?
                .join("workbench.sqlite"),
        };
        if !path.is_absolute() {
            return Err(WorkbenchError::new(
                "store_error",
                "The profile database path must be absolute.",
            ));
        }
        let parent = path
            .parent()
            .ok_or_else(|| WorkbenchError::new("store_error", "Invalid profile database path."))?;
        std::fs::create_dir_all(parent)
            .map_err(|e| WorkbenchError::new("store_error", e.to_string()))?;
        Ok(path)
    }

    pub(crate) fn shutdown(&self) {
        self.0.closed.store(true, Ordering::Release);
        if let Some(notifications) = self.0.notifications.get() {
            notifications.wake();
        }
        if let Some(Ok(worker)) = self.0.worker.get() {
            if !worker.shutdown() {
                log::warn!(target: "workbench", "profile worker shutdown could not acquire the active request; generation outcome may remain unresolved");
            }
        }
    }
    fn reserve(&self) -> Result<Reservation, WorkbenchError> {
        self.check_open()?;
        self.0
            .in_flight
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < MAX_IN_FLIGHT).then_some(n + 1)
            })
            .map_err(|_| WorkbenchError::new("busy", "Task storage is busy. Try again shortly."))?;
        Ok(Reservation(Arc::clone(&self.0)))
    }

    fn with_store<T>(
        &self,
        body: impl FnOnce(&Store) -> Result<T, WorkbenchError>,
    ) -> Result<T, WorkbenchError> {
        let mut guard = self.0.store.lock().map_err(|_| {
            WorkbenchError::new(
                "store_error",
                "Task storage lock failed; restart the application.",
            )
        })?;
        self.check_open()?;
        if guard.is_none() {
            let path = self.profile_path()?;
            *guard = Some(
                Store::open(&path)
                    .map_err(|e| WorkbenchError::new("store_error", e.to_string()))?,
            );
        }
        body(
            guard
                .as_ref()
                .ok_or_else(|| WorkbenchError::new("store_error", "Task storage did not open."))?,
        )
    }

    /// Only the native observer of an already claimed process uses this path.
    /// Shutdown refuses new requests, but must retain that process's receipts.
    /// Never lazily creates profile state or accepts an IPC method here.
    fn with_run_receipt<T>(
        &self,
        body: impl FnOnce(&Store) -> Result<T, WorkbenchError>,
    ) -> Result<T, WorkbenchError> {
        let guard =
            self.0.store.lock().map_err(|_| {
                WorkbenchError::new("store_error", "Task receipt storage lock failed.")
            })?;
        let store = guard.as_ref().ok_or_else(|| {
            WorkbenchError::new("store_error", "No open task store owns this process.")
        })?;
        body(store)
    }

    fn request(&self, method: &str, input: &str) -> Result<Value, WorkbenchError> {
        self.check_open()?;
        if input.len() > MAX_INPUT || method.len() > 64 {
            return Err(WorkbenchError::new(
                "invalid_input",
                "Task request exceeds the size limit.",
            ));
        }
        if method.starts_with("notifications.native.") {
            return notifications::native_request(self, method, input);
        }
        if matches!(
            method,
            "notifications.claim"
                | "notifications.finish"
                | "notifications.activate"
                | "decisions.create"
                | "decisions.claim"
                | "decisions.resolve"
                | "runs.started"
                | "runs.finish"
                | "runs.protocol"
                | "runs.managed.prepare"
                | "runs.managed.activate"
        ) {
            return Err(WorkbenchError::new(
                "host_only",
                "Delivery and provider callback records are owned by their host coordinators.",
            ));
        }
        if method == "runs.prepare_terminal" {
            return terminal_launch::prepare(self, input);
        }
        if method == "runs.prepare_managed" {
            return terminal_launch::prepare_managed(self, input);
        }
        if method == "runs.launch_managed" {
            return managed_run::launch(self, input);
        }
        if method == "runs.stop_managed" {
            return managed_run::stop(self, input);
        }
        if method == "runs.claim" {
            return terminal_launch::claim(self, input);
        }
        if matches!(
            method,
            "enhancements.generate"
                | "enhancements.configuration"
                | "enhancements.wake"
                | "enhancements.worker"
        ) {
            let params = generation_input(method, input)?;
            return self.worker_call(&format!("work.{method}"), params);
        }
        let result = self.with_store(|store| query(store, method, input));
        if result.is_ok() && method == "notifications.settings.put" {
            if let Some(notifications) = self.0.notifications.get() {
                notifications.wake();
            }
        }
        result
    }

    fn worker_call(&self, method: &str, params: Value) -> Result<Value, WorkbenchError> {
        self.check_open()?;
        let path = self.profile_path()?;
        let worker = self
            .0
            .worker
            .get_or_init(|| crate::harness::sidecar::ProfileConnection::new(path))
            .as_ref()
            .map_err(|e| worker_error(e.clone()))?;
        // Initialization is lazy and can overlap shutdown; never spawn after it.
        if let Err(error) = self.check_open() {
            worker.shutdown();
            return Err(error);
        }
        worker.call(method, params).map_err(worker_error)
    }

    fn register(
        &self,
        repo_path: &str,
        id: &str,
        request_id: &str,
    ) -> Result<Value, WorkbenchError> {
        self.check_open()?;
        let resolved = crate::engine::git_cli::resolve_repo(repo_path)
            .map_err(|e| WorkbenchError::new("repository_unavailable", e))?;
        let common = crate::engine::git_cli::resolve_git_common_dir(Path::new(&resolved.path))
            .map_err(|e| WorkbenchError::new("repository_unavailable", e))?;
        let identity = format!(
            "local:{}",
            common.to_str().ok_or_else(|| WorkbenchError::new(
                "invalid_input",
                "Repository path is not valid Unicode."
            ))?
        );
        self.with_store(|store| {
            let mut cursor: Option<String> = None;
            let mut pages = 0;
            loop {
                pages += 1;
                if pages > 50 {
                    return Err(WorkbenchError::new("registry_limit", "Repository registration requires an indexed identity lookup beyond 10,000 repositories."));
                }
                let mut input = json!({"limit":200});
                if let Some(cursor) = &cursor { input["cursor"] = json!(cursor); }
                let page = query(store, "repositories.list", &input.to_string())?;
                let records = page["items"].as_array().ok_or_else(|| WorkbenchError::new("protocol_error", "Invalid repository page."))?;
                if let Some(repository) = records.iter().find(|r| r["identity_key"].as_str() == Some(&identity)) {
                    return Ok(json!({"repository":repository,"path":resolved.path,"is_bare":resolved.is_bare}));
                }
                if page["has_more"] != true { break; }
                let next = page["next_cursor"].as_str().ok_or_else(|| WorkbenchError::new("protocol_error", "Missing repository cursor."))?;
                if cursor.as_deref() == Some(next) {
                    return Err(WorkbenchError::new("protocol_error", "Repository cursor did not advance."));
                }
                cursor = Some(next.into());
            }
            let input = json!({"id":id,"request_id":request_id,"expected_revision":0,"name":resolved.name,"identity_key":identity});
            let response = query(store, "repositories.put", &input.to_string())?;
            Ok(json!({"repository":response["item"],"path":resolved.path,"is_bare":resolved.is_bare,"sequence":response["sequence"]}))
        })
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct GenerationInput {
    id: String,
    request_id: String,
    expected_revision: u64,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ConfigurationInput {}

fn generation_input(method: &str, input: &str) -> Result<Value, WorkbenchError> {
    let invalid = |e: serde_json::Error| WorkbenchError::new("invalid_input", e.to_string());
    if input.len() > 4096 {
        return Err(WorkbenchError::new(
            "invalid_input",
            "Generation request exceeds its 4 KiB limit.",
        ));
    }
    // Serde can deserialize a struct from a positional JSON array. Host control
    // requests are objects only; refuse arrays before any profile or process I/O.
    if !input.trim_start().starts_with('{') {
        return Err(WorkbenchError::new(
            "invalid_input",
            "Host control requests must be JSON objects.",
        ));
    }
    if matches!(
        method,
        "enhancements.configuration" | "enhancements.wake" | "enhancements.worker"
    ) {
        let parsed: ConfigurationInput = serde_json::from_str(input).map_err(invalid)?;
        return serde_json::to_value(parsed).map_err(invalid);
    }
    let parsed: GenerationInput = serde_json::from_str(input).map_err(invalid)?;
    let valid_id = |id: &str| {
        !id.is_empty()
            && id.len() <= 128
            && id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
    };
    if !valid_id(&parsed.id)
        || !valid_id(&parsed.request_id)
        || parsed.expected_revision == 0
        || parsed.expected_revision > 9_007_199_254_740_991
    {
        return Err(WorkbenchError::new(
            "invalid_input",
            "Generation requires valid task identities and a positive exact revision.",
        ));
    }
    serde_json::to_value(parsed).map_err(invalid)
}

fn worker_error(error: crate::harness::HarnessError) -> WorkbenchError {
    match error {
        crate::harness::HarnessError::Refused(error) => {
            WorkbenchError::new(&error.code, error.message)
        }
        crate::harness::HarnessError::Busy(message) => WorkbenchError::new("busy", message),
        crate::harness::HarnessError::NotInstalled(message) => {
            WorkbenchError::new("not_installed", message)
        }
        other => WorkbenchError::new("transport_error", other.message()),
    }
}

fn query(store: &Store, method: &str, input: &str) -> Result<Value, WorkbenchError> {
    let response = store.workbench_request(method, input)?;
    serde_json::from_str(&response)
        .map_err(|e| WorkbenchError::new("protocol_error", e.to_string()))
}

fn announce<R: tauri::Runtime>(app: &tauri::AppHandle<R>, response: &Value) {
    if let Some(sequence) = response["sequence"].as_u64() {
        // The durable event is already committed. A lost wake-up is repaired by
        // reload/focus; returning a write failure here would invite duplication.
        if let Err(error) = app.emit("workbench-changed", json!({"sequence":sequence})) {
            log::warn!(target: "workbench", "change notification failed: {error}");
        }
    }
}

#[tauri::command]
pub async fn cmd_workbench_launch_terminal(
    state: State<'_, WorkbenchState>,
    terminals: State<'_, crate::terminal::TerminalSessions>,
    app: tauri::AppHandle,
    input: String,
) -> Result<crate::terminal::TerminalSpawned, WorkbenchError> {
    let launch = terminal_run::parse(&input)?;
    let reservation = state.reserve()?;
    let state = state.inner().clone();
    let terminals = terminals.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _reservation = reservation;
        terminal_run::spawn(&app, &terminals, &state, launch)
    })
    .await
    .map_err(|e| WorkbenchError::new("worker_error", e.to_string()))?
}

#[tauri::command]
pub async fn cmd_workbench_request(
    state: State<'_, WorkbenchState>,
    app: tauri::AppHandle,
    method: String,
    input: String,
) -> Result<String, WorkbenchError> {
    let reservation = state.reserve()?;
    let state = state.inner().clone();
    let response = tauri::async_runtime::spawn_blocking(move || {
        let _reservation = reservation;
        state.request(&method, &input)
    })
    .await
    .map_err(|e| WorkbenchError::new("worker_error", e.to_string()))??;
    announce(&app, &response);
    Ok(response.to_string())
}

#[tauri::command]
pub async fn cmd_workbench_register_repository(
    state: State<'_, WorkbenchState>,
    app: tauri::AppHandle,
    repo_path: String,
    id: String,
    request_id: String,
) -> Result<String, WorkbenchError> {
    if repo_path.len() > 16_384 || id.len() > 128 || request_id.len() > 128 {
        return Err(WorkbenchError::new(
            "invalid_input",
            "Repository request exceeds the size limit.",
        ));
    }
    let reservation = state.reserve()?;
    let state = state.inner().clone();
    let response = tauri::async_runtime::spawn_blocking(move || {
        let _reservation = reservation;
        state.register(&repo_path, &id, &request_id)
    })
    .await
    .map_err(|e| WorkbenchError::new("worker_error", e.to_string()))??;
    announce(&app, &response);
    Ok(response.to_string())
}

#[cfg(test)]
mod tests {
    use super::{generation_input, Inner, WorkbenchState, MAX_IN_FLIGHT};
    use serde_json::json;
    use std::sync::Arc;

    #[test]
    fn automatic_controls_accept_only_empty_objects_before_starting_a_host() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("absent/profile.sqlite");
        let host = state(&path);
        for method in ["enhancements.wake", "enhancements.worker"] {
            assert_eq!(generation_input(method, "{}").unwrap(), json!({}));
            for input in [
                "null",
                "[]",
                "{",
                r#"{"skip_permissions":true}"#,
                r#"{"model":"unrequested"}"#,
            ] {
                assert_eq!(
                    host.request(method, input).unwrap_err().code,
                    "invalid_input"
                );
            }
        }
        assert!(!path.parent().unwrap().exists());
        assert!(host.0.worker.get().is_none());
    }

    fn state(path: &std::path::Path) -> WorkbenchState {
        WorkbenchState(Arc::new(Inner {
            path: Some(path.to_path_buf()),
            ..Inner::default()
        }))
    }

    #[test]
    fn canonical_task_briefs_use_native_storage_without_starting_a_model_worker() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("brief profile/workbench.sqlite");
        let host = state(&path);
        for (id, name) in [("r1", "Primary repository"), ("r2", "Linked repository")] {
            host.request("repositories.put", &json!({"id":id,"request_id":id,"expected_revision":0,"name":name,"identity_key":format!("clone:{id}"),"remote_url":"https://credential@example.test"}).to_string()).unwrap();
        }
        host.request("items.put", r#"{"id":"t","request_id":"t","expected_revision":0,"title":"Keep E42","description":"Exact $(touch NEVER_EXECUTE) 🧪","repository_ids":["r2","r1"],"primary_repository_id":"r1"}"#).unwrap();
        let events = host.request("events.list", "{}").unwrap();
        let brief = host
            .request("items.brief.get", r#"{"id":"t","expected_revision":1}"#)
            .unwrap();
        assert_eq!(
            brief["item"]["repositories"][0]["name"],
            "Linked repository"
        );
        assert_eq!(
            brief["item"]["repositories"][1]["name"],
            "Primary repository"
        );
        assert_eq!(
            brief["item"]["task"]["description"],
            "Exact $(touch NEVER_EXECUTE) 🧪"
        );
        assert!(!brief.to_string().contains("credential@"));
        assert_eq!(host.request("events.list", "{}").unwrap(), events);
        assert!(host.0.worker.get().is_none());
        drop(host);
        let reopened = state(&path);
        assert_eq!(
            reopened
                .request("items.brief.get", r#"{"id":"t","expected_revision":1}"#)
                .unwrap(),
            brief
        );
        assert_eq!(
            reopened
                .request("items.brief.get", r#"{"id":"t","expected_revision":2}"#)
                .unwrap_err()
                .code,
            "revision_conflict"
        );
        assert!(reopened.0.worker.get().is_none());
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "requires explicit GITPULSE_WORKBENCH_TEST_MANVI and GITPULSE_WORKBENCH_TEST_DCSTORE binaries"]
    fn real_profile_host_shares_native_revisions_and_refuses_a_stale_generation() {
        use crate::harness::sidecar::{set_test_binary, test_serial};
        use std::os::unix::fs::PermissionsExt;
        let serial = test_serial();
        let binary = std::fs::canonicalize(
            std::env::var("GITPULSE_WORKBENCH_TEST_MANVI").expect("Supply the built Manvi binary"),
        )
        .unwrap();
        let store = std::fs::canonicalize(
            std::env::var("GITPULSE_WORKBENCH_TEST_DCSTORE")
                .expect("Supply the built dcstore binary"),
        )
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let wrapper = dir.path().join("scoped-manvi");
        let quote =
            |path: &std::path::Path| format!("'{}'", path.to_str().unwrap().replace('\'', "'\\''"));
        // Overrides belong to this child only. Configuration and the stale claim
        // below must not resolve a provider or contact any model endpoint.
        std::fs::write(
            &wrapper,
            format!(
                "#!/bin/sh\nexport MANVI_MODEL=native-fixture-model\nexport MANVI_LLM_PROVIDER_DEFAULT=local\nexport MANVI_STORE_BINARY={}\nexec {} \"$@\"\n",
                quote(&store), quote(&binary)
            ),
        )
        .unwrap();
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();
        set_test_binary(&serial, Some(wrapper.to_str().unwrap().into()));
        let outcome = std::panic::catch_unwind(|| {
            let path = dir.path().join("profile with spaces/workbench.sqlite");
            let host = state(&path);
            let configuration = host.request("enhancements.configuration", "{}").unwrap();
            assert_eq!(configuration["model"], "native-fixture-model");
            assert_eq!(configuration["provider"], "local");
            assert!(!path.exists(), "configuration opened the profile store");
            let status = host.request("enhancements.worker", "{}").unwrap();
            assert_eq!(status["state"], "not_started");
            assert!(!path.exists(), "status opened the profile store");
            host.request(
                "automation.put",
                r#"{"id":"profile","request_id":"disable","expected_revision":1,"enabled":false}"#,
            )
            .unwrap();
            host.request("enhancements.wake", "{}").unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            loop {
                let status = host.request("enhancements.worker", "{}").unwrap();
                if status["state"] == "disabled" {
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "native worker did not read the shared settings: {status}"
                );
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            host.request("repositories.put", r#"{"request_id":"r","id":"r","expected_revision":0,"name":"Native fixture","identity_key":"fixture:r"}"#).unwrap();
            host.request("items.put", r#"{"request_id":"t","id":"t","expected_revision":0,"title":"Keep E42","repository_ids":["r"],"primary_repository_id":"r"}"#).unwrap();
            host.request("enhancements.create", r#"{"request_id":"e","id":"e","expected_revision":0,"task_id":"t","source_revision":1,"fields":["title"],"provider":"local","model":"native-fixture-model"}"#).unwrap();
            host.request("items.put", r#"{"request_id":"edit","id":"t","expected_revision":1,"title":"Keep revised E42 evidence","repository_ids":["r"],"primary_repository_id":"r"}"#).unwrap();
            let error = host
                .request(
                    "enhancements.generate",
                    r#"{"request_id":"start","id":"e","expected_revision":1}"#,
                )
                .unwrap_err();
            assert_eq!(error.code, "revision_conflict");
            let proposal = host.request("enhancements.get", r#"{"id":"e"}"#).unwrap();
            assert_eq!(proposal["item"]["state"], "pending");
            assert_eq!(proposal["item"]["revision"], 1);
            assert_eq!(
                host.request("items.get", r#"{"id":"t"}"#).unwrap()["item"]["revision"],
                2
            );
            host.shutdown();
            assert!(host.request("enhancements.configuration", "{}").is_err());
        });
        set_test_binary(&serial, None);
        if let Err(panic) = outcome {
            std::panic::resume_unwind(panic);
        }
    }

    #[test]
    fn renderer_cannot_forge_provider_callbacks_or_delivery_claims() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("absent/profile.sqlite");
        let host = state(&path);
        for method in [
            "decisions.create",
            "decisions.claim",
            "decisions.resolve",
            "notifications.claim",
            "notifications.finish",
            "notifications.activate",
            "runs.started",
            "runs.finish",
            "runs.protocol",
            "runs.managed.activate",
            "runs.managed.prepare",
        ] {
            assert_eq!(host.request(method, "{}").unwrap_err().code, "host_only");
        }
        assert!(!path.parent().unwrap().exists());
        assert!(host.0.worker.get().is_none());
    }

    #[test]
    fn invalid_generation_never_creates_a_profile_or_starts_a_worker() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("absent/profile.sqlite");
        let host = state(&path);
        for input in [
            r#"{}"#,
            r#"["e","r",1]"#,
            r#"{"id":"e","id":"other","request_id":"r","expected_revision":1}"#,
            r#"{"id":"e","\u0069d":"other","request_id":"r","expected_revision":1}"#,
            r#"{"id":"e","request_id":"r","expected_revision":1,"skip_permissions":true}"#,
            r#"{"id":"../e","request_id":"r","expected_revision":1}"#,
            r#"{"id":"e","request_id":"r","expected_revision":0}"#,
            r#"{"id":"e","request_id":"r","expected_revision":9007199254740992}"#,
            r#"{"id":"e","request_id":"r","expected_revision":1.5}"#,
            r#"{"id":"e\ud800","request_id":"r","expected_revision":1}"#,
        ] {
            assert_eq!(
                host.request("enhancements.generate", input)
                    .unwrap_err()
                    .code,
                "invalid_input"
            );
        }
        assert_eq!(
            host.request("enhancements.configuration", r#"{"profile":"/elsewhere"}"#)
                .unwrap_err()
                .code,
            "invalid_input"
        );
        assert!(host.0.worker.get().is_none());
        assert!(!path.parent().unwrap().exists());
        let parsed = generation_input(
            "enhancements.generate",
            r#"{"id":"e","request_id":"r","expected_revision":1}"#,
        )
        .unwrap();
        assert_eq!(parsed["id"], "e");
    }

    #[test]
    fn shutdown_refuses_queued_lazy_requests_without_creating_profile_state() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("absent/profile.sqlite");
        let host = state(&path);
        host.shutdown();
        host.shutdown();
        assert_eq!(host.reserve().err().unwrap().code, "closed");
        assert_eq!(
            host.register("/missing-repository", "r", "register")
                .unwrap_err()
                .code,
            "closed"
        );
        for method in ["items.list", "enhancements.configuration"] {
            assert_eq!(host.request(method, "{}").unwrap_err().code, "closed");
        }
        assert!(host.0.worker.get().is_none());
        assert!(host.0.store.lock().unwrap().is_none());
        assert!(!path.parent().unwrap().exists());
    }

    #[test]
    fn lazy_profile_survives_restart_and_preserves_typed_conflicts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profile/workbench.sqlite");
        let host = state(&path);
        assert!(!path.exists());
        let repo = json!({"request_id":"r1","id":"r1","expected_revision":0,"name":"Repo","identity_key":"clone-1"});
        host.request("repositories.put", &repo.to_string()).unwrap();
        let task = json!({"request_id":"t1","id":"t1","expected_revision":0,"title":"Keep detailed evidence","description":"Do not drop me","repository_ids":["r1"],"primary_repository_id":"r1"});
        let accepted = host.request("items.put", &task.to_string()).unwrap();
        assert_eq!(
            host.request("items.put", &task.to_string()).unwrap(),
            accepted
        );
        let mut stale = task.clone();
        stale["request_id"] = json!("other");
        assert_eq!(
            host.request("items.put", &stale.to_string())
                .unwrap_err()
                .code,
            "revision_conflict"
        );
        drop(host);
        let reopened = state(&path);
        let card = reopened.request("items.list", "{}").unwrap();
        assert_eq!(card["total"], 1);
        assert!(card["items"][0].get("description").is_none());
        assert_eq!(
            reopened.request("items.get", r#"{"id":"t1"}"#).unwrap()["item"]["description"],
            "Do not drop me"
        );
    }

    #[test]
    fn queue_is_bounded_and_releases_on_drop() {
        let host = WorkbenchState::default();
        let permits: Vec<_> = (0..MAX_IN_FLIGHT)
            .map(|_| host.reserve().unwrap())
            .collect();
        assert!(host.reserve().is_err());
        drop(permits);
        assert!(host.reserve().is_ok());
        assert!(host.0.store.lock().unwrap().is_none());
    }

    #[test]
    fn oversized_request_does_not_create_database() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("not-created.sqlite");
        let error = state(&path)
            .request("items.list", &" ".repeat(256 * 1024 + 1))
            .unwrap_err();
        assert_eq!(error.code, "invalid_input");
        assert!(!path.exists());
    }

    #[test]
    fn registration_deduplicates_linked_worktrees_and_preserves_distinct_clones() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        let linked = dir.path().join("linked");
        let other = dir.path().join("other");
        let git = |cwd: &std::path::Path, args: &[&str]| {
            let output = std::process::Command::new("git")
                .current_dir(cwd)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(dir.path(), &["init", repo.to_str().unwrap()]);
        git(
            &repo,
            &[
                "-c",
                "user.name=Workbench Test",
                "-c",
                "user.email=workbench@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--allow-empty",
                "-m",
                "fixture",
            ],
        );
        git(
            &repo,
            &["worktree", "add", "--detach", linked.to_str().unwrap()],
        );
        git(
            dir.path(),
            &[
                "clone",
                "--local",
                repo.to_str().unwrap(),
                other.to_str().unwrap(),
            ],
        );
        let path = dir.path().join("profile.sqlite");
        let host = state(&path);
        let first = host
            .register(repo.to_str().unwrap(), "repo-1", "register-1")
            .unwrap();
        let alias = host
            .register(linked.to_str().unwrap(), "unused", "register-2")
            .unwrap();
        assert_eq!(first["repository"]["id"], alias["repository"]["id"]);
        let clone = host
            .register(other.to_str().unwrap(), "repo-2", "register-3")
            .unwrap();
        assert_ne!(first["repository"]["id"], clone["repository"]["id"]);
        drop(host);
        let reopened = state(&path);
        let again = reopened
            .register(repo.to_str().unwrap(), "unused-again", "register-4")
            .unwrap();
        assert_eq!(first["repository"], again["repository"]);
        assert_eq!(
            reopened.request("repositories.list", "{}").unwrap()["total"],
            2
        );
        assert!(!repo.join(".devcouncil").exists());
    }
}
