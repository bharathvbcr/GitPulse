//! A hosted `manvi serve` end to end against a fake that speaks Manvi's socket
//! protocol: starting one, recording whose it is, adopting it from a later
//! connection, refusing one that is not ours, and leaving or stopping its runs
//! at a quit. The fake runs in this process, so it answers with this process's
//! pid — which is also what lets the tests prove a replacement is refused
//! rather than signalled when the "server" is the test itself.

use super::*;
use serde_json::{json, Value};
use std::io::BufRead;
use std::os::unix::net::UnixListener;

/// A short, canonical directory: `sun_path` is ~100 bytes and the temp
/// directory is a symlink on macOS.
fn short_dir() -> tempfile::TempDir {
    let base = std::env::temp_dir().canonicalize().unwrap();
    tempfile::Builder::new()
        .prefix("ph")
        .tempdir_in(base)
        .unwrap()
}

/// What the fake has been asked, and how it answers.
struct Shared {
    owner: String,
    controller: AtomicBool,
    jobs: Mutex<Vec<(String, String)>>,
    ops: Mutex<Vec<String>>,
}

impl Shared {
    fn saw(&self, op: &str) -> usize {
        relock(&self.ops).iter().filter(|seen| *seen == op).count()
    }

    fn reply(&self, op: &str) -> Result<Value, (&'static str, &'static str)> {
        let controller = self.controller.load(Ordering::Acquire);
        if !controller && op != "serve.status" && op != "serve.takeover" {
            return Err(("E_CONTROLLER_BUSY", "another client controls this server"));
        }
        Ok(match op {
            "serve.status" => json!({
                "protocol": 1,
                "owner_id": self.owner,
                "process_id": std::process::id(),
                "controller": controller,
                "jobs": relock(&self.jobs).iter().map(|(id, phase)| json!({
                    "id": id, "owner_id": self.owner, "session_id": "s", "process_id": 0,
                    "phase": phase, "activated": true
                })).collect::<Vec<_>>(),
                "idle_exit_ms": 300000
            }),
            "hello" => json!({
                "protocol": 1,
                "ops": ["hello", "serve.status", "serve.stop_all", "serve.takeover",
                        "work.runs.managed.stop", "work.enhancements.generate",
                        "work.enhancements.configuration"],
                "posture": "host"
            }),
            "serve.stop_all" => {
                let stopped: Vec<String> = relock(&self.jobs).drain(..).map(|(id, _)| id).collect();
                json!({"stopped": stopped, "pending": []})
            }
            _ => json!({"ok": true}),
        })
    }
}

struct FakeServer {
    shared: Arc<Shared>,
    socket: PathBuf,
    stop: Arc<AtomicBool>,
}

impl FakeServer {
    fn start(socket: &Path, jobs: &[(&str, &str)], controller: bool) -> Self {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let shared = Arc::new(Shared {
            owner: format!("manvi-{}-{stamp}-abc123", std::process::id()),
            controller: AtomicBool::new(controller),
            jobs: Mutex::new(
                jobs.iter()
                    .map(|(id, phase)| (id.to_string(), phase.to_string()))
                    .collect(),
            ),
            ops: Mutex::new(Vec::new()),
        });
        let listener = UnixListener::bind(socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let (accepting, flag) = (shared.clone(), stop.clone());
        thread::spawn(move || {
            while !flag.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        stream.set_nonblocking(false).unwrap();
                        let shared = accepting.clone();
                        thread::spawn(move || serve_connection(stream, shared));
                    }
                    Err(_) => thread::sleep(Duration::from_millis(10)),
                }
            }
        });
        Self {
            shared,
            socket: socket.to_path_buf(),
            stop,
        }
    }
}

impl Drop for FakeServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = std::fs::remove_file(&self.socket);
    }
}

fn serve_connection(stream: std::os::unix::net::UnixStream, shared: Arc<Shared>) {
    let mut writer = stream.try_clone().unwrap();
    for line in std::io::BufReader::new(stream)
        .lines()
        .map_while(Result::ok)
    {
        let request: Value = serde_json::from_str(&line).unwrap();
        let (id, op) = (
            request["id"].clone(),
            request["op"].as_str().unwrap().to_string(),
        );
        relock(&shared.ops).push(op.clone());
        let answer = match shared.reply(&op) {
            Ok(result) => json!({"id": id, "ok": true, "result": result}),
            Err((code, message)) => json!({
                "id": id, "ok": false,
                "error": {"code": code, "message": message, "retryable": true}
            }),
        };
        if writeln!(writer, "{answer}").is_err() {
            return;
        }
    }
}

