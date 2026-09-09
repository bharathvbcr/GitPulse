use super::{
    announce, process_birth, query, terminal_command, terminal_launch, WorkbenchError,
    WorkbenchState,
};
use crate::terminal::{
    self, SessionObserver, TerminalExitPayload, TerminalSessions, TerminalSpawned,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, Mutex,
};
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::AppHandle;

static OWNER_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Launch {
    id: String,
    expected_revision: u64,
    rows: u16,
    cols: u16,
}

pub(super) fn parse(input: &str) -> Result<Launch, WorkbenchError> {
    if input.len() > 4096 || !input.trim_start().starts_with('{') {
        return Err(WorkbenchError::new(
            "invalid_input",
            "A terminal launch must be a JSON object of at most 4 KiB.",
        ));
    }
    let launch: Launch = serde_json::from_str(input)
        .map_err(|e| WorkbenchError::new("invalid_input", e.to_string()))?;
    if launch.id.is_empty()
        || launch.id.len() > 128
        || !launch
            .id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        || launch.expected_revision != 1
        || launch.rows == 0
        || launch.rows > 1000
        || launch.cols == 0
        || launch.cols > 1000
    {
        return Err(WorkbenchError::new(
            "invalid_input",
            "Launch requires a prepared attempt and bounded terminal dimensions.",
        ));
    }
    Ok(launch)
}

#[derive(Deserialize)]
struct Source {
    id: String,
    revision: u64,
    state: String,
    cwd: String,
    provider: String,
    permission_mode: String,
    bypass_acknowledged: bool,
    brief: Brief,
}
#[derive(Deserialize)]
struct Brief {
    markdown: String,
}

pub(super) fn spawn<R: tauri::Runtime>(
    app: &AppHandle<R>,
    terminals: &TerminalSessions,
    state: &WorkbenchState,
    launch: Launch,
) -> Result<TerminalSpawned, WorkbenchError> {
    let response =
        state.with_store(|store| query(store, "runs.get", &json!({"id":launch.id}).to_string()))?;
    let source: Source = serde_json::from_value(response["item"].clone())
        .map_err(|e| WorkbenchError::new("protocol_error", e.to_string()))?;
    if let Some(session) = terminal::tracked_session(terminals, &source.id) {
        return Ok(session);
    }
    if source.state != "prepared" || source.revision != launch.expected_revision {
        return Err(WorkbenchError::new("launch_consumed", "This attempt was already claimed or ended. Inspect its run state; starting again requires a new attempt."));
    }
    let program = terminal_command::program(&source.provider)?;
    terminal_command::check(
        &program,
        &source.cwd,
        &source.provider,
        &source.permission_mode,
    )?;
    start(app, terminals, state, launch, source, program)
}

fn start<R: tauri::Runtime>(
    app: &AppHandle<R>,
    terminals: &TerminalSessions,
    state: &WorkbenchState,
    launch: Launch,
    source: Source,
    program: String,
) -> Result<TerminalSpawned, WorkbenchError> {
    let brief = terminal_command::BriefFile::create(&source.brief.markdown)?;
    let args = terminal_command::arguments(
        &source.provider,
        &source.permission_mode,
        source.bypass_acknowledged,
        &source.cwd,
        &brief.path,
    )?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| WorkbenchError::new("clock_error", e.to_string()))?
        .as_nanos();
    let owner = format!(
        "native-{}-{stamp}-{}",
        std::process::id(),
        OWNER_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    );
    let observer = Arc::new(RunObserver {
        app: app.clone(),
        host: state.clone(),
        run_id: source.id,
        owner,
        start: Mutex::new(None),
        _brief: brief,
    });
    terminal::spawn_tracked_session(
        app,
        terminals,
        &source.cwd,
        launch.rows,
        launch.cols,
        program,
        args,
        observer,
    )
    .map_err(|e| WorkbenchError::new("launch_error", e))
}

struct RunObserver<R: tauri::Runtime> {
    app: AppHandle<R>,
    host: WorkbenchState,
    run_id: String,
    owner: String,
    start: Mutex<Option<(u32, String)>>,
    _brief: terminal_command::BriefFile,
}

