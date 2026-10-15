//! The terminal host (`gitpulse-ptyd`) end to end: a real host process, real
//! PTYs, real shells, and — for the cases the host exists for — a real "app"
//! process that is SIGKILLed with its sessions open.
//!
//! The killed app is this test binary run again as a child, selected by
//! `HOST_TEST_CHILD`: it starts sessions in the host and then waits to be
//! killed. The parent is the next app: it connects to the same host directory
//! and must find what the dead one left.
#![cfg(unix)]
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use gitpulse_lib::procguard::LockedSpawn;
use gitpulse_lib::terminal::host::client::HostConfig;
use gitpulse_lib::terminal::{
    attach_session, kill_session, list_sessions, spawn_session, write_to_session, OutputSink,
    TerminalSessions,
};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::Listener;

mod common;

const CHILD_ENV: &str = "HOST_TEST_CHILD";
const DIR_ENV: &str = "HOST_TEST_DIR";
const REPO_ENV: &str = "HOST_TEST_REPO";

fn host_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_gitpulse-ptyd"))
}

/// A host directory short enough for `sun_path`, canonical so no component
/// is a link.
fn host_dir() -> tempfile::TempDir {
    let base = std::env::temp_dir().canonicalize().unwrap();
    tempfile::Builder::new()
        .prefix("ph")
        .tempdir_in(base)
        .unwrap()
}

fn terminals(dir: &Path) -> TerminalSessions {
    TerminalSessions::with_host(HostConfig {
        dir: dir.to_path_buf(),
        binary: Some(host_binary()),
        idle_exit: Some(Duration::from_secs(3)),
    })
}

fn repo() -> tempfile::TempDir {
    gitpulse_lib::engine::git_cli::run_process_with_unlimited_spawn_rate();
    let dir = tempfile::tempdir().unwrap();
    assert!(std::process::Command::new("git")
        .args(["init", "-q"])
        .arg(dir.path())
        .status_locked()
        .unwrap()
        .success());
    // Saved, in the hermetic home: the host is another process, and a
    // session grant never reaches another process (`repository_trust::grant`).
    let path = dir.path().to_str().unwrap();
    let preview = gitpulse_lib::repository_trust::inspect(path).unwrap();
    gitpulse_lib::repository_trust::grant(path, &preview.identity, true).unwrap();
    dir
}

/// The host runs the trust check itself, in the process that forks: a
/// repository this app trusted only for its own session is refused there.
#[test]
fn trust_is_checked_in_the_host_and_a_session_grant_does_not_reach_it() {
    let dir = host_dir();
    let repo = tempfile::tempdir().unwrap();
    common::run_git(repo.path(), &["init", "-q"]);
    let path = repo.path().to_str().unwrap();
    let preview = gitpulse_lib::repository_trust::inspect(path).unwrap();
    gitpulse_lib::repository_trust::grant(path, &preview.identity, false).unwrap();
    let app = App::new(dir.path());
    let _stop = Stop(app.state.clone());
    let refused = spawn_session(
        app.app.handle(),
        &app.state,
        path,
        24,
        80,
        Some("/bin/sh".into()),
        Some(vec!["-c".into(), "exit 0".into()]),
        None,
        app.sink(),
    )
    .unwrap_err();
    assert!(refused.contains("REPOSITORY_TRUST_REQUIRED"), "{refused}");
    assert!(list_sessions(&app.state).unwrap().is_empty());
}

/// Each session's output messages, in arrival order.
type Output = Arc<Mutex<Vec<(String, Vec<u8>)>>>;

/// What one app sees: output per session (acknowledged as it arrives, as a
/// view does), losses and exits.
struct App {
    app: tauri::App<tauri::test::MockRuntime>,
    state: TerminalSessions,
    output: Output,
    losses: Arc<Mutex<Vec<serde_json::Value>>>,
    exits: mpsc::Receiver<serde_json::Value>,
}

impl App {
    fn new(dir: &Path) -> Self {
        let app = tauri::test::mock_builder()
            .build(gitpulse_lib::context())
            .unwrap();
        let state = terminals(dir);
        state.bind_app(app.handle());
        let output: Output = Arc::default();
        let (seen, acks) = (output.clone(), state.clone());
        app.listen_any("terminal-output", move |event| {
            let data: serde_json::Value = serde_json::from_str(event.payload()).unwrap();
            let id = data["id"].as_str().unwrap().to_string();
            let bytes = STANDARD.decode(data["data_b64"].as_str().unwrap()).unwrap();
            let _ = gitpulse_lib::terminal::acknowledge_output(&acks, &id, bytes.len());
            seen.lock().unwrap().push((id, bytes));
        });
        let losses: Arc<Mutex<Vec<serde_json::Value>>> = Arc::default();
        let lost = losses.clone();
        app.listen_any("terminal-loss", move |event| {
            lost.lock()
                .unwrap()
                .push(serde_json::from_str(event.payload()).unwrap());
        });
        let (send, exits) = mpsc::channel();
        app.listen_any("terminal-exit", move |event| {
            let _ = send.send(serde_json::from_str(event.payload()).unwrap());
        });
        App {
            app,
            state,
            output,
            losses,
            exits,
        }
    }

