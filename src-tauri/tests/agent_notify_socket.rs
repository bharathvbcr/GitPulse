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

/// Runs the real hook binary against a listener this test owns, and returns
/// what reached the socket.
fn report_through_socket(
    event: &str,
    payload: &str,
    session: Option<&str>,
    agent: Option<&str>,
) -> Option<Vec<u8>> {
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
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(session) = session {
        command.env(gitpulse_lib::alerts::bridge::SESSION_ENV, session);
    }
    if let Some(agent) = agent {
        command.env(gitpulse_lib::hooks::AGENT_KIND_ENV, agent);
    }
    let mut child = command.spawn().expect("spawn gitpulse-hook");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();

    // Accept on this thread; the hook is already running and will connect.
    let accepted = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().ok()?;
        // Best effort: macOS refuses `SO_RCVTIMEO` on an `AF_UNIX` socket with
        // `EINVAL`, which is why the bound that actually matters is structural
        // — the caller waits for the child before joining this thread, so by
        // then the only writer has exited and the read is already at EOF.
        let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
        let mut bytes = Vec::new();
        stream.read_to_end(&mut bytes).ok()?;
        // Answer as the app would, so the hook's half of the exchange is the
        // one it will really see.
        let _ = stream.write_all(b"{\"ok\":true}\n");
        Some(bytes)
    });

    let status = child.wait().expect("hook exits");
    assert_eq!(
        status.code(),
        Some(0),
        "a hook must never block a user's turn, whatever happened"
    );
    accepted.join().unwrap()
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
    assert_eq!(notice.reason.as_deref(), Some("needs your permission"));
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
                let bytes = report_through_socket(
                    event,
                    &claude_payload("/Users/me/work"),
                    None,
                    Some("codex"),
                )
                .unwrap_or_else(|| panic!("{event} sent nothing"));
                let notice = gitpulse_lib::alerts::bridge::parse_report(&bytes)
                    .unwrap_or_else(|e| panic!("{event} was refused by the app: {e}"));
                assert_eq!(notice.label, "Codex");
                assert!(notice.key.starts_with("hook-codex-"), "{}", notice.key);
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
        .spawn()
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
        started.elapsed() < Duration::from_secs(10),
        "an absent socket held the turn for {:?}",
        started.elapsed()
    );
}
