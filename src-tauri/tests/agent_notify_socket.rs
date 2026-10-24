//! The hook-to-app notification path, end to end, at the process boundary.
//!
//! Everything below the process is unit tested: the report's shape, the
//! socket's refusals, the scanner, the worker. What none of that can prove is
//! the part that actually fails in the field — that the *installed binary*, run
//! the way a host runs it, with the environment GitPulse gives it, writes
//! something the running app accepts.
//!
//! That gap has bitten this repository before: the plugin's hooks spawned a
//! binary no install ever shipped, and every source-level check was green.
//! So this test is the real executable, a real Unix socket and a real
//! Claude-shaped payload, with the app's own parser deciding whether what
//! arrived is a notice.

#![cfg(unix)]

// The hermetic test environment: a per-process home, git config and trust
// store, rewritten before `main`. See `src/test_support/env.rs`.
#[path = "../src/test_support/env.rs"]
mod hermetic_env;

use gitpulse_lib::procguard::LockedSpawn;
use std::io::{Read, Write};
use std::os::unix::net::UnixListener;
use std::process::{Command, Stdio};
use std::time::Duration;

/// A directory short enough to hold a socket.
///
/// `sun_path` is 104 bytes on macOS and the system temp directory spends much
/// of it, which is the same ceiling `alerts::bridge::bind` reports rather than
/// letting it surface as `ENAMETOOLONG`. Canonicalised because `/tmp` is a
/// symlink there and the bridge refuses symlinked ancestors.
struct ShortDir(std::path::PathBuf);

impl ShortDir {
    fn new(tag: &str) -> Self {
        // Unique per call, not per event name: two tests exercise the same
        // event concurrently, and a shared path would have them binding over
        // one another — which presents as an unexplained missing report.
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let serial = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let base = std::fs::canonicalize("/tmp").expect("a temporary directory");
        let path = base.join(format!("gph-{tag}-{}-{serial}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("scratch directory");
        Self(path)
    }
    fn socket(&self) -> std::path::PathBuf {
        self.0.join("agent-notify.sock")
    }
}

impl Drop for ShortDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The longest the helper waits for a hook that is still running to connect.
/// Once the hook has exited the wait ends at once; this only bounds a hook that
/// neither connects nor exits.
const ACCEPT_DEADLINE: Duration = Duration::from_secs(30);

/// Runs the real hook binary against a listener this test owns, and returns
/// what reached the socket, or `None` when the hook exited without connecting.
fn report_through_socket(
    event: &str,
    payload: &str,
    session: Option<&str>,
    agent: Option<&str>,
) -> Option<Vec<u8>> {
    exchange(
        event,
        payload,
        session,
        agent,
        b"{\"ok\":true}\n",
        Duration::ZERO,
    )
    .sent
}

/// What one run of the real hook binary did.
struct Exchange {
    /// What reached the socket, or `None` when the hook never connected.
    sent: Option<Vec<u8>>,
    stdout: Vec<u8>,
    stderr: String,
    /// When the test gave the hook its stdin, on the bridge's clock.
    stdin_at: u64,
}

/// [`report_through_socket`] answering with `reply` instead of an
/// acknowledgement, holding back the hook's stdin for `stdin_delay`, and
/// keeping what the hook wrote to stdout and stderr.
fn exchange(
    event: &str,
    payload: &str,
    session: Option<&str>,
    agent: Option<&str>,
    reply: &'static [u8],
    stdin_delay: Duration,
) -> Exchange {
    let dir = ShortDir::new(event);
    let listener = UnixListener::bind(dir.socket()).expect("bind");
    listener.set_nonblocking(false).unwrap();

    let logs = tempfile::tempdir().unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_gitpulse-hook"));
    command
        .arg("notify")
        .arg(event)
        .env(gitpulse_lib::logging::LOG_DIR_ENV, logs.path())
        .env(
            gitpulse_lib::alerts::bridge::SOCKET_ENV,
            dir.socket().as_os_str(),
        )
        .env_remove(gitpulse_lib::alerts::bridge::SESSION_ENV)
        .env_remove(gitpulse_lib::hooks::AGENT_KIND_ENV)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(session) = session {
        command.env(gitpulse_lib::alerts::bridge::SESSION_ENV, session);
    }
    if let Some(agent) = agent {
        command.env(gitpulse_lib::hooks::AGENT_KIND_ENV, agent);
    }
    let mut child = command.spawn_locked().expect("spawn gitpulse-hook");
    // The hook is blocked reading stdin for this long, so anything it stamps
    // "at entry" must be older than `stdin_at`, taken just before the write.
    std::thread::sleep(stdin_delay);
    let stdin_at = gitpulse_lib::alerts::bridge::order_stamp_ns();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();

    // Accept on another thread while the hook runs, because the hook waits for
    // the app's answer. The accept is bounded: a hook that exits without
    // connecting — which `run_notify` does on purpose for a state report with
    // no session — must come back as "nothing arrived", not as a test that
    // waits forever on a connection no process will ever make.
    listener.set_nonblocking(true).unwrap();
    let hook_exited = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let exited = std::sync::Arc::clone(&hook_exited);
    let accepted = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + ACCEPT_DEADLINE;
        let mut stream = loop {
            // Read the flag before trying: a connection the hook made before
            // it exited is already queued, so the try that follows a set flag
            // is guaranteed to see it.
            let exited = exited.load(std::sync::atomic::Ordering::Acquire);
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if exited || std::time::Instant::now() >= deadline {
                        return None;
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(_) => return None,
            }
        };
        // BSD hands the listener's O_NONBLOCK to the accepted socket; the read
        // below must block until the hook closes its half.
        stream.set_nonblocking(false).ok()?;
        // Best effort: macOS refuses `SO_RCVTIMEO` on an `AF_UNIX` socket with
        // `EINVAL`, which is why the bound that actually matters is structural
        // — the caller waits for the child before joining this thread, so by
        // then the only writer has exited and the read is already at EOF.
        let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
        let mut bytes = Vec::new();
        stream.read_to_end(&mut bytes).ok()?;
        // Answer as the app would, so the hook's half of the exchange is the
        // one it will really see.
        let _ = stream.write_all(reply);
        Some(bytes)
    });

