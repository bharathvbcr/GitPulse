use super::{
    announce, process_birth, query, receipts, terminal_command, terminal_launch, WorkbenchError,
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
/// How long a second launch of one attempt waits for the first to register
/// its session. Covers a fork and two store writes with room to spare.
const LAUNCH_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

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
        || launch.expected_revision != terminal_launch::PREPARED_REVISION
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
    // One launch of this attempt at a time, and a second one waits for the
    // first rather than failing. The claim below is single-use and the
    // session is registered only after the fork, so a second caller in that
    // window read "already claimed" with nothing to attach to — a reloaded
    // page or a second window asking for the same terminal got an error
    // instead of the terminal.
    let _launch = state
        .0
        .terminal_launches
        .enter_within(&launch.id, LAUNCH_WAIT)?;
    let response =
        state.with_store(|store| query(store, "runs.get", &json!({"id":launch.id}).to_string()))?;
    let source: Source = serde_json::from_value(response["item"].clone())
        .map_err(|e| WorkbenchError::new("protocol_error", e.to_string()))?;
    if let Some(session) =
        terminal::attach_tracked_session(terminals, &source.id, launch.rows, launch.cols)
    {
        return Ok(session);
    }
    if source.state != "prepared" || source.revision != launch.expected_revision {
        return Err(WorkbenchError::new("launch_consumed", "This attempt was already claimed or ended. Inspect its run state; starting again requires a new attempt."));
    }
    let program = terminal_command::program(&source.provider)?;
    // Read once, so the flags the build is checked for are the flags it gets.
    let defaults = crate::tool_config::agent_defaults();
    let sources = defaults.claude_setting_sources_arg();
    // The model the attempt recorded when it was prepared — the saved default
    // with that launch's override — not the default as it is now. An attempt
    // stored before runs recorded one has no `model_choice` key and takes the
    // default, as it always did.
    let recorded = match response["item"].get("model_choice") {
        Some(record) => terminal_command::model_choice_from_record(record)
            .map_err(|message| WorkbenchError::new("protocol_error", message))?,
        None => defaults.model_for(&source.provider).cloned(),
    };
    let options = terminal_command::LaunchOptions {
        notify: crate::tool_config::session_alerts().configure_agents,
        setting_sources: sources.as_deref(),
        model: recorded.as_ref(),
    };
    terminal_command::check(
        &program,
        &source.cwd,
        &source.provider,
        &source.permission_mode,
        &options,
    )?;
    start(app, terminals, state, launch, source, program, options)
}