    fn sink(&self) -> OutputSink {
        OutputSink::events(self.app.handle())
    }

    fn shell(&self, repo: &Path, script: &str) -> String {
        spawn_session(
            self.app.handle(),
            &self.state,
            repo.to_str().unwrap(),
            24,
            80,
            Some("/bin/sh".into()),
            Some(vec!["-c".into(), script.into()]),
            None,
            self.sink(),
        )
        .unwrap()
        .id
    }

    fn text(&self, session: &str) -> String {
        let output = self.output.lock().unwrap();
        let bytes: Vec<u8> = output
            .iter()
            .filter(|(id, _)| id == session)
            .flat_map(|(_, b)| b.clone())
            .collect();
        String::from_utf8_lossy(&bytes).into_owned()
    }

    fn exit_of(&self, session: &str, within: Duration) -> serde_json::Value {
        let deadline = Instant::now() + within;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let exit = self
                .exits
                .recv_timeout(left)
                .unwrap_or_else(|_| panic!("no exit for {session} within {within:?}"));
            if exit["id"] == session {
                return exit;
            }
        }
    }
}

fn wait_for(within: Duration, done: impl Fn() -> bool) -> bool {
    let deadline = Instant::now() + within;
    while !done() {
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    true
}

struct Stop(TerminalSessions);
impl Drop for Stop {
    fn drop(&mut self) {
        if let Err(error) = gitpulse_lib::terminal::shutdown_sessions(&self.0) {
            eprintln!("host test cleanup incomplete: {error}");
        }
    }
}

#[test]
fn a_shell_in_the_host_is_typed_into_and_reports_its_real_exit() {
    let dir = host_dir();
    let repo = repo();
    let app = App::new(dir.path());
    let _stop = Stop(app.state.clone());
    assert_eq!(app.state.backend(), "host");
    let id = app.shell(
        repo.path(),
        "printf 'ready\\n'; read line; printf 'got-%s\\n' \"$line\"; exit 7",
    );
    assert!(
        wait_for(Duration::from_secs(10), || app.text(&id).contains("ready")),
        "{:?}",
        app.text(&id)
    );
    write_to_session(&app.state, &id, "abc\n").unwrap();
    let exit = app.exit_of(&id, Duration::from_secs(10));
    assert!(app.text(&id).contains("got-abc"), "{:?}", app.text(&id));
    assert_eq!(exit["exit_code"], 7);
    assert_eq!(exit["reaped"], true);
    assert!(list_sessions(&app.state).unwrap().is_empty());
}

#[test]
fn the_host_socket_and_its_directory_are_the_users_alone() {
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};
    let dir = host_dir();
    let repo = repo();
    let app = App::new(dir.path());
    let _stop = Stop(app.state.clone());
    let id = app.shell(repo.path(), "read x");
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    let socket = dir.path().join(gitpulse_lib::terminal::host::socket_name(
        gitpulse_lib::terminal::host::PROTOCOL,
    ));
    assert!(std::fs::metadata(&socket).unwrap().file_type().is_socket());
    assert_eq!(mode(&socket), 0o600);
    assert_eq!(mode(dir.path()), 0o700);
    assert_eq!(mode(&dir.path().join("receipts")), 0o700);
    assert_eq!(
        mode(&dir.path().join(format!("receipts/{id}.jsonl"))),
        0o600
    );
    kill_session(&app.state, &id).unwrap();
}

/// The child half of the crash tests: an app that starts sessions in the host,
/// says which, and waits to be killed.
#[test]
#[ignore = "run only as the killed app of the crash tests"]
fn crash_child_app() {
    if std::env::var(CHILD_ENV).as_deref() != Ok("1") {
        return;
    }
    let dir = PathBuf::from(std::env::var(DIR_ENV).unwrap());
    let repo = PathBuf::from(std::env::var(REPO_ENV).unwrap());
    let app = App::new(&dir);
    // A shell that never repaints, printing a marker before and after the
    // app dies, then waiting for input; and one that exits on its own, with
    // a code, while no app is there.
    let talker = app.shell(
        &repo,
        "printf 'before-crash\\n'; sleep 1.5; printf 'after-crash\\n'; read line; printf 'typed-%s\\n' \"$line\"; read again",
    );
    let quitter = app.shell(&repo, "sleep 1.5; exit 42");
    assert!(wait_for(Duration::from_secs(10), || app
        .text(&talker)
        .contains("before-crash")));
    println!("SESSIONS {talker} {quitter}");
    std::thread::sleep(Duration::from_secs(600));
}