impl<R: tauri::Runtime> RunObserver<R> {
    fn current(&self) -> Result<Value, WorkbenchError> {
        self.host.with_run_receipt(|store| {
            query(store, "runs.get", &json!({"id":self.run_id}).to_string())
        })
    }
    fn write(&self, method: &str, input: Value) -> Result<(), WorkbenchError> {
        let result = self
            .host
            .with_run_receipt(|store| query(store, method, &input.to_string()))?;
        announce(&self.app, &result);
        Ok(())
    }
    fn record_start(&self, session: &str) -> Result<(), WorkbenchError> {
        let identity = self
            .start
            .lock()
            .map_err(|_| WorkbenchError::new("process_error", "Process identity lock failed."))?
            .clone()
            .ok_or_else(|| {
                WorkbenchError::new(
                    "process_error",
                    "Process creation identity was unavailable.",
                )
            })?;
        let current = self.current()?;
        if current["item"]["state"] == "running"
            && current["item"]["session_id"] == session
            && current["item"]["process_id"] == identity.0
            && current["item"]["process_start"] == identity.1
        {
            return Ok(());
        }
        self.write("runs.started", json!({"id":self.run_id,"request_id":format!("{}-started",self.owner),"expected_revision":current["item"]["revision"],"owner_id":self.owner,"session_id":session,"process_id":identity.0,"process_start":identity.1}))
    }
    fn finish(
        &self,
        session: &str,
        spawn_failure: Option<&str>,
        payload: Option<&TerminalExitPayload>,
    ) -> Result<(), WorkbenchError> {
        let mut current = self.current()?;
        if payload.is_some() && current["item"]["state"] == "starting" {
            // Retry only storage bookkeeping using retained, actually observed
            // process identity. This path can never execute another child.
            match self.record_start(session) {
                Ok(()) => current = self.current()?,
                Err(error) => {
                    log::warn!(target: "workbench", "Process-start receipt remains unavailable at exit: {}", message(error))
                }
            }
        }
        let state = current["item"]["state"].as_str().unwrap_or("");
        if matches!(state, "exited" | "failed" | "cancelled") {
            return Ok(());
        }
        let outcome = if spawn_failure.is_some() {
            "failed"
        } else if payload.is_some_and(|p| p.reaped)
            && (state == "running"
                || (state == "unresolved" && current["item"]["process_id"].is_u64()))
        {
            "exited"
        } else {
            "unresolved"
        };
        let reason = match (spawn_failure, payload) {
            (Some(reason), _) => reason.to_owned(),
            (_, Some(exit)) if outcome == "exited" => format!("Terminal process exited. Signal: {}. {}", exit.signal, exit.error.as_deref().unwrap_or("")),
            (_, Some(exit)) if !exit.reaped => format!("Process exit could not be confirmed. {}", exit.error.as_deref().unwrap_or("Execution needs reconciliation.")),
            _ => "Terminal ended without a durable process-start receipt; execution outcome needs reconciliation.".into(),
        };
        let reason: String = reason.chars().take(512).collect();
        let code = if outcome == "exited" {
            payload
                .and_then(|p| p.exit_code)
                .and_then(|c| u32::try_from(c).ok())
        } else {
            None
        };
        self.write("runs.finish", json!({"id":self.run_id,"request_id":format!("{}-finished",self.owner),"expected_revision":current["item"]["revision"],"owner_id":self.owner,"session_id":session,"outcome":outcome,"reason":reason,"exit_code":code}))
    }
}

impl<R: tauri::Runtime> SessionObserver for RunObserver<R> {
    fn run_id(&self) -> &str {
        &self.run_id
    }
    fn before_spawn(&self, session: &str) -> Result<(), String> {
        let result = terminal_launch::claim(&self.host, &json!({"id":self.run_id,"request_id":format!("{}-claim",self.owner),"expected_revision":1,"owner_id":self.owner,"session_id":session}).to_string()).map_err(message)?;
        announce(&self.app, &result);
        Ok(())
    }
    fn started(&self, session: &str, pid: Option<u32>) -> Result<(), String> {
        let pid = pid
            .filter(|pid| *pid > 0)
            .ok_or("PTY did not report a process ID")?;
        let birth = process_birth::read(pid)?;
        *self
            .start
            .lock()
            .map_err(|_| "Process identity lock failed")? = Some((pid, birth));
        self.record_start(session).map_err(message)
    }
    fn spawn_failed(&self, session: &str, reason: &str) {
        if let Err(error) = self.finish(session, Some(reason), None) {
            log::error!(target: "workbench", "Could not record a failed task launch: {}", message(error));
        }
    }
    fn finished(&self, payload: &TerminalExitPayload) {
        if let Err(error) = self.finish(&payload.id, None, Some(payload)) {
            log::error!(target: "workbench", "Task terminal exited but its receipt is unresolved: {}", message(error));
        }
    }
}

fn message(error: WorkbenchError) -> String {
    format!("{}: {}", error.code, error.message)
}