    let status = child.wait().expect("hook exits");
    hook_exited.store(true, std::sync::atomic::Ordering::Release);
    assert_eq!(
        status.code(),
        Some(0),
        "a hook must never block a user's turn, whatever happened"
    );
    // A hook writes a line or two, far under a pipe's buffer, so reading after
    // the exit loses nothing and cannot have blocked it.
    let mut stdout = Vec::new();
    let mut stderr = String::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_end(&mut stdout)
        .unwrap();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    Exchange {
        sent: accepted.join().unwrap(),
        stdout,
        stderr,
        stdin_at,
    }
}

/// The payload shape Claude Code documents for every hook event.
fn claude_payload(cwd: &str) -> String {
    format!(
        r#"{{"session_id":"claude-abc","transcript_path":"/tmp/t.jsonl","cwd":"{cwd}","permission_mode":"default","hook_event_name":"Notification","message":"Claude needs your permission to use Bash"}}"#
    )
}

#[test]
fn the_installed_hook_reaches_the_socket_and_the_app_accepts_what_it_sends() {
    let bytes = report_through_socket(
        "permission_prompt",
        &claude_payload("/Users/me/GitPulse"),
        Some("term-7-1a"),
        Some("claude"),
    )
    .expect("a report reached the socket");
    let notice = gitpulse_lib::alerts::bridge::parse_report(&bytes)
        .expect("the app's own parser accepts what the binary sent");
    assert_eq!(notice.key, "term-7-1a");
    assert_eq!(notice.label, "Claude Code");
    assert_eq!(notice.place.as_deref(), Some("GitPulse"));
    assert_eq!(notice.reason(), Some("needs your permission"));
    assert_eq!(
        notice.detail.as_deref(),
        Some("Claude needs your permission to use Bash")
    );
}