/// A plan whose launcher starts the fake at the plan's socket, or panics when
/// the test says nothing may be started.
fn plan_in(dir: &Path, launches: Option<Arc<Mutex<Option<FakeServer>>>>) -> hosted::HostPlan {
    let paths = hosted::HostPaths::under(&dir.join("h"));
    let launch: hosted::Launcher = match launches {
        Some(started) => Arc::new(move |_command, paths: &hosted::HostPaths| {
            *relock(&started) = Some(FakeServer::start(&paths.socket, &[], true));
            Ok(hosted::Launched {
                exited: Arc::new(AtomicBool::new(false)),
            })
        }),
        None => Arc::new(|_, _| panic!("nothing may be started in this test")),
    };
    hosted::HostPlan { paths, launch }
}

/// Resets what a quit chose and what was last observed, whatever the test did.
struct Reset;
impl Drop for Reset {
    fn drop(&mut self) {
        hosted::reset_exit_choice();
        hosted::observe(hosted::Observed::Untried);
    }
}

/// Writes the record GitPulse leaves when it starts a server.
fn record_for(plan: &hosted::HostPlan, shared: &Shared, profile: &Path, selection: &str) {
    plan.paths.prepare().unwrap();
    hosted::write_record(
        &plan.paths,
        &hosted::Record {
            version: 1,
            socket: plan.paths.socket.display().to_string(),
            owner_id: shared.owner.clone(),
            process_id: std::process::id(),
            protocol: 1,
            selection: selection.to_string(),
            profile: profile.display().to_string(),
            restart: false,
            start_us: 0,
        },
    )
    .unwrap();
}

fn connection(
    dir: &Path,
    plan: hosted::HostPlan,
    selection: Option<ModelSelection>,
) -> (ProfileConnection, PathBuf) {
    let profile = dir.join("profile.sqlite");
    let connection =
        ProfileConnection::new_hosting(profile.clone(), selection, Some(plan)).unwrap();
    (connection, profile)
}