#[cfg(all(test, unix))]
mod tests {
    use super::{parse, spawn, start, Launch, Source};
    use crate::engine::git_cli::git_global;
    use crate::terminal::{
        acknowledge_output, shutdown_sessions, write_to_session, TerminalSessions,
    };
    use crate::workbench::{Inner, WorkbenchState};
    use base64::{engine::general_purpose::STANDARD, Engine};
    use serde_json::{json, Value};
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::sync::{mpsc, Arc, Mutex};
    use std::time::{Duration, Instant};
    use tauri::Listener;

    struct Cleanup(TerminalSessions);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            if let Err(error) = shutdown_sessions(&self.0) {
                eprintln!("task terminal cleanup: {error}");
            }
        }
    }

    fn fixture(root: &Path) -> WorkbenchState {
        git_global(&["init", root.to_str().unwrap()]).unwrap();
        let state = WorkbenchState(Arc::new(Inner {
            path: Some(root.join("profile.sqlite")),
            ..Inner::default()
        }));
        state
            .register(root.to_str().unwrap(), "repo", "register")
            .unwrap();
        state.request("items.put", &json!({"id":"task","request_id":"task","expected_revision":0,"title":"Keep E42","description":"Do not execute $(untrusted) 🧪".repeat(2000),"repository_ids":["repo"],"primary_repository_id":"repo"}).to_string()).unwrap();
        state.request("runs.prepare_terminal", &json!({"id":"run","request_id":"prepare","task_id":"task","source_revision":1,"repository_id":"repo","repository_revision":1,"repo_path":root,"provider":"claude","permission_mode":"ask"}).to_string()).unwrap();
        state
    }
    fn source(state: &WorkbenchState) -> Source {
        serde_json::from_value(
            state.request("runs.get", r#"{"id":"run"}"#).unwrap()["item"].clone(),
        )
        .unwrap()
    }
    fn launch() -> Launch {
        parse(r#"{"id":"run","expected_revision":1,"rows":24,"cols":80}"#).unwrap()
    }
    fn script(root: &Path, body: &str) -> String {
        let path = root.join("scripted provider");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        path.to_str().unwrap().to_owned()
    }

    #[test]
    fn real_pty_records_process_then_exit_and_replay_attaches_without_spawning() {
        let root = tempfile::tempdir().unwrap();
        let state = fixture(root.path());
        let app = tauri::test::mock_builder().build(crate::context()).unwrap();
        let terminals = TerminalSessions::default();
        let _cleanup = Cleanup(terminals.clone());
        let output = Arc::new(Mutex::new(String::new()));
        let captured = output.clone();
        let ack = terminals.clone();
        app.listen("terminal-output", move |event| {
            let value: Value = serde_json::from_str(event.payload()).unwrap();
            let bytes = STANDARD
                .decode(value["data_b64"].as_str().unwrap())
                .unwrap();
            captured
                .lock()
                .unwrap()
                .push_str(&String::from_utf8_lossy(&bytes));
            acknowledge_output(&ack, value["id"].as_str().unwrap(), bytes.len()).unwrap();
        });
        let (sent, received) = mpsc::channel();
        app.listen("terminal-exit", move |event| {
            sent.send(event.payload().to_owned()).unwrap();
        });
        let program = script(
            root.path(),
            "printf '%s\\n' \"$DEVCOUNCIL_ROOT\" \"$@\"\nIFS= read -r finish\nexit 37",
        );
        let started = start(
            app.handle(),
            &terminals,
            &state,
            launch(),
            source(&state),
            program,
        )
        .unwrap();
        let running = state.request("runs.get", r#"{"id":"run"}"#).unwrap();
        assert_eq!(running["item"]["state"], "running");
        assert!(running["item"]["process_id"].as_u64().unwrap() > 0);
        assert!(running["item"]["process_start"]
            .as_str()
            .unwrap()
            .contains(':'));
        let replay = spawn(app.handle(), &terminals, &state, launch()).unwrap();
        assert_eq!(replay.id, started.id);
        assert_eq!(
            state.request("runs.get", r#"{"id":"run"}"#).unwrap(),
            running
        );
        write_to_session(&terminals, &started.id, "finish\n").unwrap();
        let exited: Value =
            serde_json::from_str(&received.recv_timeout(Duration::from_secs(5)).unwrap()).unwrap();
        assert_eq!(exited["exit_code"], 37);
        let saved = state.request("runs.get", r#"{"id":"run"}"#).unwrap();
        assert_eq!(saved["item"]["state"], "exited");
        assert_eq!(saved["item"]["exit_code"], 37);
        assert_eq!(
            state.request("items.get", r#"{"id":"task"}"#).unwrap()["item"]["status"],
            "inbox"
        );
        assert_eq!(
            spawn(app.handle(), &terminals, &state, launch())
                .unwrap_err()
                .code,
            "launch_consumed"
        );
        let output = output.lock().unwrap().clone();
        assert!(!output.contains("untrusted"));
        assert!(output.contains(root.path().canonicalize().unwrap().to_str().unwrap()));
        let start = output.find("Read the UTF-8 task brief at ").unwrap()
            + "Read the UTF-8 task brief at ".len();
        let path = serde_json::Deserializer::from_str(&output[start..])
            .into_iter::<String>()
            .next()
            .unwrap()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        while Path::new(&path).exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            !Path::new(&path).exists(),
            "temporary brief must be released after exit"
        );
    }

    #[test]
    fn shutdown_preserves_the_exit_receipt_and_refuses_new_work() {
        let root = tempfile::tempdir().unwrap();
        let state = fixture(root.path());
        let app = tauri::test::mock_builder().build(crate::context()).unwrap();
        let terminals = TerminalSessions::default();
        let _cleanup = Cleanup(terminals.clone());
        let (sent, received) = mpsc::channel();
        app.listen("terminal-exit", move |event| {
            sent.send(event.payload().to_owned()).unwrap();
        });
        let program = script(root.path(), "IFS= read -r finish");
        start(
            app.handle(),
            &terminals,
            &state,
            launch(),
            source(&state),
            program,
        )
        .unwrap();
        state.shutdown();
        assert_eq!(
            state
                .request("runs.get", r#"{"id":"run"}"#)
                .unwrap_err()
                .code,
            "closed"
        );
        shutdown_sessions(&terminals).unwrap();
        let exit: Value =
            serde_json::from_str(&received.recv_timeout(Duration::from_secs(5)).unwrap()).unwrap();
        assert_eq!(exit["reaped"], true);
        let reopened = WorkbenchState(Arc::new(Inner {
            path: Some(root.path().join("profile.sqlite")),
            ..Inner::default()
        }));
        let saved = reopened.request("runs.get", r#"{"id":"run"}"#).unwrap();
        assert_eq!(saved["item"]["state"], "exited");
        assert_eq!(saved["item"]["outcome_uncertain"], false);
        assert_eq!(
            reopened.request("items.get", r#"{"id":"task"}"#).unwrap()["item"]["status"],
            "inbox"
        );
    }

    #[test]
    fn failed_spawn_is_recorded_without_claim_replay_or_task_completion() {
        let root = tempfile::tempdir().unwrap();
        let state = fixture(root.path());
        let app = tauri::test::mock_builder().build(crate::context()).unwrap();
        let terminals = TerminalSessions::default();
        let _cleanup = Cleanup(terminals.clone());
        assert!(start(
            app.handle(),
            &terminals,
            &state,
            launch(),
            source(&state),
            root.path()
                .join("removed-provider")
                .to_string_lossy()
                .into_owned()
        )
        .is_err());
        let saved = state.request("runs.get", r#"{"id":"run"}"#).unwrap();
        assert_eq!(saved["item"]["state"], "failed");
        assert!(saved["item"]["process_id"].is_null());
        assert_eq!(
            spawn(app.handle(), &terminals, &state, launch())
                .unwrap_err()
                .code,
            "launch_consumed"
        );
    }

    #[test]
    fn start_receipt_failure_stops_the_child_and_preserves_uncertainty() {
        let root = tempfile::tempdir().unwrap();
        let state = fixture(root.path());
        state.with_store(|store| { store.connection().execute_batch("CREATE TRIGGER fail_start BEFORE UPDATE ON work_runs WHEN json_extract(NEW.body,'$.state')='running' BEGIN SELECT RAISE(ABORT,'injected start failure'); END;").unwrap(); Ok(()) }).unwrap();
        let app = tauri::test::mock_builder().build(crate::context()).unwrap();
        let terminals = TerminalSessions::default();
        let _cleanup = Cleanup(terminals.clone());
        let (sent, received) = mpsc::channel();
        app.listen("terminal-exit", move |event| {
            sent.send(event.payload().to_owned()).unwrap();
        });
        let program = script(root.path(), "IFS= read -r finish");
        assert!(start(
            app.handle(),
            &terminals,
            &state,
            launch(),
            source(&state),
            program
        )
        .is_err());
        received.recv_timeout(Duration::from_secs(5)).unwrap();
        let saved = state.request("runs.get", r#"{"id":"run"}"#).unwrap();
        assert_eq!(saved["item"]["state"], "unresolved");
        assert_eq!(saved["item"]["outcome_uncertain"], true);
    }
}
