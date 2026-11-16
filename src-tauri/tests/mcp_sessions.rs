//! End-to-end integration tests for `gitpulse_sessions` over `gitpulse-mcp`.
//!
//! Spawns real OS fake-agent processes (`claude` and `codex` copies of this
//! test binary) with controlled ancestry, `GITPULSE_RUN_ID`,
//! `GITPULSE_SESSION_ID`, Claude registry entries, offline vs live alerts
//! snapshot state, boundedness (`limit`), and zero-git-spawn enforcement via
//! git `trace2`.

mod common;

use gitpulse_lib::alerts::{
    default_state_snapshot_path, read_state_snapshot, start as start_alerts, try_offer, Host,
    HubStateRead, Notice, Origin,
};
use gitpulse_lib::procguard::LockedSpawn;
use gitpulse_lib::repository_trust;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const ROLE_ENV: &str = "GITPULSE_FAKE_AGENT_ROLE";
const MCP_BIN_ENV: &str = "GITPULSE_MCP_BIN";
const LOG_DIR_ENV: &str = "GITPULSE_LOG_DIR";

/// Helper test entrypoint used when this compiled test binary is copied as
/// `claude` or `codex` and launched as a fake agent process.
#[test]
fn fake_agent_process_entry() {
    let Ok(role) = std::env::var(ROLE_ENV) else {
        return;
    };
    match role.as_str() {
        "sleep" => {
            println!();
            println!("FAKE_AGENT_READY");
            std::io::stdout().flush().expect("flush ready");
            let mut buf = [0_u8; 64];
            let mut stdin = std::io::stdin().lock();
            while let Ok(n) = stdin.read(&mut buf) {
                if n == 0 {
                    break;
                }
            }
        }
        "caller" => {
            // Terminate libtest's partial `test ... ` line before handing
            // stdout to `gitpulse-mcp`'s JSON-RPC stream.
            println!();
            println!("FAKE_CALLER_READY");
            std::io::stdout().flush().expect("flush caller ready");
            let mcp_bin = std::env::var(MCP_BIN_ENV).expect(MCP_BIN_ENV);
            let log_dir = std::env::var(LOG_DIR_ENV).expect(LOG_DIR_ENV);
            let mut child = Command::new(&mcp_bin)
                .stdin(Stdio::inherit())
                .stdout(Stdio::inherit())
                .stderr(Stdio::null())
                .env(LOG_DIR_ENV, log_dir)
                .spawn_locked()
                .unwrap_or_else(|e| panic!("spawn {mcp_bin}: {e}"));
            let status = child.wait().expect("wait gitpulse-mcp");
            assert!(status.success(), "gitpulse-mcp exited with {status}");
        }
        other => panic!("unexpected {ROLE_ENV}={other}"),
    }
}

struct SnapshotHost {
    path: PathBuf,
}

impl Host for SnapshotHost {
    fn submit(&self, _: &str, _: &str, _: &str, _: bool) -> Result<bool, String> {
        Ok(true)
    }

    fn attended(&self, _: &str) -> bool {
        false
    }

    fn state_snapshot_path(&self) -> Option<PathBuf> {
        Some(self.path.clone())
    }
}

struct ChildGuard {
    child: Child,
    stdin: Option<ChildStdin>,
}

