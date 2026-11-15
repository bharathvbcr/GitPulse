//! The load verdict in `command-gate`, at the real executable boundary.
//!
//! The unit tests in `src/hooks/load_gate_tests.rs` prove the decision over
//! facts handed to it. This proves the part they cannot: that the hook
//! *process* finds the app's published snapshot where the app writes it, reads
//! the setting from `tools.json` itself, refuses on the app's verdict even in a
//! bypass-permissions session and with no harness to ask, and says so, rather
//! than staying silent, when there is no verdict to read.

#![cfg(unix)]

#[path = "../src/test_support/env.rs"]
mod hermetic_env;

use gitpulse_lib::procguard::LockedSpawn;
use gitpulse_lib::sysload::{
    Binding, Clock, PublishedVerdict, Reading, Snapshot, SnapshotWriter, Tier, WriterIdentity,
    SNAPSHOT_FILE_NAME,
};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const HOOK: &str = env!("CARGO_BIN_EXE_gitpulse-hook");
const SESSION: &str = "7b1f5c2e-1111-4222-8333-944455556666";

/// Where `tool_config::default_config_dir` puts the snapshot for this `HOME`.
fn config_dir(home: &Path) -> PathBuf {
    if cfg!(target_os = "macos") {
        home.join("Library/Application Support/GitPulse")
    } else {
        home.join(".config").join("gitpulse")
    }
}

struct World {
    _root: tempfile::TempDir,
    home: PathBuf,
    tmp: PathBuf,
    logs: PathBuf,
    config: PathBuf,
}

impl World {
    fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("hl")
            .tempdir_in("/tmp")
            .unwrap();
        let base = root.path().canonicalize().unwrap();
        let (home, tmp, logs) = (base.join("home"), base.join("tmp"), base.join("logs"));
        for dir in [&home, &tmp, &logs] {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::create_dir_all(config_dir(&home)).unwrap();
        let config = base.join("tools.json");
        Self {
            _root: root,
            home,
            tmp,
            logs,
            config,
        }
    }

    /// Publishes `verdict` the way the app's sampler does, then drops the
    /// writer: the snapshot stays, fresh.
    fn publish(&self, verdict: Reading<PublishedVerdict>) {
        let mut snapshot = Snapshot::empty(Clock::now(), WriterIdentity::default(), true);
        snapshot.verdict = verdict;
        let mut writer = SnapshotWriter::elect(&config_dir(&self.home))
            .unwrap()
            .expect("the election is free in a fresh home");
        writer.publish(&snapshot).unwrap();
        assert!(config_dir(&self.home).join(SNAPSHOT_FILE_NAME).exists());
    }

    fn ask(&self, command: &str, mode: &str, tool_use_id: &str) -> (Option<i32>, String) {
        let payload = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "tool_name": "Bash",
            "tool_input": { "command": command },
            "cwd": self.home,
            "session_id": SESSION,
            "tool_use_id": tool_use_id,
            "permission_mode": mode,
        })
        .to_string();
        let mut child = Command::new(HOOK)
            .arg("command-gate")
            .env(gitpulse_lib::logging::LOG_DIR_ENV, &self.logs)
            .env("HOME", &self.home)
            .env("TMPDIR", &self.tmp)
            .env("XDG_CONFIG_HOME", self.home.join(".config"))
            .env("GITPULSE_TOOL_CONFIG", &self.config)
            .env("GITPULSE_MANVI_BIN", self.home.join("no-such-manvi"))
            .env_remove("MAC_HEAVY_LABEL")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn_locked()
            .expect("spawn gitpulse-hook");
        child
            .stdin
            .take()
            .unwrap()
            .write_all(payload.as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        (
            output.status.code(),
            String::from_utf8(output.stdout).unwrap(),
        )
    }
}

fn severe() -> Reading<PublishedVerdict> {
    Reading::Value(PublishedVerdict {
        tier: Tier::Severe,
        binding: Some(Binding::Swap),
        admit: false,
        shed_background: true,
        shed_reactive: true,
    })
}

fn parsed(stdout: &str) -> serde_json::Value {
    serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("not one JSON document ({e}): {stdout:?}"))
}