#[test]
fn every_event_the_shipped_manifest_routes_survives_the_process_boundary() {
    // Read from the manifest a host will actually install, not from a list
    // here: the failure this catches is a matcher added to the plugin that the
    // binary refuses, which looks exactly like notifications not working.
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("plugins/gitpulse/hooks/hooks.json");
    let parsed: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(manifest).unwrap()).unwrap();
    let mut checked = 0;
    for groups in parsed["hooks"].as_object().unwrap().values() {
        for group in groups.as_array().into_iter().flatten() {
            for handler in group["hooks"].as_array().into_iter().flatten() {
                let command = handler["command"].as_str().unwrap_or_default();
                let mut words = command.split_whitespace().skip(1);
                if words.next() != Some("notify") {
                    continue;
                }
                let event = words.next().expect("notify names its event");
                let role = gitpulse_lib::alerts::bridge::event(event)
                    .unwrap_or_else(|| {
                        panic!("the manifest routes {event}, which the app does not know")
                    })
                    .role;
                let payload = claude_payload("/Users/me/work");
                if role == gitpulse_lib::alerts::bridge::Role::Banner {
                    // A banner is worth sending from any session, GitPulse's or not.
                    let bytes = report_through_socket(event, &payload, None, Some("codex"))
                        .unwrap_or_else(|| panic!("{event} sent nothing"));
                    let notice = gitpulse_lib::alerts::bridge::parse_report(&bytes)
                        .unwrap_or_else(|e| panic!("{event} was refused by the app: {e}"));
                    assert_eq!(notice.label, "Codex");
                    assert!(notice.key.starts_with("hook-codex-"), "{}", notice.key);
                } else {
                    // A state or a resolution is about a GitPulse terminal: it
                    // is sent from one and accepted for it…
                    let bytes =
                        report_through_socket(event, &payload, Some("term-3-9c"), Some("codex"))
                            .unwrap_or_else(|| {
                                panic!("{event} sent nothing from a GitPulse session")
                            });
                    let notice = gitpulse_lib::alerts::bridge::parse_report(&bytes)
                        .unwrap_or_else(|e| panic!("{event} was refused by the app: {e}"));
                    assert_eq!(notice.label, "Codex");
                    assert_eq!(notice.key, "term-3-9c");
                    // …and outside one the hook sends nothing, because the app
                    // would refuse it ("state report outside a session").
                    assert!(
                        report_through_socket(event, &payload, None, Some("codex")).is_none(),
                        "{event} reached the socket with no GitPulse session"
                    );
                }
                checked += 1;
            }
        }
    }
    assert!(
        checked >= 5,
        "only {checked} notify entries were exercised; the manifest scan is not finding them"
    );
}

#[test]
fn a_hook_that_exits_without_connecting_is_reported_promptly_not_awaited() {
    // `permission_request` is a state report; with no GitPulse session the hook
    // exits without connecting. The helper used to block in `accept` for that
    // connection forever, which hung the whole test binary instead of failing.
    let started = std::time::Instant::now();
    let arrived = report_through_socket(
        "permission_request",
        &claude_payload("/Users/me/work"),
        None,
        Some("claude"),
    );
    assert!(
        arrived.is_none(),
        "a state report with no session reached the socket"
    );
    assert!(
        started.elapsed() < hermetic_env::coverage_relaxed(Duration::from_secs(10)),
        "waiting for a connection that never came took {:?}",
        started.elapsed()
    );
}