impl ChildGuard {
    fn pid(&self) -> u32 {
        self.child.id()
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        drop(self.stdin.take());
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct CallerMcpSession {
    guard: ChildGuard,
    lines: Receiver<Value>,
}

impl CallerMcpSession {
    fn send_and_wait(&mut self, id: u64, method: &str, params: Value) -> Value {
        let mut payload = params;
        if !payload.is_object() {
            payload = json!({});
        }
        payload.as_object_mut().expect("object").insert(
            "_meta".into(),
            json!({
                "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                "io.modelcontextprotocol/clientInfo": { "name": "gitpulse-mcp-sessions-test", "version": "0" },
                "io.modelcontextprotocol/clientCapabilities": {}
            }),
        );
        let frame = serde_json::to_string(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": payload,
        }))
        .expect("serialize request");
        let stdin = self.guard.stdin.as_mut().expect("caller stdin open");
        writeln!(stdin, "{frame}").expect("write request");
        stdin.flush().expect("flush request");

        let timeout = common::env::coverage_relaxed(Duration::from_secs(15));
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let Ok(reply) = self.lines.recv_timeout(left) else {
                panic!("timed out waiting for JSON-RPC response id={id} ({method})");
            };
            if reply["id"].as_u64() == Some(id) {
                return reply;
            }
        }
    }

    fn call_tool(&mut self, id: u64, name: &str, arguments: Value) -> Value {
        let reply = self.send_and_wait(
            id,
            "tools/call",
            json!({
                "name": name,
                "arguments": arguments,
            }),
        );
        assert!(
            reply["error"].is_null(),
            "{name} returned JSON-RPC error: {}",
            reply["error"]
        );
        let result = &reply["result"];
        assert_ne!(
            result["isError"], true,
            "{name} returned isError: {}",
            result["content"][0]["text"]
        );
        result["structuredContent"].clone()
    }
}

fn spawn_sleep_agent(bin: &Path, cwd: &Path, extra_env: &[(&str, &str)]) -> ChildGuard {
    let mut cmd = Command::new(bin);
    cmd.args(["--exact", "fake_agent_process_entry", "--nocapture"])
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .env(ROLE_ENV, "sleep");
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let mut child = cmd
        .spawn_locked()
        .unwrap_or_else(|e| panic!("spawn fake agent {}: {e}", bin.display()));
    let stdin = child.stdin.take();
    let stdout = child.stdout.take().expect("agent stdout");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if line.trim() == "FAKE_AGENT_READY" {
                let _ = tx.send(());
                break;
            }
        }
    });
    let ready_timeout = common::env::coverage_relaxed(Duration::from_secs(10));
    if rx.recv_timeout(ready_timeout).is_err() {
        drop(stdin);
        let _ = child.kill();
        let _ = child.wait();
        panic!(
            "fake agent {} never signalled FAKE_AGENT_READY",
            bin.display()
        );
    }
    ChildGuard { child, stdin }
}

fn spawn_caller_mcp_agent(
    bin: &Path,
    cwd: &Path,
    log_dir: &Path,
    run_id: &str,
    term_session_id: &str,
) -> CallerMcpSession {
    let mut child = Command::new(bin)
        .args(["--exact", "fake_agent_process_entry", "--nocapture"])
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .env(ROLE_ENV, "caller")
        .env(MCP_BIN_ENV, env!("CARGO_BIN_EXE_gitpulse-mcp"))
        .env(LOG_DIR_ENV, log_dir)
        .env("GITPULSE_RUN_ID", run_id)
        .env("GITPULSE_SESSION_ID", term_session_id)
        .spawn_locked()
        .unwrap_or_else(|e| panic!("spawn fake caller {}: {e}", bin.display()));
    let stdin = child.stdin.take();
    let stdout = child.stdout.take().expect("caller stdout");
    let (ready_tx, ready_rx) = mpsc::channel();
    let (json_tx, lines) = mpsc::channel();
    std::thread::spawn(move || {
        let mut signaled_ready = false;
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if !signaled_ready && trimmed == "FAKE_CALLER_READY" {
                signaled_ready = true;
                let _ = ready_tx.send(());
                continue;
            }
            if !trimmed.starts_with('{') {
                continue;
            }
            let parsed = serde_json::from_str::<Value>(trimmed)
                .unwrap_or_else(|e| panic!("invalid JSON line from fake caller ({e}): {trimmed}"));
            if json_tx.send(parsed).is_err() {
                break;
            }
        }
    });
    let ready_timeout = common::env::coverage_relaxed(Duration::from_secs(10));
    if ready_rx.recv_timeout(ready_timeout).is_err() {
        drop(stdin);
        let _ = child.kill();
        let _ = child.wait();
        panic!(
            "fake caller {} never signalled FAKE_CALLER_READY",
            bin.display()
        );
    }
    CallerMcpSession {
        guard: ChildGuard { child, stdin },
        lines,
    }
}