#[test]
fn a_published_refusal_reaches_the_model_even_in_bypass_mode_and_with_no_harness() {
    let world = World::new();
    world.publish(severe());
    for (n, mode) in ["default", "bypassPermissions"].into_iter().enumerate() {
        let (code, out) = world.ask(
            "cd src-tauri && cargo test --lib",
            mode,
            &format!("toolu_{n}"),
        );
        assert_eq!(code, Some(0), "a hook that refuses still exits 0");
        let out = parsed(&out);
        let specific = &out["hookSpecificOutput"];
        assert_eq!(specific["hookEventName"], "PreToolUse");
        assert_eq!(specific["permissionDecision"], "deny", "{mode}: {out}");
        let reason = specific["permissionDecisionReason"].as_str().unwrap();
        assert!(
            reason.contains("cargo test") && reason.contains("swap in use"),
            "{reason}"
        );
        assert!(reason.contains("Do not retry in a loop"), "{reason}");
    }
}

#[test]
fn a_command_that_is_not_heavy_gets_nothing_about_load() {
    let world = World::new();
    world.publish(severe());
    for command in ["git status", "cargo fmt", "echo \"cargo test\"", "ls -la"] {
        let (code, out) = world.ask(command, "default", "toolu_1");
        assert_eq!(code, Some(0));
        assert!(!out.contains("load"), "{command}: {out}");
        assert!(!out.contains("deny"), "{command}: {out}");
    }
}

#[test]
fn the_setting_in_tools_json_is_read_by_the_hook_itself() {
    let world = World::new();
    world.publish(severe());
    std::fs::write(
        &world.config,
        r#"{"version":1,"host_load_hook_mode":"advise"}"#,
    )
    .unwrap();
    let (_, out) = world.ask("cargo build", "default", "toolu_1");
    let out = parsed(&out);
    assert!(
        out["hookSpecificOutput"]
            .get("permissionDecision")
            .is_none(),
        "{out}"
    );
    assert!(out["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap()
        .contains("would refuse"));
    std::fs::write(
        &world.config,
        r#"{"version":1,"host_load_hook_mode":"off"}"#,
    )
    .unwrap();
    let (code, out) = world.ask("cargo build", "default", "toolu_2");
    assert_eq!(code, Some(0));
    assert!(!out.contains("load") && !out.contains("deny"), "{out}");
}

#[test]
fn no_snapshot_or_no_verdict_is_said_never_refused_and_never_silent() {
    let world = World::new();
    // No sampler has ever written here.
    let (code, out) = world.ask("cargo test", "default", "toolu_1");
    assert_eq!(code, Some(0));
    let out = parsed(&out);
    assert!(
        out["hookSpecificOutput"]
            .get("permissionDecision")
            .is_none(),
        "{out}"
    );
    assert!(out["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap()
        .contains("did NOT load-check"));
    // A writer that carries no verdict (an older GitPulse).
    world.publish(Reading::Unavailable(
        gitpulse_lib::sysload::Why::Unsupported,
    ));
    let (_, out) = world.ask("cargo test", "default", "toolu_2");
    let out = parsed(&out);
    assert!(
        out["hookSpecificOutput"]
            .get("permissionDecision")
            .is_none(),
        "{out}"
    );
    assert!(out["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap()
        .contains("no governor verdict"));
}

#[test]
fn the_fifth_identical_refusal_in_one_session_says_stop() {
    let world = World::new();
    world.publish(severe());
    let mut last = String::new();
    for n in 1..=5 {
        let (_, out) = world.ask("cargo test", "default", &format!("toolu_{n}"));
        last = parsed(&out)["hookSpecificOutput"]["permissionDecisionReason"]
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(
            last.contains("STOP: you are looping"),
            n == 5,
            "refusal {n}: {last}"
        );
    }
    assert!(last.contains("This grants nothing"));
}

#[test]
fn many_hooks_reading_one_snapshot_at_once_all_get_the_same_answer() {
    let world = std::sync::Arc::new(World::new());
    world.publish(severe());
    let threads: Vec<_> = (0..16)
        .map(|n| {
            let world = world.clone();
            std::thread::spawn(move || world.ask("cargo clippy", "default", &format!("toolu_c{n}")))
        })
        .collect();
    for thread in threads {
        let (code, out) = thread.join().unwrap();
        assert_eq!(code, Some(0));
        assert_eq!(
            parsed(&out)["hookSpecificOutput"]["permissionDecision"],
            "deny"
        );
    }
}