fn start<R: tauri::Runtime>(
    app: &AppHandle<R>,
    terminals: &TerminalSessions,
    state: &WorkbenchState,
    launch: Launch,
    source: Source,
    program: String,
    options: terminal_command::LaunchOptions<'_>,
) -> Result<TerminalSpawned, WorkbenchError> {
    let brief = terminal_command::BriefFile::create(&source.brief.markdown)?;
    let args = terminal_command::arguments(
        &source.provider,
        &source.permission_mode,
        source.bypass_acknowledged,
        &source.cwd,
        &brief.path,
        &terminal_command::Extras {
            run_id: Some(&source.id),
            brief_dir: Some(&brief.dir),
            launch: options,
        },
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
        observed: Mutex::new(receipts::Observed {
            run_id: source.id.clone(),
            owner: owner.clone(),
            session: String::new(),
            start: None,
            exit: None,
            spawn_failure: None,
        }),
        run_id: source.id,
        owner,
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
    /// Everything seen so far. The decision about what to store is
    /// `receipts::apply`, shared with the replay of a receipt that storage
    /// refused.
    observed: Mutex<receipts::Observed>,
    _brief: terminal_command::BriefFile,
}

impl<R: tauri::Runtime> RunObserver<R> {
    /// Adds what was just seen, then brings the store up to date. A refusal
    /// is journaled before it is returned, so the observation outlives this
    /// process; the store taking it retires any earlier journal entry.
    fn observe(&self, update: impl FnOnce(&mut receipts::Observed)) -> Result<(), WorkbenchError> {
        let observed = {
            let mut observed = self.observed.lock().map_err(|_| {
                WorkbenchError::new("process_error", "Process identity lock failed.")
            })?;
            update(&mut observed);
            observed.clone()
        };
        let store = |method: &str, input: &Value| {
            self.host
                .with_run_receipt(|store| query(store, method, &input.to_string()))
        };
        match receipts::apply(&store, &observed) {
            Ok(written) => {
                for result in &written {
                    announce(&self.app, result);
                }
                receipts::retire(&self.host, &self.run_id);
                Ok(())
            }
            Err(error) => {
                let why = format!("{}: {}", error.code, error.message);
                match receipts::journal(&self.host, &observed) {
                    Ok(()) => {
                        log::warn!(target: "workbench", "run {} receipt was not stored ({why}); kept on disk for the next reconciliation", self.run_id)
                    }
                    Err(journal) => {
                        log::error!(target: "workbench", "run {} receipt was not stored ({why}) and could not be kept on disk: {journal}", self.run_id)
                    }
                }
                Err(error)
            }
        }
    }
}

impl<R: tauri::Runtime> SessionObserver for RunObserver<R> {
    fn run_id(&self) -> &str {
        &self.run_id
    }
    fn before_spawn(&self, session: &str) -> Result<(), String> {
        let result = terminal_launch::claim(&self.host, &json!({"id":self.run_id,"request_id":format!("{}-claim",self.owner),"expected_revision":terminal_launch::PREPARED_REVISION,"owner_id":self.owner,"session_id":session}).to_string()).map_err(message)?;
        announce(&self.app, &result);
        Ok(())
    }
    fn started(&self, session: &str, pid: Option<u32>) -> Result<(), String> {
        let pid = pid
            .filter(|pid| *pid > 0)
            .ok_or("PTY did not report a process ID")?;
        let birth = process_birth::read(pid)?;
        self.observe(|observed| {
            observed.session = session.to_owned();
            observed.start = Some((pid, birth));
        })
        .map_err(message)
    }
    fn spawn_failed(&self, session: &str, reason: &str) {
        if let Err(error) = self.observe(|observed| {
            observed.session = session.to_owned();
            observed.spawn_failure = Some(reason.to_owned());
        }) {
            log::error!(target: "workbench", "Could not record a failed task launch: {}", message(error));
        }
    }
    fn finished(&self, payload: &TerminalExitPayload) {
        if let Err(error) = self.observe(|observed| {
            observed.session = payload.id.clone();
            observed.exit = Some(receipts::ObservedExit::from(payload));
        }) {
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
        crate::test_support::trust_repo(root);
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

    /// An attempt chosen for a folder inside a checkout runs its agent in that
    /// folder, with the checkout's root as the repository it trusts and
    /// records. It could not run at all: preparation refused anything but
    /// the root, and the spawn validated the folder as if it were a
    /// repository of its own.
    #[test]
    fn an_attempt_in_a_subdirectory_of_its_checkout_starts_there() {
        let root = tempfile::tempdir().unwrap();
        git_global(&["init", root.path().to_str().unwrap()]).unwrap();
        crate::test_support::trust_repo(root.path());
        let folder = root.path().join("packages").join("web app");
        std::fs::create_dir_all(&folder).unwrap();
        let state = WorkbenchState(Arc::new(Inner {
            path: Some(root.path().join("profile.sqlite")),
            ..Inner::default()
        }));
        state
            .register(root.path().to_str().unwrap(), "repo", "register")
            .unwrap();
        state.request("items.put", &json!({"id":"task","request_id":"task","expected_revision":0,"title":"Keep E42","repository_ids":["repo"],"primary_repository_id":"repo"}).to_string()).unwrap();
        let prepared = state
            .request("runs.prepare_terminal", &json!({"id":"run","request_id":"prepare","task_id":"task","source_revision":1,"repository_id":"repo","repository_revision":1,"repo_path":folder,"provider":"claude","permission_mode":"ask"}).to_string())
            .unwrap_or_else(|e| panic!("a subdirectory of the checkout was refused: {} {}", e.code, e.message));
        let canonical = folder.canonicalize().unwrap();
        assert_eq!(prepared["item"]["cwd"], json!(canonical));
        assert_eq!(
            prepared["item"]["git_dir"],
            json!(root.path().join(".git").canonicalize().unwrap())
        );
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
        let program = script(
            root.path(),
            "printf 'cwd=%s\\n' \"$(pwd -P)\"\nIFS= read -r finish\nexit 0",
        );
        let started = start(
            app.handle(),
            &terminals,
            &state,
            launch(),
            source(&state),
            program,
            super::terminal_command::LaunchOptions::default(),
        )
        .unwrap_or_else(|e| {
            panic!(
                "the agent could not start in the subdirectory: {} {}",
                e.code, e.message
            )
        });
        let expected = format!("cwd={}", canonical.display());
        let waited = Instant::now();
        while !output.lock().unwrap().contains(&expected) {
            assert!(
                waited.elapsed() < Duration::from_secs(30),
                "the agent did not start in {}: {}",
                canonical.display(),
                output.lock().unwrap()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(
            state.request("runs.get", r#"{"id":"run"}"#).unwrap()["item"]["state"],
            "running"
        );
        write_to_session(&terminals, &started.id, "finish\n").unwrap();
    }

    /// A second launch of one attempt that arrives while the first holds it —
    /// claimed, forking, not yet registered — waits and then gets the first
    /// one's session. It read "already claimed" with nothing to attach to,
    /// so a reloaded page or a second window got an error instead of the
    /// terminal. One process, one claim, the same session for both.
    #[test]
    fn a_second_launch_in_flight_waits_and_attaches_to_the_first() {
        let root = tempfile::tempdir().unwrap();
        let state = fixture(root.path());
        let app = tauri::test::mock_builder().build(crate::context()).unwrap();
        let terminals = TerminalSessions::default();
        let _cleanup = Cleanup(terminals.clone());
        let ack = terminals.clone();
        app.listen("terminal-output", move |event| {
            let value: Value = serde_json::from_str(event.payload()).unwrap();
            let _ = acknowledge_output(
                &ack,
                value["id"].as_str().unwrap(),
                value["bytes"].as_u64().unwrap() as usize,
            );
        });
        let program = script(root.path(), "IFS= read -r finish\nexit 0");
        let first = state.0.terminal_launches.try_enter("run").unwrap();
        let (first_session, second) = std::thread::scope(|scope| {
            let handle = app.handle();
            let waiter = scope.spawn(|| spawn(handle, &terminals, &state, launch()));
            let parked = Instant::now();
            while state.0.terminal_launches.waiting() == 0 {
                assert!(
                    parked.elapsed() < Duration::from_secs(10),
                    "the second launch never waited"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
            let started = start(
                app.handle(),
                &terminals,
                &state,
                launch(),
                source(&state),
                program,
                super::terminal_command::LaunchOptions::default(),
            )
            .unwrap();
            drop(first);
            (started, waiter.join().unwrap())
        });
        let second = second.expect("the second launch got an error instead of the terminal");
        assert_eq!(second.id, first_session.id);
        assert_eq!(
            state.request("runs.get", r#"{"id":"run"}"#).unwrap()["item"]["state"],
            "running"
        );
        assert!(state.0.terminal_launches.is_empty());
        write_to_session(&terminals, &first_session.id, "finish\n").unwrap();
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
            let reserved = value["bytes"]
                .as_u64()
                .expect("terminal output must name its reserved length")
                as usize;
            assert_eq!(
                reserved,
                bytes.len(),
                "reserved credit must equal the emitted chunk"
            );
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
            super::terminal_command::LaunchOptions::default(),
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
        // What the agent is handed: the store's export, which opens with the
        // agent guidance every handoff carries, then the saved task. Read while
        // the agent is alive, because the file is released when it exits.
        let prompt = "Read the UTF-8 task brief at ";
        let waited = Instant::now();
        let handed = loop {
            let seen = output.lock().unwrap().clone();
            let path = seen.find(prompt).and_then(|at| {
                serde_json::Deserializer::from_str(&seen[at + prompt.len()..])
                    .into_iter::<String>()
                    .next()
                    .and_then(Result::ok)
            });
            if let Some(path) = path {
                break std::fs::read_to_string(path).unwrap();
            }
            assert!(
                waited.elapsed() < Duration::from_secs(30),
                "the launch prompt never named the brief"
            );
            std::thread::sleep(Duration::from_millis(20));
        };
        let opening = format!(
            "# Task brief v1\n\n{}\n{}\n\n## Title\nKeep E42\n",
            dc_store::workbench::AGENT_GUIDANCE_HEADING,
            dc_store::workbench::AGENT_GUIDANCE.trim()
        );
        assert!(
            handed.starts_with(&opening),
            "the brief must open with the guidance, then the task: {handed:.200}"
        );
        assert!(handed.contains("Task: task (revision 1)"));
        assert!(handed.contains("Do not execute $(untrusted) 🧪"));
        write_to_session(&terminals, &started.id, "finish\n").unwrap();
        // The script exits as soon as it has read that line, so the only open
        // question is when the host schedules it — which is not what this test is
        // about. Watch the process itself go away first, then require the exit
        // event, which is the property: an exit that happened is reported and
        // recorded. Waiting 5s on the event alone made a busy host the likeliest
        // way to fail this, and it failed exactly that way in a full-suite run.
        // The outer bound is a backstop only: it turns a child that never exits
        // into a failure instead of a suite that hangs.
        let pid = running["item"]["process_id"].as_u64().unwrap() as libc::pid_t;
        let waited = Instant::now();
        // SAFETY: signal 0 checks for the process's existence and delivers
        // nothing. The pid came from this test's own child moments ago.
        while unsafe { libc::kill(pid, 0) } == 0 {
            assert!(
                waited.elapsed() < Duration::from_secs(120),
                "the pty child never exited after being told to finish"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        let exited: Value = serde_json::from_str(
            &received
                .recv_timeout(Duration::from_secs(5))
                .expect("an exit that happened must be reported"),
        )
        .unwrap();
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
            super::terminal_command::LaunchOptions::default(),
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
                .into_owned(),
            super::terminal_command::LaunchOptions::default(),
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
            program,
            super::terminal_command::LaunchOptions::default(),
        )
        .is_err());
        received.recv_timeout(Duration::from_secs(5)).unwrap();
        let saved = state.request("runs.get", r#"{"id":"run"}"#).unwrap();
        assert_eq!(saved["item"]["state"], "unresolved");
        assert_eq!(saved["item"]["outcome_uncertain"], true);
    }

    /// The exit receipt is refused by storage. Before, the observer logged it
    /// and the exit code the PTY reported was gone for good: the run stayed
    /// `running` until a reconciler released it as `outcome_uncertain` with
    /// no code. Now the observation is kept on disk and stored on the next
    /// start, exactly once.
    #[test]
    fn an_exit_receipt_storage_refused_is_stored_after_a_restart_exactly_once() {
        let root = tempfile::tempdir().unwrap();
        let state = fixture(root.path());
        state.with_store(|store| { store.connection().execute_batch("CREATE TRIGGER fail_finish BEFORE UPDATE ON work_runs WHEN json_extract(NEW.body,'$.state')='exited' BEGIN SELECT RAISE(ABORT,'injected finish failure'); END;").unwrap(); Ok(()) }).unwrap();
        let app = tauri::test::mock_builder().build(crate::context()).unwrap();
        let terminals = TerminalSessions::default();
        let _cleanup = Cleanup(terminals.clone());
        let (sent, received) = mpsc::channel();
        app.listen("terminal-exit", move |event| {
            sent.send(event.payload().to_owned()).unwrap();
        });
        let program = script(root.path(), "IFS= read -r finish\nexit 7");
        let session = start(
            app.handle(),
            &terminals,
            &state,
            launch(),
            source(&state),
            program,
            super::terminal_command::LaunchOptions::default(),
        )
        .unwrap();
        write_to_session(&terminals, &session.id, "finish\n").unwrap();
        let exit: Value =
            serde_json::from_str(&received.recv_timeout(Duration::from_secs(5)).unwrap()).unwrap();
        assert_eq!(exit["exit_code"], 7);

        let saved = state.request("runs.get", r#"{"id":"run"}"#).unwrap();
        assert_eq!(
            saved["item"]["state"], "running",
            "the injected failure did not fire"
        );
        let journal = root.path().join("run-receipts").join("run.json");
        assert!(journal.exists(), "the refused receipt was not kept on disk");
        let kept: super::receipts::Observed =
            serde_json::from_slice(&std::fs::read(&journal).unwrap()).unwrap();
        assert_eq!(kept.exit.as_ref().and_then(|e| e.exit_code), Some(7));

        // The storage fault clears; GitPulse starts again.
        state
            .with_store(|store| {
                store
                    .connection()
                    .execute_batch("DROP TRIGGER fail_finish;")
                    .unwrap();
                Ok(())
            })
            .unwrap();
        state.shutdown();
        let restarted = WorkbenchState(Arc::new(Inner {
            path: Some(root.path().join("profile.sqlite")),
            ..Inner::default()
        }));
        restarted.reconcile_stale_runs().unwrap();
        let stored = restarted.request("runs.get", r#"{"id":"run"}"#).unwrap();
        assert_eq!(stored["item"]["state"], "exited");
        assert_eq!(
            stored["item"]["exit_code"], 7,
            "the observed exit code was lost"
        );
        assert_eq!(stored["item"]["outcome_uncertain"], false);
        assert!(!journal.exists(), "a stored receipt must be retired");

        // Idempotent: another pass changes nothing.
        let revision = stored["item"]["revision"].clone();
        restarted.reconcile_stale_runs().unwrap();
        assert_eq!(
            restarted.request("runs.get", r#"{"id":"run"}"#).unwrap()["item"]["revision"],
            revision
        );

        // A receipt for a run the store has already ended is retired, and the
        // store's record stands.
        let mut stale = kept.clone();
        stale.exit.as_mut().unwrap().exit_code = Some(9);
        super::receipts::journal(&restarted, &stale).unwrap();
        restarted.reconcile_stale_runs().unwrap();
        let after = restarted.request("runs.get", r#"{"id":"run"}"#).unwrap();
        assert_eq!(after["item"]["exit_code"], 7);
        assert_eq!(after["item"]["revision"], revision);
        assert!(!journal.exists());
    }
}