/// Starts the child app and returns it with the two session ids it printed.
fn start_child_app(dir: &Path, repo: &Path) -> (std::process::Child, String, String) {
    use std::io::BufRead;
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "crash_child_app",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD_ENV, "1")
        .env(DIR_ENV, dir)
        .env(REPO_ENV, repo)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn_locked()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let said: Arc<Mutex<String>> = Arc::default();
    {
        let said = said.clone();
        std::thread::spawn(move || {
            use std::io::Read;
            let mut text = String::new();
            let _ = std::io::BufReader::new(stderr).read_to_string(&mut text);
            said.lock().unwrap().push_str(&text);
        });
    }
    let (send, got) = mpsc::channel();
    std::thread::spawn(move || {
        for line in std::io::BufReader::new(stdout)
            .lines()
            .map_while(Result::ok)
        {
            // libtest prints the test's name on the same line first.
            match line.split_once("SESSIONS ") {
                Some((_, ids)) => {
                    let _ = send.send(Ok(ids.to_string()));
                }
                None => {
                    let _ = send.send(Err(line));
                }
            }
        }
    });
    let mut seen = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(30);
    let ids = loop {
        match got.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(Ok(ids)) => break ids,
            Ok(Err(line)) => seen.push(line),
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                std::thread::sleep(Duration::from_millis(200));
                panic!(
                    "the child app never started its sessions; it printed {seen:#?} and said {:?}",
                    said.lock().unwrap()
                );
            }
        }
    };
    let mut ids = ids.split_whitespace().map(str::to_owned);
    (child, ids.next().unwrap(), ids.next().unwrap())
}

/// AC: SIGKILL of the app leaves every hosted session running; the next app
/// lists them, reattaches with what was printed replayed from the host's ring,
/// and can type into them. A session that ended while no app was there is
/// reported with its real exit status.
#[test]
fn sessions_outlive_a_sigkilled_app_and_the_next_app_takes_them_over() {
    let dir = host_dir();
    let repo = repo();
    let (mut child, talker, quitter) = start_child_app(dir.path(), repo.path());
    // SIGKILL: no teardown, no goodbye, no flush.
    // SAFETY: a pid this test spawned and has not reaped.
    assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGKILL) }, 0);
    child.wait().unwrap();
    // Long enough for the talker to print after the crash and the quitter
    // to exit while no app is connected.
    std::thread::sleep(Duration::from_secs(3));

    let app = App::new(dir.path());
    let _stop = Stop(app.state.clone());
    app.state.connect_host().unwrap();
    // The quitter ended while no app was there: its exit, with its code, is
    // recorded now.
    let exit = app.exit_of(&quitter, Duration::from_secs(10));
    assert_eq!(exit["exit_code"], 42);
    assert_eq!(exit["reaped"], true);

    let listed = list_sessions(&app.state).unwrap();
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert_eq!(listed[0].id, talker);
    assert!(listed[0].detached, "the dead app's view is gone");

    attach_session(&app.state, &talker, 24, 80, app.sink()).unwrap();
    // Replayed from the ring: what it printed before and after the crash.
    let text = app.text(&talker);
    assert!(text.contains("before-crash"), "{text:?}");
    assert!(text.contains("after-crash"), "{text:?}");
    assert!(
        text.find("before-crash") < text.find("after-crash"),
        "replayed in order: {text:?}"
    );
    write_to_session(&app.state, &talker, "hello\n").unwrap();
    assert!(
        wait_for(Duration::from_secs(10), || app
            .text(&talker)
            .contains("typed-hello")),
        "{:?}",
        app.text(&talker)
    );
    assert!(
        app.losses.lock().unwrap().is_empty(),
        "nothing was lost: the ring held it all"
    );
    kill_session(&app.state, &talker).unwrap();
    assert!(list_sessions(&app.state).unwrap().is_empty());
}

/// AC: "leave running" on quit lets the host go without stopping anything,
/// and the next launch finds the session.
#[test]
fn a_quit_that_leaves_sessions_running_disconnects_and_the_next_app_finds_them() {
    let dir = host_dir();
    let repo = repo();
    let first = App::new(dir.path());
    let id = first.shell(repo.path(), "printf 'still-here\\n'; read x");
    assert!(wait_for(Duration::from_secs(10), || first
        .text(&id)
        .contains("still-here")));
    assert!(
        first.state.leaves_sessions_running(),
        "the default for a host"
    );
    gitpulse_lib::teardown::close_terminals(&first.state);
    drop(first);

    let next = App::new(dir.path());
    let _stop = Stop(next.state.clone());
    let listed = list_sessions(&next.state).unwrap();
    assert_eq!(
        listed.iter().map(|l| l.id.as_str()).collect::<Vec<_>>(),
        vec![id.as_str()]
    );
    attach_session(&next.state, &id, 24, 80, next.sink()).unwrap();
    assert!(next.text(&id).contains("still-here"));
}

