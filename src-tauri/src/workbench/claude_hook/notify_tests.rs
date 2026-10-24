//! The notification hooks a launch carries, run the way Claude Code runs them.

use super::*;
use crate::alerts::bridge::{SESSION_ENV, SOCKET_ENV};
use std::io::{Read, Write};
use std::os::unix::net::UnixListener;
use std::sync::{Arc, Mutex};

/// A socket that records each report's `event` and answers `{"ok":true}`.
struct Socket {
    path: PathBuf,
    events: Arc<Mutex<Vec<String>>>,
}

impl Socket {
    fn bind(dir: &Path) -> Self {
        let path = dir.join("n.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let seen = events.clone();
        // Serves the handful of connections the test makes, then ends with the
        // test process; nothing joins it because it only ever blocks in accept.
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut body = Vec::new();
                let _ = stream.read_to_end(&mut body);
                if let Ok(value) = serde_json::from_slice::<Value>(&body) {
                    if let Some(event) = value["event"].as_str() {
                        seen.lock().unwrap().push(event.to_owned());
                    }
                }
                let _ = stream.write_all(br#"{"ok":true}"#);
            }
        });
        Self { path, events }
    }
}

/// Every `(Claude event, notify argument)` the injected settings carry.
fn injected_notify_entries() -> Vec<(String, String)> {
    let hooks = settings_hooks(Path::new("/opt/gitpulse/gitpulse-hook")).unwrap();
    let mut entries = Vec::new();
    for (event, groups) in hooks.as_object().unwrap() {
        for group in groups.as_array().unwrap() {
            for hook in group["hooks"].as_array().unwrap() {
                let command = hook["command"].as_str().unwrap();
                if let Some((_, rest)) = command.split_once("' notify ") {
                    entries.push((event.clone(), rest.to_owned()));
                }
            }
        }
    }
    entries
}

/// A claude launched under a configuration without the plugin has only what
/// the launch put in `--settings`. Running each notify hook that carries, as
/// Claude would for its event, delivers the four reports that say what the
/// session is doing, and none of them writes to the host's stdout.
#[test]
fn a_launch_without_the_plugin_delivers_the_notification_events() {
    // Short and under /tmp: a socket path is limited to about a hundred bytes.
    let dir = tempfile::Builder::new()
        .prefix("cn")
        .tempdir_in("/tmp")
        .unwrap();
    let socket = Socket::bind(dir.path());
    let serial = crate::test_support::env::env_serial();
    let _env = crate::test_support::env::bind_env(&serial)
        .set(SOCKET_ENV, &socket.path)
        .set(SESSION_ENV, "term-7-1a")
        .remove(crate::hooks::AGENT_KIND_ENV);

    let entries = injected_notify_entries();
    assert!(!entries.is_empty());
    let input = crate::hooks::HookInput {
        cwd: "/work/repo".into(),
        tool_name: "Bash".into(),
        tool_subject: "00000000deadbeef".into(),
        ..crate::hooks::HookInput::default()
    };
    for (_, argument) in &entries {
        let output = crate::hooks::dispatch("notify", Some(argument), &input)
            .unwrap_or_else(|e| panic!("notify {argument}: {e}"));
        assert!(
            output.render_for_host(false).is_none(),
            "notify {argument} wrote to the host"
        );
    }
    let mut delivered = socket.events.lock().unwrap().clone();
    delivered.sort();
    delivered.dedup();
    for wanted in [
        "permission_request",
        "turn_finished",
        "tool_finished",
        "session_ended",
    ] {
        assert!(
            delivered.iter().any(|event| event == wanted),
            "{wanted} was not delivered; got {delivered:?}"
        );
    }
}

/// Only the reporting hooks are added beside the gate: no banner matcher, and
/// no hook that could answer the host.
#[test]
fn the_injected_notify_hooks_are_reports_and_not_banners() {
    let entries = injected_notify_entries();
    let mut arguments: Vec<&str> = entries.iter().map(|(_, a)| a.as_str()).collect();
    arguments.sort_unstable();
    assert_eq!(
        arguments,
        [
            "permission_request",
            "prompt_submitted",
            "session_ended",
            "tool_finished",
            "tool_finished",
            "turn_finished",
        ]
    );
    for (_, argument) in &entries {
        let event = crate::alerts::bridge::event(argument).unwrap();
        assert_ne!(
            event.role,
            crate::alerts::bridge::Role::Banner,
            "{argument}"
        );
    }
}
