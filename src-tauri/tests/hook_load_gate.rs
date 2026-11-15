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
        self.ask_as(SESSION, command, mode, tool_use_id)
    }

    fn ask_as(
        &self,
        session: &str,
        command: &str,
        mode: &str,
        tool_use_id: &str,
    ) -> (Option<i32>, String) {
        let payload = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "tool_name": "Bash",
            "tool_input": { "command": command },
            "cwd": self.home,
            "session_id": session,
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

#[test]
fn the_session_brief_carries_one_load_line_read_from_the_snapshot() {
    let world = World::new();
    world.publish(severe());
    let repo = world.home.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    assert!(Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .current_dir(&repo)
        .status_locked()
        .unwrap()
        .success());
    let payload = serde_json::json!({
        "hook_event_name": "SessionStart",
        "cwd": repo.canonicalize().unwrap(),
        "session_id": SESSION,
        "source": "startup",
    })
    .to_string();
    let mut child = Command::new(HOOK)
        .arg("session-brief")
        .env(gitpulse_lib::logging::LOG_DIR_ENV, &world.logs)
        .env("HOME", &world.home)
        .env("TMPDIR", &world.tmp)
        .env("XDG_CONFIG_HOME", world.home.join(".config"))
        .env("GITPULSE_TOOL_CONFIG", &world.config)
        .env("GITPULSE_MANVI_BIN", world.home.join("no-such-manvi"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn_locked()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(0));
    let out = parsed(&String::from_utf8(output.stdout).unwrap());
    let brief = out["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    let lines: Vec<_> = brief
        .lines()
        .filter(|l| l.starts_with("host load:"))
        .collect();
    assert_eq!(lines.len(), 1, "{brief}");
    assert!(
        lines[0].contains("severe, swap in use") && lines[0].contains("not admitting launches"),
        "{brief}"
    );
    assert_eq!(
        brief.lines().nth(2),
        Some(lines[0]),
        "right under the header: {brief}"
    );
}

/// A cargo that holds its slot until the test releases it.
const FAKE_CARGO: &str = "#!/bin/sh\nwhile [ ! -e \"$RELEASE\" ]; do sleep 0.1; done\n";

fn wait_until(what: &str, mut ready: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(90);
    while !ready() {
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

#[test]
fn a_heavy_command_with_all_slots_held_is_told_it_will_queue() {
    use std::os::unix::fs::PermissionsExt;
    let world = World::new();
    std::fs::write(
        &world.config,
        r#"{"version":1,"host_load_leases":{"slots":1}}"#,
    )
    .unwrap();
    let bin = world.home.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(bin.join("cargo"), FAKE_CARGO).unwrap();
    std::fs::set_permissions(bin.join("cargo"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let release = world.home.join("release");
    // Another session's build holds the only slot.
    let mut holder = Command::new(HOOK)
        .args(["lease", "--", "cargo", "build"])
        .env_clear()
        .env("HOME", &world.home)
        .env("TMPDIR", &world.tmp)
        .env("XDG_CONFIG_HOME", world.home.join(".config"))
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .env("GITPULSE_TOOL_CONFIG", &world.config)
        .env("GITPULSE_TERMINAL_SESSION", "term-other")
        .env("RELEASE", &release)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn_locked()
        .expect("spawn the holder");
    let record = config_dir(&world.home).join("leases/slot-1.json");
    wait_until("the holder's slot record", || record.exists());
    let (code, out) = world.ask("cargo build", "default", "toolu_1");
    assert_eq!(code, Some(0));
    let context = parsed(&out)["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(context.contains("Every build slot is held"), "{context}");
    assert!(
        context.contains("cargo build"),
        "it names who holds it: {context}"
    );
    assert!(
        context.contains("600000 ms timeout or run it in the background"),
        "{context}"
    );
    // A build that is not leased is not told about a queue.
    let (_, out) = world.ask("cargo fmt --check", "default", "toolu_2");
    assert!(!out.contains("slot"), "{out}");
    std::fs::write(&release, "go").unwrap();
    assert!(holder.wait().unwrap().success());
    // The slot is free again: no queue line.
    let (_, out) = world.ask("cargo build", "default", "toolu_3");
    assert!(!out.contains("Every build slot is held"), "{out}");
}

#[test]
fn session_brief_writes_lease_path_to_claude_env_file() {
    let world = World::new();
    let config = config_dir(&world.home);
    gitpulse_lib::sysload::lease::ensure_links(&config, Path::new(HOOK)).unwrap();
    let repo = world.home.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    assert!(Command::new("git")
        .args(["init", "-q"])
        .current_dir(&repo)
        .status()
        .unwrap()
        .success());
    let env_file = world.home.join("claude-env");
    let payload = serde_json::json!({
        "hook_event_name": "SessionStart",
        "cwd": repo,
        "session_id": SESSION,
        "source": "startup",
    })
    .to_string();
    let mut child = Command::new(HOOK)
        .arg("session-brief")
        .env(gitpulse_lib::logging::LOG_DIR_ENV, &world.logs)
        .env("HOME", &world.home)
        .env("TMPDIR", &world.tmp)
        .env("XDG_CONFIG_HOME", world.home.join(".config"))
        .env("GITPULSE_TOOL_CONFIG", &world.config)
        .env("GITPULSE_MANVI_BIN", world.home.join("no-such-manvi"))
        .env("CLAUDE_ENV_FILE", &env_file)
        .env_remove("GITPULSE_LEASE_BIN")
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
    assert_eq!(output.status.code(), Some(0));
    let brief = parsed(&String::from_utf8(output.stdout).unwrap())["hookSpecificOutput"]
        ["additionalContext"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(brief.contains("build leases: on"), "{brief}");
    let written = std::fs::read_to_string(&env_file).unwrap();
    assert_eq!(
        written,
        format!(
            "export PATH=\"{}:$PATH\"\n",
            config.join("lease-bin").display()
        )
    );
    // The command gate now knows this session is gated and says nothing about it.
    let (_, out) = world.ask("cargo build", "default", "toolu_1");
    assert!(!out.contains("did NOT lease-gate"), "{out}");
    // Another session, which never got the folder on its PATH, is told.
    let (_, out) = world.ask_as(
        "00000000-aaaa-4bbb-8ccc-000000000000",
        "cargo build",
        "default",
        "toolu_3",
    );
    assert!(out.contains("did NOT lease-gate this build"), "{out}");
}