/// AC: "stop sessions" on quit stops them, and their exits are recorded.
#[test]
fn a_quit_that_stops_sessions_stops_them() {
    let dir = host_dir();
    let repo = repo();
    let app = App::new(dir.path());
    let id = app.shell(repo.path(), "read x");
    app.state.set_leave_running(false);
    assert!(!app.state.leaves_sessions_running());
    gitpulse_lib::teardown::close_terminals(&app.state);
    let exit = app.exit_of(&id, Duration::from_secs(10));
    assert_eq!(exit["reaped"], true);
    let next = App::new(dir.path());
    let _stop = Stop(next.state.clone());
    assert!(list_sessions(&next.state).unwrap().is_empty());
}

/// A fake host of another protocol in the host directory, answering the
/// frozen hello with `sessions`, and recording whether it was asked to retire.
fn fake_host(dir: &Path, protocol: u32, sessions: usize) -> Arc<Mutex<Vec<String>>> {
    use gitpulse_lib::terminal::host::proto;
    let path = dir.join(gitpulse_lib::terminal::host::socket_name(protocol));
    std::fs::create_dir_all(dir).unwrap();
    let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
    let asked: Arc<Mutex<Vec<String>>> = Arc::default();
    let record = asked.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let Ok(Some((header, _))) = proto::read_frame(&mut stream) else {
                continue;
            };
            let request: serde_json::Value = serde_json::from_slice(&header).unwrap();
            let op = request["op"].as_str().unwrap_or("").to_string();
            record.lock().unwrap().push(op.clone());
            let ok = match op.as_str() {
                "hello" => {
                    serde_json::json!({"protocol": protocol, "build": "0.0.1-old", "pid": 4242, "sessions": sessions})
                }
                "retire" => serde_json::json!({"retired": sessions == 0, "sessions": sessions}),
                _ => serde_json::json!(null),
            };
            let _ = proto::write_frame(
                &mut stream,
                &serde_json::json!({"kind": "reply", "id": request["id"], "ok": ok}),
                &[],
            );
        }
    });
    asked
}

/// AC: version skew is detected and reported, never silently mis-spoken: a
/// host of another protocol with sessions is left running and named; an idle
/// one is asked to go.
#[test]
fn a_host_of_another_protocol_is_reported_with_its_sessions_or_retired_when_idle() {
    let dir = host_dir();
    let busy = fake_host(dir.path(), 999, 3);
    let idle = fake_host(dir.path(), 998, 0);
    let app = App::new(dir.path());
    let _stop = Stop(app.state.clone());
    app.state.connect_host().unwrap();
    let trouble = app.state.host_trouble();
    assert_eq!(trouble.len(), 1, "{trouble:?}");
    assert!(trouble[0].contains("protocol 999"), "{trouble:?}");
    assert!(trouble[0].contains("3 session(s)"), "{trouble:?}");
    assert!(trouble[0].contains("0.0.1-old"), "{trouble:?}");
    assert_eq!(
        *busy.lock().unwrap(),
        vec!["hello"],
        "a busy host is not asked to retire"
    );
    assert_eq!(*idle.lock().unwrap(), vec!["hello", "retire"]);
}

/// A host that answers with another protocol on this version's own socket is
/// refused, and every call says why.
#[test]
fn a_host_that_answers_with_another_protocol_is_refused_by_name() {
    let dir = host_dir();
    let protocol = gitpulse_lib::terminal::host::PROTOCOL;
    // A fake on this version's own socket name, speaking another version.
    {
        use gitpulse_lib::terminal::host::proto;
        let path = dir
            .path()
            .join(gitpulse_lib::terminal::host::socket_name(protocol));
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let Ok(Some((header, _))) = proto::read_frame(&mut stream) else {
                    continue;
                };
                let request: serde_json::Value = serde_json::from_slice(&header).unwrap();
                let _ = proto::write_frame(
                    &mut stream,
                    &serde_json::json!({"kind": "reply", "id": request["id"], "ok": {"protocol": protocol + 1, "build": "x", "pid": 1, "sessions": 0}}),
                    &[],
                );
            }
        });
    }
    let state = terminals(dir.path());
    let error = list_sessions(&state).unwrap_err();
    assert!(
        error.contains(&format!("speaks protocol {}", protocol + 1)),
        "{error}"
    );
    assert!(!state.host_trouble().is_empty());
}