#[test]
fn a_socket_that_is_not_there_costs_the_turn_nothing() {
    // The ordinary case: the plugin is installed and GitPulse is closed. The
    // hook must exit 0, promptly, and print nothing a host could read as a
    // decision.
    let dir = ShortDir::new("absent");
    let logs = tempfile::tempdir().unwrap();
    let started = std::time::Instant::now();
    let output = Command::new(env!("CARGO_BIN_EXE_gitpulse-hook"))
        .arg("notify")
        .arg("agent_completed")
        .env(gitpulse_lib::logging::LOG_DIR_ENV, logs.path())
        .env(
            gitpulse_lib::alerts::bridge::SOCKET_ENV,
            dir.socket().as_os_str(),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn_locked()
        .and_then(|mut child| {
            child
                .stdin
                .take()
                .unwrap()
                .write_all(claude_payload("/tmp/x").as_bytes())?;
            child.wait_with_output()
        })
        .expect("hook runs");
    assert_eq!(output.status.code(), Some(0));
    assert!(
        output.stdout.is_empty(),
        "a report wrote something onto the decision channel: {:?}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        started.elapsed() < hermetic_env::coverage_relaxed(Duration::from_secs(10)),
        "an absent socket held the turn for {:?}",
        started.elapsed()
    );
}

/// What the app's own parser reads as a report's `seq`.
fn seq_of(bytes: &[u8]) -> u64 {
    gitpulse_lib::alerts::bridge::parse_report_stamped(bytes)
        .expect("the app's own parser accepts what the binary sent")
        .1
        .expect("the report carries a seq")
}

#[test]
fn two_real_hook_spawns_produce_a_monotone_seq_on_the_bridges_clock() {
    let before = gitpulse_lib::alerts::bridge::order_stamp_ns();
    let first = report_through_socket(
        "turn_finished",
        &claude_payload("/Users/me/GitPulse"),
        Some("term-7-1a"),
        Some("claude"),
    )
    .expect("the first report reached the socket");
    let second = report_through_socket(
        "tool_finished",
        &claude_payload("/Users/me/GitPulse"),
        Some("term-7-1a"),
        Some("claude"),
    )
    .expect("the second report reached the socket");
    let after = gitpulse_lib::alerts::bridge::order_stamp_ns();
    let (first, second) = (seq_of(&first), seq_of(&second));
    assert!(
        before <= first && first < second && second <= after,
        "seq is not monotone on the bridge's clock: {before} {first} {second} {after}"
    );
}

#[test]
fn the_hook_stamps_seq_before_it_reads_stdin() {
    // The hook sits in a stdin read for 300 ms. A stamp taken after the read
    // would be newer than the moment the test gave it its input.
    let run = exchange(
        "turn_finished",
        &claude_payload("/Users/me/GitPulse"),
        Some("term-7-1a"),
        Some("claude"),
        b"{\"ok\":true}\n",
        Duration::from_millis(300),
    );
    let seq = seq_of(&run.sent.expect("a report reached the socket"));
    assert!(
        seq < run.stdin_at,
        "seq {seq} was taken after stdin was given at {}",
        run.stdin_at
    );
}

/// The app's refusal, as it reaches the hook's stderr; stdout stays empty.
fn refused_with(reply: &'static [u8]) -> Exchange {
    let run = exchange(
        "agent_completed",
        &claude_payload("/Users/me/GitPulse"),
        None,
        Some("claude"),
        reply,
        Duration::ZERO,
    );
    assert!(run.sent.is_some(), "the report was not sent");
    assert!(
        run.stdout.is_empty(),
        "a refusal wrote onto the decision channel: {:?}",
        String::from_utf8_lossy(&run.stdout)
    );
    run
}

#[test]
fn the_hook_prints_the_apps_reason_verbatim_and_blames_no_version() {
    for token in ["queue_full", "stale", "not_running", "unknown_agent"] {
        let reply: &'static [u8] = Box::leak(
            format!("{{\"ok\":false,\"reason\":\"{token}\"}}\n")
                .into_bytes()
                .into_boxed_slice(),
        );
        let run = refused_with(reply);
        assert!(
            run.stderr.contains(token),
            "{token} missing from {:?}",
            run.stderr
        );
        assert!(
            !run.stderr.contains("different versions"),
            "{token} was blamed on versions: {:?}",
            run.stderr
        );
    }
}

/// A new hook talking to an app that predates reasons: a bare `ok:false`. The
/// hook says it was refused with no reason, and does not guess at versions.
#[test]
fn a_new_hook_talking_to_an_app_that_gives_no_reason_guesses_nothing() {
    let run = refused_with(b"{\"ok\":false}\n");
    assert!(run.stderr.contains("refused"), "{:?}", run.stderr);
    assert!(run.stderr.contains("no reason"), "{:?}", run.stderr);
    assert!(
        !run.stderr.contains("different versions"),
        "{:?}",
        run.stderr
    );
}

#[test]
fn a_version_refusal_is_the_one_refusal_that_talks_about_versions() {
    let run = refused_with(b"{\"ok\":false,\"reason\":\"unsupported_version\",\"max_v\":9}\n");
    assert!(
        run.stderr.contains("unsupported_version"),
        "{:?}",
        run.stderr
    );
    assert!(run.stderr.contains("v9"), "{:?}", run.stderr);
}

/// A new hook against an app that acknowledges as the old one did: success is
/// success, the seq it sent is simply an unknown field to that app.
#[test]
fn a_new_hook_talking_to_an_app_that_ignores_seq_still_succeeds() {
    let run = exchange(
        "agent_completed",
        &claude_payload("/Users/me/GitPulse"),
        None,
        Some("claude"),
        b"{\"ok\":true}\n",
        Duration::ZERO,
    );
    assert!(run.sent.is_some());
    assert!(run.stdout.is_empty());
    assert!(
        !run.stderr.contains("refused") && !run.stderr.contains("not delivered"),
        "{:?}",
        run.stderr
    );
}

/// ft-6060bbd7: a report writes nothing to stdout, even when its input is
/// malformed, so nothing a host could read as a decision ever appears.
#[test]
fn malformed_input_writes_nothing_to_stdout() {
    let run = exchange(
        "agent_completed",
        "this is not json",
        None,
        Some("claude"),
        b"{\"ok\":true}\n",
        Duration::ZERO,
    );
    assert!(
        run.stdout.is_empty(),
        "{:?}",
        String::from_utf8_lossy(&run.stdout)
    );
}