fn canonical(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

fn copy_fake_agent_binary(dir: &Path, name: &str) -> PathBuf {
    let exe = std::env::current_exe().expect("current_exe");
    let dst = dir.join(name);
    std::fs::copy(&exe, &dst)
        .unwrap_or_else(|e| panic!("copy {} -> {}: {e}", exe.display(), dst.display()));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dst, std::fs::Permissions::from_mode(0o755))
            .expect("chmod fake agent");
    }
    dst
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| u64::try_from(d.as_millis()).ok())
        .unwrap_or(0)
}

fn row_by_pid(rows: &[Value], pid: u32) -> &Value {
    rows.iter()
        .find(|row| row["pid"].as_u64() == Some(u64::from(pid)))
        .unwrap_or_else(|| panic!("missing session pid={pid} in {rows:?}"))
}

#[cfg(unix)]
#[test]
fn gitpulse_sessions_identifies_caller_sources_states_bounds_and_spawns_no_git() {
    common::unmetered_spawns();

    let workspace = tempfile::tempdir().expect("workspace");
    let main_wt = workspace.path().join("repo-main");
    let linked_wt = workspace.path().join("repo-linked");
    std::fs::create_dir_all(&main_wt).expect("mkdir main_wt");
    common::run_git(&main_wt, &["init", "-q"]);
    common::run_git(&main_wt, &["commit", "-q", "--allow-empty", "-m", "init"]);
    common::run_git(
        &main_wt,
        &[
            "worktree",
            "add",
            "-q",
            linked_wt.to_str().expect("linked str"),
            "-b",
            "lane",
        ],
    );

    let main_str = main_wt.to_string_lossy().into_owned();
    let preview = repository_trust::inspect(&main_str).expect("inspect repo");
    // Persist grant so out-of-process `gitpulse-mcp` shares the hermetic trust decision.
    repository_trust::grant(&main_str, &preview.identity, true).expect("persist trust");

    let bin_dir = workspace.path().join("bin");
    let log_dir = workspace.path().join("logs");
    std::fs::create_dir_all(&bin_dir).expect("mkdir bin");
    std::fs::create_dir_all(&log_dir).expect("mkdir logs");
    let claude_bin = copy_fake_agent_binary(&bin_dir, "claude");
    let codex_bin = copy_fake_agent_binary(&bin_dir, "codex");

    // Start fake agent processes under the two worktrees.
    let claude_reg = spawn_sleep_agent(&claude_bin, &main_wt, &[]);
    let claude_unreg = spawn_sleep_agent(&claude_bin, &main_wt, &[]);
    let codex_agent = spawn_sleep_agent(
        &codex_bin,
        &linked_wt,
        &[("GITPULSE_SESSION_ID", "term-codex-3")],
    );

    // Register only `claude_reg` in the hermetic `~/.claude/sessions/<pid>.json`.
    let home = PathBuf::from(std::env::var_os("HOME").expect("hermetic HOME"));
    let sessions_dir = home.join(".claude").join("sessions");
    std::fs::create_dir_all(&sessions_dir).expect("mkdir sessions");
    let reg_file = sessions_dir.join(format!("{}.json", claude_reg.pid()));
    std::fs::write(
        &reg_file,
        serde_json::to_string(&json!({
            "pid": claude_reg.pid(),
            "cwd": canonical(&main_wt).to_string_lossy(),
            "startedAt": now_ms() + 60_000,
            "entrypoint": "cli",
            "status": "busy",
            "sessionId": "sid-reg-2",
        }))
        .expect("serialize reg"),
    )
    .expect("write reg file");

    // Configure git trace2 in `$HOME/.gitconfig` (which GitPulse's scrubbed
    // git env still reads) and verify trace2 records git invocations.
    let home_gitconfig = home.join(".gitconfig");
    let trace_log = home.join("mcp-sessions-trace2.json");
    let prior_gitconfig = std::fs::read_to_string(&home_gitconfig).unwrap_or_default();
    std::fs::write(
        &home_gitconfig,
        format!(
            "{prior_gitconfig}\n[trace2]\n\teventTarget = {}\n",
            trace_log.display()
        ),
    )
    .expect("write trace2 gitconfig");
    let probe_out = Command::new("git")
        .args(["-C", &main_str, "rev-parse", "HEAD"])
        .env_remove("GIT_CONFIG_GLOBAL")
        .output_locked()
        .expect("probe git trace2");
    assert!(probe_out.status.success(), "probe git rev-parse failed");
    assert!(
        std::fs::metadata(&trace_log)
            .map(|m| m.len() > 0)
            .unwrap_or(false),
        "expected trace2 in $HOME/.gitconfig to record git spawn"
    );
    let _ = std::fs::remove_file(&trace_log);

    // Spawn the MCP server under a fake `claude` parent process with run_id.
    let mut mcp = spawn_caller_mcp_agent(
        &claude_bin,
        &linked_wt,
        &log_dir,
        "run-gp-77",
        "term-caller-1",
    );
    let caller_pid = mcp.guard.pid();

    let discover = mcp.send_and_wait(1, "server/discover", json!({}));
    assert!(discover["error"].is_null(), "discover error: {discover}");

    // Phase 1: App NOT running -> app_state facet is ok:false, registry/none/unknown work.
    let offline = mcp.call_tool(2, "gitpulse_sessions", json!({ "repo_path": main_str }));
    assert_eq!(offline["ok"], true, "offline payload: {offline}");
    assert_eq!(offline["app_state"]["ok"], false);
    assert!(
        offline["app_state"]["error"]
            .as_str()
            .is_some_and(|e| !e.is_empty()),
        "offline app_state error must be non-empty: {offline}"
    );
    let offline_rows = offline["sessions"].as_array().expect("sessions array");
    let caller_pids: Vec<u64> = offline_rows
        .iter()
        .filter(|row| row["is_caller"] == true)
        .filter_map(|row| row["pid"].as_u64())
        .collect();
    assert_eq!(
        caller_pids,
        vec![u64::from(caller_pid)],
        "expected is_caller:true for exact caller pid {caller_pid}, got rows={offline_rows:?}"
    );

    let caller_row = row_by_pid(offline_rows, caller_pid);
    assert_eq!(caller_row["kind"], "claude");
    assert_eq!(caller_row["is_caller"], true);
    assert_eq!(caller_row["run_id"], "run-gp-77");
    assert_eq!(caller_row["tree"], "run:run-gp-77");
    assert_eq!(
        canonical(Path::new(
            caller_row["worktree"].as_str().expect("worktree")
        )),
        canonical(&linked_wt)
    );
    assert_eq!(caller_row["state"], "unknown");
    assert_eq!(caller_row["state_source"], "none");

    let reg_row = row_by_pid(offline_rows, claude_reg.pid());
    assert_eq!(reg_row["kind"], "claude");
    assert_eq!(reg_row["is_caller"], false);
    assert_eq!(reg_row["state"], "working");
    assert_eq!(reg_row["state_source"], "registry");
    assert_eq!(reg_row["session_id"], "sid-reg-2");
    assert!(reg_row["state_age_s"].as_u64().is_some());

    let unreg_row = row_by_pid(offline_rows, claude_unreg.pid());
    assert_eq!(unreg_row["kind"], "claude");
    assert_eq!(unreg_row["is_caller"], false);
    assert_eq!(unreg_row["state"], "unknown");
    assert_eq!(unreg_row["state_source"], "none");

    let codex_row = row_by_pid(offline_rows, codex_agent.pid());
    assert_eq!(codex_row["kind"], "codex");
    assert_eq!(codex_row["is_caller"], false);
    assert_eq!(codex_row["state"], "unknown");
    assert_eq!(codex_row["state_source"], "none");

    // Bounded call (`limit: 1`) must cap output and keep the caller first.
    let bounded = mcp.call_tool(
        3,
        "gitpulse_sessions",
        json!({ "repo_path": main_str, "limit": 1 }),
    );
    assert_eq!(bounded["cap"], 1);
    assert_eq!(bounded["returned"], 1);
    assert!(
        bounded["total"].as_u64().unwrap_or(0) >= 4,
        "expected >= 4 sessions in {bounded}"
    );
    assert_eq!(bounded["truncated"], true);
    let bounded_rows = bounded["sessions"].as_array().expect("bounded sessions");
    assert_eq!(bounded_rows.len(), 1);
    assert_eq!(bounded_rows[0]["pid"], caller_pid);
    assert_eq!(bounded_rows[0]["is_caller"], true);

    // Zero git processes spawned during offline + bounded calls.
    let trace_after_offline = std::fs::read_to_string(&trace_log).unwrap_or_default();
    assert!(
        trace_after_offline.trim().is_empty(),
        "gitpulse_sessions spawned git offline: {trace_after_offline}"
    );

    // Phase 2: App running -> live alerts worker publishes atomic snapshot.
    let snapshot_path = default_state_snapshot_path().expect("snapshot path");
    start_alerts(Arc::new(SnapshotHost {
        path: snapshot_path.clone(),
    }));
    let offer1 = try_offer(Notice {
        key: "term-caller-1".into(),
        origin: Origin::Hook,
        label: "Claude Code".into(),
        place: None,
        event: Some("permission_prompt"),
        detail: Some("Allow bash".into()),
        is_agent: true,
        channel: "hook",
        subject: None,
        subagent: false,
        agent_id: None,
        fully_idle: None,
        provider_session_id: None,
    });
    let offer2 = try_offer(Notice {
        key: "term-codex-3".into(),
        origin: Origin::Hook,
        label: "Codex".into(),
        place: None,
        event: Some("agent_completed"),
        detail: None,
        is_agent: true,
        channel: "hook",
        subject: None,
        subagent: false,
        agent_id: None,
        fully_idle: None,
        provider_session_id: None,
    });
    assert!(
        !matches!(offer1, gitpulse_lib::alerts::Offer::NotRunning)
            && !matches!(offer2, gitpulse_lib::alerts::Offer::NotRunning),
        "alerts worker must accept test notices: {offer1:?}, {offer2:?}"
    );

    assert!(
        common::env::wait_until(Duration::from_secs(5), || match read_state_snapshot() {
            HubStateRead::Ok(snap) => {
                let has_caller = snap
                    .sessions
                    .iter()
                    .any(|s| s.key == "term-caller-1" && s.state == "blocked");
                let has_codex = snap
                    .sessions
                    .iter()
                    .any(|s| s.key == "term-codex-3" && s.state == "done");
                has_caller && has_codex
            }
            HubStateRead::Unavailable(_) => false,
        }),
        "alerts worker never published caller(blocked) and codex(done) to snapshot",
    );

    let online = mcp.call_tool(4, "gitpulse_sessions", json!({ "repo_path": main_str }));
    assert_eq!(online["ok"], true, "online payload: {online}");
    assert_eq!(online["app_state"]["ok"], true);
    assert!(online["app_state"]["updated_ms"].as_u64().is_some());

    let online_rows = online["sessions"].as_array().expect("online sessions");
    let online_caller = row_by_pid(online_rows, caller_pid);
    assert_eq!(online_caller["state"], "blocked");
    assert_eq!(online_caller["state_source"], "hub");
    assert!(online_caller["state_age_s"].as_u64().is_some());

    let online_codex = row_by_pid(online_rows, codex_agent.pid());
    assert_eq!(online_codex["state"], "done");
    assert_eq!(online_codex["state_source"], "hub");
    assert!(online_codex["state_age_s"].as_u64().is_some());

    let online_reg = row_by_pid(online_rows, claude_reg.pid());
    assert_eq!(online_reg["state"], "working");
    assert_eq!(online_reg["state_source"], "registry");

    let online_unreg = row_by_pid(online_rows, claude_unreg.pid());
    assert_eq!(online_unreg["state"], "unknown");
    assert_eq!(online_unreg["state_source"], "none");

    // Still zero git processes spawned while reading live hub snapshot.
    let trace_after_online = std::fs::read_to_string(&trace_log).unwrap_or_default();
    assert!(
        trace_after_online.trim().is_empty(),
        "gitpulse_sessions spawned git online: {trace_after_online}"
    );

    let _ = std::fs::remove_file(&snapshot_path);
    let _ = std::fs::remove_file(&home_gitconfig);
    drop(mcp);
    drop(codex_agent);
    drop(claude_unreg);
    drop(claude_reg);
}