#[test]
fn a_server_is_started_recorded_and_adopted_by_the_next_connection() {
    let serial = test_serial();
    let _reset = Reset;
    let dir = short_dir();
    let started = Arc::new(Mutex::new(None));
    let plan = plan_in(dir.path(), Some(started.clone()));
    let (first, profile) = connection(dir.path(), plan.clone(), None);

    // The binary only has to exist for the capability probe, which a hosted
    // server's own fake answers for through this script.
    let binary = dir.path().join("manvi");
    std::fs::write(
        &binary,
        "#!/bin/sh\necho 'manvi serve --listen /abs/serve.sock'\n",
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let _binary = bind_test_binary(&serial, binary.to_str().unwrap());

    first
        .call("work.runs.managed.stop", json!({"id": "run-1"}))
        .unwrap();
    assert_eq!(hosted::observed(), hosted::Observed::Hosted);
    let server = relock(&started).take().expect("the launcher ran");
    assert!(hosted::is_private(&plan.paths.dir));
    let record = hosted::read_record(&plan.paths).unwrap().unwrap();
    assert_eq!(record.owner_id, server.shared.owner);
    assert_eq!(record.profile, profile.display().to_string());
    assert_eq!(record.selection, "default");

    // A quit that leaves the runs: disconnects, never stops.
    assert!(first.shutdown());
    assert_eq!(server.shared.saw("serve.stop_all"), 0);
    assert_eq!(server.shared.saw("serve.takeover"), 0);

    // The next launch finds the same server and does not start another.
    relock(&server.shared.jobs).push(("run-1".into(), "running".into()));
    let (second, _) = connection(dir.path(), plan_in(dir.path(), None), None);
    match second.hosted_view() {
        hosted::View::Live { jobs, complete, .. } => {
            assert!(complete);
            assert_eq!(jobs, vec![("run-1".to_string(), "running".to_string())]);
        }
        other => panic!("expected the server to be adopted, got {other:?}"),
    }
    // The adopted connection drives the run it found.
    second
        .call("work.runs.managed.stop", json!({"id": "run-1"}))
        .unwrap();
    assert_eq!(hosted::observed(), hosted::Observed::Hosted);
    assert!(second.shutdown());
}

#[test]
fn a_quit_that_stops_the_runs_asks_for_them_to_end_and_one_that_leaves_does_not() {
    let serial = test_serial();
    let _reset = Reset;
    let dir = short_dir();
    let plan = plan_in(dir.path(), None);
    plan.paths.prepare().unwrap();
    let server = FakeServer::start(
        &plan.paths.socket,
        &[("a", "running"), ("b", "running")],
        true,
    );
    let (connection, profile) = connection(dir.path(), plan.clone(), None);
    record_for(&plan, &server.shared, &profile, "default");
    let _ = &serial;

    connection
        .call("work.runs.managed.stop", json!({"id": "a"}))
        .unwrap();
    hosted::set_exit_choice(hosted::ExitChoice::Leave);
    settle_hosted_for_exit();
    assert_eq!(
        server.shared.saw("serve.stop_all"),
        0,
        "leaving must not stop anything"
    );
    assert_eq!(relock(&server.shared.jobs).len(), 2);

    let (connection, _) = self::connection(dir.path(), plan, None);
    connection
        .call("work.runs.managed.stop", json!({"id": "a"}))
        .unwrap();
    hosted::set_exit_choice(hosted::ExitChoice::Stop);
    assert!(connection.shutdown());
    assert_eq!(server.shared.saw("serve.stop_all"), 1);
    assert!(relock(&server.shared.jobs).is_empty());
}

#[test]
fn a_server_that_answers_for_another_owner_is_refused_and_nothing_is_started() {
    let _serial = test_serial();
    let _reset = Reset;
    let dir = short_dir();
    let plan = plan_in(dir.path(), None);
    plan.paths.prepare().unwrap();
    let server = FakeServer::start(&plan.paths.socket, &[("a", "running")], true);
    let (connection, profile) = connection(dir.path(), plan.clone(), None);
    record_for(&plan, &server.shared, &profile, "default");
    // GitPulse started a different server than the one now answering.
    let mut record = hosted::read_record(&plan.paths).unwrap().unwrap();
    record.owner_id = format!("manvi-{}-1-def456", std::process::id());
    hosted::write_record(&plan.paths, &record).unwrap();

    match connection.hosted_view() {
        hosted::View::Unknown(why) => {
            assert!(why.contains("was not adopted"), "{why}");
            assert!(why.contains(&server.shared.owner), "{why}");
            assert!(why.contains("def456"), "{why}");
        }
        other => panic!("a foreign owner was adopted: {other:?}"),
    }
    // Started work would be a second Manvi on one profile.
    let error = connection
        .call("work.runs.managed.stop", json!({"id": "a"}))
        .unwrap_err();
    assert!(matches!(error, HarnessError::Unavailable(_)), "{error:?}");
    assert!(matches!(hosted::observed(), hosted::Observed::Blocked(_)));
    assert_eq!(server.shared.saw("work.runs.managed.stop"), 0);
}

#[test]
fn a_server_with_no_record_or_for_another_profile_is_not_adopted() {
    let _serial = test_serial();
    let _reset = Reset;
    let dir = short_dir();
    let plan = plan_in(dir.path(), None);
    plan.paths.prepare().unwrap();
    let server = FakeServer::start(&plan.paths.socket, &[], true);
    let (connection, profile) = connection(dir.path(), plan.clone(), None);

    match connection.hosted_view() {
        hosted::View::Unknown(why) => assert!(why.contains("no record"), "{why}"),
        other => panic!("an unrecorded server was adopted: {other:?}"),
    }

    record_for(
        &plan,
        &server.shared,
        &dir.path().join("another.sqlite"),
        "default",
    );
    match connection.hosted_view() {
        hosted::View::Unknown(why) => {
            assert!(why.contains("not this profile"), "{why}");
            assert!(why.contains("another.sqlite"), "{why}");
        }
        other => panic!("another profile's server was adopted: {other:?}"),
    }

    record_for(&plan, &server.shared, &profile, "default");
    assert!(matches!(
        connection.hosted_view(),
        hosted::View::Live { .. }
    ));
}

#[test]
fn a_socket_nobody_answers_on_is_gone_and_a_file_that_is_not_a_socket_is_not() {
    let _serial = test_serial();
    let _reset = Reset;
    let dir = short_dir();
    let plan = plan_in(dir.path(), None);
    plan.paths.prepare().unwrap();
    let (connection, _) = connection(dir.path(), plan.clone(), None);

    assert_eq!(connection.hosted_view(), hosted::View::Gone);

    // A socket file whose server was killed: still nothing to adopt.
    drop(UnixListener::bind(&plan.paths.socket).unwrap());
    assert_eq!(connection.hosted_view(), hosted::View::Gone);

    // Something else at the path is never removed and never read as "gone".
    std::fs::remove_file(&plan.paths.socket).unwrap();
    std::fs::write(&plan.paths.socket, "not a socket").unwrap();
    match connection.hosted_view() {
        hosted::View::Unknown(why) => assert!(why.contains("not a socket"), "{why}"),
        other => panic!("{other:?}"),
    }
    assert!(plan.paths.socket.exists());
}

#[test]
fn another_controller_is_waited_for_but_never_taken_over() {
    let _serial = test_serial();
    let _reset = Reset;
    let dir = short_dir();
    let plan = plan_in(dir.path(), None);
    plan.paths.prepare().unwrap();
    let server = FakeServer::start(&plan.paths.socket, &[("a", "running")], false);
    let (connection, profile) = connection(dir.path(), plan.clone(), None);
    record_for(&plan, &server.shared, &profile, "default");

    match connection.hosted_view() {
        hosted::View::Unknown(why) => assert!(why.contains("another client controls"), "{why}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(server.shared.saw("serve.takeover"), 0);
    assert!(
        server.shared.saw("serve.status") >= 2,
        "it should have retried"
    );
}

#[test]
fn a_manvi_that_cannot_serve_on_a_socket_runs_as_the_stdio_child_and_says_why() {
    use std::os::unix::fs::PermissionsExt;
    let serial = test_serial();
    let _reset = Reset;
    let dir = short_dir();
    let binary = dir.path().join("old-manvi");
    // Its usage predates `--listen`, and it serves stdio like any Manvi.
    let script = r#"#!/bin/sh
if [ "$1" = "serve" ] && [ "$2" = "--help" ]; then
  echo "manvi serve [--posture host|devcouncil] [--workbench-db /absolute/profile.sqlite]"
  exit 0
fi
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/^{"id":"\([0-9]*\)".*/\1/p')
  case "$line" in
    *'"op":"hello"'*) result='{"protocol":1,"ops":["hello","work.enhancements.configuration"],"posture":"host"}' ;;
    *) result='{"ok":true,"model":"fixture-model"}' ;;
  esac
  printf '{"id":"%s","ok":true,"result":%s}\n' "$id" "$result"
done
"#;
    std::fs::write(&binary, script).unwrap();
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
    let _binary = bind_test_binary(&serial, binary.to_str().unwrap());
    let plan = plan_in(dir.path(), None);
    let (connection, _) = connection(dir.path(), plan.clone(), None);

    let answer = connection
        .call("work.enhancements.configuration", json!({}))
        .unwrap();
    assert_eq!(answer["model"], "fixture-model");
    assert!(
        relock(&connection.slot)
            .sidecar
            .as_ref()
            .unwrap()
            .child()
            .lock()
            .is_ok(),
        "the worker should be the stdio child"
    );
    let observed = hosted::observed();
    let hosted::Observed::FellBack(why) = &observed else {
        panic!("expected a fallback, got {observed:?}");
    };
    assert!(why.contains("cannot serve on a socket"), "{why}");
    let account = hosted::describe(true, &observed, None);
    assert!(!account.hosted);
    assert!(account.requested);
    assert!(
        account
            .detail
            .starts_with("Managed agent runs end when GitPulse quits: the installed Manvi"),
        "{}",
        account.detail
    );
    assert!(!plan.paths.socket.exists(), "nothing was started to listen");
    assert!(connection.shutdown());
}

#[test]
fn a_model_that_changed_waits_for_the_runs_that_keep_the_server_and_never_signals_a_process_it_is()
{
    let _serial = test_serial();
    let _reset = Reset;
    let dir = short_dir();
    let plan = plan_in(dir.path(), None);
    plan.paths.prepare().unwrap();
    let server = FakeServer::start(&plan.paths.socket, &[("a", "running")], true);
    let selection = ModelSelection::Local {
        base_url: "http://127.0.0.1:1".into(),
        model: "other".into(),
    };
    let (connection, profile) = connection(dir.path(), plan.clone(), Some(selection));
    // The server was started with the default model.
    record_for(&plan, &server.shared, &profile, "default");

    // What does not depend on the model goes on working, with the runs.
    connection
        .call("work.runs.managed.stop", json!({"id": "a"}))
        .unwrap();
    let error = connection
        .call("work.enhancements.generate", json!({}))
        .unwrap_err();
    match error {
        HarnessError::Refused(WireError { code, message, .. }) => {
            assert_eq!(code, "model_pending");
            assert!(message.contains("keeping 1 managed run going"), "{message}");
        }
        other => panic!("{other:?}"),
    }

    // Once the runs have ended the server must be replaced — and this "server"
    // is the test process itself, which is never signalled.
    relock(&server.shared.jobs).clear();
    let error = connection
        .call("work.enhancements.generate", json!({}))
        .unwrap_err();
    assert!(
        error.message().contains("could not be replaced"),
        "{}",
        error.message()
    );
}

#[test]
fn a_server_cannot_be_ended_when_it_holds_runs_or_is_not_the_recorded_process() {
    let dir = short_dir();
    let paths = hosted::HostPaths::under(&dir.path().join("h"));
    let status = |pid: u32, jobs: &[&str]| hosted::ServeStatus {
        protocol: 1,
        owner_id: format!("manvi-{pid}-1-ab"),
        process_id: pid,
        controller: true,
        jobs: jobs
            .iter()
            .map(|id| hosted::ServeJob {
                id: id.to_string(),
                phase: "running".into(),
            })
            .collect(),
    };
    let recorded = |pid: u32| hosted::Record {
        version: 1,
        socket: paths.socket.display().to_string(),
        owner_id: format!("manvi-{pid}-1-ab"),
        process_id: pid,
        protocol: 1,
        selection: "default".into(),
        profile: "/p".into(),
        restart: false,
        start_us: 1,
    };
    assert!(
        hosted::end_idle_server(&paths, &recorded(4242), &status(4242, &["a"]))
            .unwrap_err()
            .contains("holds runs")
    );
    // Reported pid differs from the recorded one.
    assert!(
        hosted::end_idle_server(&paths, &recorded(4242), &status(4243, &[]))
            .unwrap_err()
            .contains("not the one recorded")
    );
    // Never this process, and never init.
    let me = std::process::id();
    assert!(hosted::end_idle_server(&paths, &recorded(me), &status(me, &[])).is_err());
    assert!(hosted::end_idle_server(&paths, &recorded(1), &status(1, &[])).is_err());
}

/// The guarded primitive is what signals, so a recycled pid is never reached:
/// a live process whose start time is not the recorded one is refused, and is
/// not signalled.
#[test]
fn a_live_process_that_is_not_the_one_recorded_is_not_signalled() {
    use crate::procguard::LockedSpawn;
    use std::os::unix::process::CommandExt;
    let dir = short_dir();
    let paths = hosted::HostPaths::under(&dir.path().join("h"));
    let mut child = std::process::Command::new("/bin/sleep")
        .arg("30")
        .process_group(0)
        .spawn_locked()
        .expect("spawn a stand-in");
    let pid = child.id();
    let record = hosted::Record {
        version: 1,
        socket: paths.socket.display().to_string(),
        owner_id: format!("manvi-{pid}-1-ab"),
        process_id: pid,
        protocol: 1,
        selection: "default".into(),
        profile: "/p".into(),
        restart: false,
        // Not this process's start time.
        start_us: 1,
    };
    let status = hosted::ServeStatus {
        protocol: 1,
        owner_id: record.owner_id.clone(),
        process_id: pid,
        controller: true,
        jobs: Vec::new(),
    };
    let error = hosted::end_idle_server(&paths, &record, &status).unwrap_err();
    assert!(error.contains("different process"), "{error}");
    assert!(
        child.try_wait().unwrap().is_none(),
        "the stand-in must still be running"
    );
    let _ = child.kill();
    let _ = child.wait();
}
