//! Agents attach to `gitpulse-mcp` over newline-delimited JSON-RPC on stdio.
//!
//! Library unit tests call `mcp::process_request` in-process. That cannot
//! prove the binary's stdout is a clean JSON-RPC channel: `logging::init`
//! writes stderr and a log file, and a stray print on stdout would make every
//! MCP client fail to parse the first response. This test launches the real
//! binary twice and reads the wire.

// The hermetic test environment: a per-process home, git config and trust
// store, rewritten before `main`. See `src/test_support/env.rs`.
#[path = "../src/test_support/env.rs"]
mod hermetic_env;

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use waylode_lib::procguard::LockedSpawn;

fn mcp_bin() -> &'static str {
    env!("CARGO_BIN_EXE_waylode-mcp")
}

fn modern_meta() -> Value {
    json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientInfo": { "name": "gitpulse-stdio-probe", "version": "0" },
        "io.modelcontextprotocol/clientCapabilities": {}
    })
}

fn line(id: u32, method: &str, extra: Value) -> String {
    let mut params = extra;
    if !params.is_object() {
        params = json!({});
    }
    params
        .as_object_mut()
        .expect("object")
        .insert("_meta".into(), modern_meta());
    serde_json::to_string(&json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params
    }))
    .expect("request")
}

fn speak_once() -> Vec<Value> {
    let log_dir = tempfile::tempdir().expect("log dir");
    let mut child = Command::new(mcp_bin())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("GITPULSE_LOG_DIR", log_dir.path())
        .spawn_locked()
        .unwrap_or_else(|e| panic!("spawn {}: {e}", mcp_bin()));

    let mut stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    let requests = [
        line(1, "server/discover", json!({})),
        line(2, "tools/list", json!({})),
        line(
            3,
            "tools/call",
            json!({
                "name": "gitpulse_insights",
                "arguments": { "repo_path": "/no/such/gitpulse-stdio-repo" }
            }),
        ),
    ];
    for req in &requests {
        writeln!(stdin, "{req}").expect("write request");
    }
    drop(stdin);

    // Correlate by id, never by arrival order. The server answers requests
    // concurrently — a fast `tools/list` deliberately overtakes a slow
    // `tools/call` — and the stdio binding says responses are "correlated by
    // JSON-RPC `id`". Indexing by position encoded an ordering the protocol
    // does not promise, and only passed while the server was single-threaded.
    let mut lines = Vec::new();
    let reader = BufReader::new(stdout);
    for raw in reader.lines() {
        let raw = raw.expect("stdout line");
        if raw.trim().is_empty() {
            continue;
        }
        let parsed: Value = serde_json::from_str(&raw)
            .unwrap_or_else(|e| panic!("stdout is not JSON-RPC ({e}): {raw}"));
        lines.push(parsed);
        if lines.len() == requests.len() {
            break;
        }
    }

    let status = child.wait().expect("wait");
    assert!(
        status.success() || lines.len() == requests.len(),
        "gitpulse-mcp exited {status} after {} responses",
        lines.len()
    );
    assert_eq!(
        lines.len(),
        requests.len(),
        "expected {} JSON-RPC responses on stdout, got {}",
        requests.len(),
        lines.len()
    );
    lines.sort_by_key(|line| line["id"].as_u64().unwrap_or(u64::MAX));
    lines
}

fn assert_modern_complete(resp: &Value, id: u32) {
    assert_eq!(resp["jsonrpc"], "2.0", "{resp}");
    assert_eq!(resp["id"], id, "{resp}");
    assert!(resp["error"].is_null(), "unexpected error: {resp}");
    assert_eq!(resp["result"]["resultType"], "complete", "{resp}");
}

#[test]
fn gitpulse_mcp_stdio_speaks_2026_07_28_twice() {
    // Two launches: a server that answers once and then corrupts the channel
    // on restart is exactly how an agent "works in the unit test" and fails
    // when the client reconnects.
    for launch in 1..=2 {
        let replies = speak_once();
        assert_modern_complete(&replies[0], 1);
        let versions = replies[0]["result"]["supportedVersions"]
            .as_array()
            .unwrap_or_else(|| panic!("launch {launch}: no supportedVersions: {}", replies[0]));
        assert!(
            versions.iter().any(|v| v == "2026-07-28"),
            "launch {launch}: {versions:?}"
        );
        assert!(replies[0]["result"]["capabilities"]["tools"].is_object());
        assert!(replies[0]["result"]["ttlMs"].is_number());
        assert!(replies[0]["result"]["cacheScope"].is_string());

        assert_modern_complete(&replies[1], 2);
        assert!(replies[1]["result"]["ttlMs"].is_number());
        assert!(replies[1]["result"]["cacheScope"].is_string());
        let names: Vec<&str> = replies[1]["result"]["tools"]
            .as_array()
            .expect("tools array")
            .iter()
            .map(|t| t["name"].as_str().unwrap_or(""))
            .collect();
        assert!(
            names.contains(&"waylode_insights"),
            "launch {launch}: {names:?}"
        );

        assert_modern_complete(&replies[2], 3);
        assert_eq!(replies[2]["result"]["isError"], false, "{}", replies[2]);
        let payload = &replies[2]["result"]["structuredContent"];
        assert_eq!(payload["worktrees"]["ok"], false, "{payload}");
        assert!(
            payload["worktrees"]["error"]
                .as_str()
                .is_some_and(|e| !e.is_empty()),
            "failed facet must say why: {payload}"
        );
    }
}

/// Runs the binary with `args` and a stdin held open, as a terminal would
/// leave it, and returns its exit code, stdout and stderr. A server that went
/// on to serve would wait on that stdin forever; it is killed at the deadline
/// and reported as hung.
fn run_with_args(args: &[&str]) -> (Option<i32>, String, String) {
    let log_dir = tempfile::tempdir().expect("log dir");
    let mut child = Command::new(mcp_bin())
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("GITPULSE_LOG_DIR", log_dir.path())
        .spawn_locked()
        .unwrap_or_else(|e| panic!("spawn {}: {e}", mcp_bin()));
    let held_stdin = child.stdin.take();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait().expect("wait") {
            break status;
        }
        if std::time::Instant::now() > deadline {
            child.kill().expect("kill");
            child.wait().expect("reap");
            panic!("gitpulse-mcp {args:?} did not exit: it is serving a stdin nobody writes");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    drop(held_stdin);
    let mut stdout = String::new();
    let mut stderr = String::new();
    std::io::Read::read_to_string(&mut child.stdout.take().unwrap(), &mut stdout).unwrap();
    std::io::Read::read_to_string(&mut child.stderr.take().unwrap(), &mut stderr).unwrap();
    (status.code(), stdout, stderr)
}

/// A person who types `gitpulse-mcp --help` gets an answer and their prompt
/// back. It used to start serving and sit on the terminal's stdin, and every
/// such probe left a process behind.
#[test]
fn command_line_arguments_are_answered_without_serving() {
    for flag in ["--help", "-h"] {
        let (code, stdout, stderr) = run_with_args(&[flag]);
        assert_eq!(code, Some(0), "{flag}: {stderr}");
        assert!(stdout.starts_with("waylode-mcp"), "{flag}: {stdout}");
        assert!(stdout.contains("--version"), "{flag}: {stdout}");
    }
    for flag in ["--version", "-V"] {
        let (code, stdout, stderr) = run_with_args(&[flag]);
        assert_eq!(code, Some(0), "{flag}: {stderr}");
        // The first line is the version alone, as it always was; the second
        // names the one-shot commands, for mcp:doctor.
        assert_eq!(
            stdout,
            format!(
                "waylode-mcp {}\ncommands: tools, describe, call, read\n",
                env!("CARGO_PKG_VERSION")
            ),
            "{flag}"
        );
    }
    for args in [&["--stdio"][..], &["serve"], &["--help", "extra"]] {
        let (code, stdout, stderr) = run_with_args(args);
        assert_eq!(code, Some(2), "{args:?}: {stderr}");
        assert!(stdout.is_empty(), "{args:?} wrote to the wire: {stdout}");
        assert!(stderr.contains("unexpected argument"), "{args:?}: {stderr}");
    }
}

/// Runs a one-shot command the way an agent's shell does (no stdin) and
/// returns its exit code, stdout and stderr. Both pipes are drained while it
/// runs: `tools --json` prints far more than a pipe buffer holds, and a
/// reader that waited for exit first would deadlock against it.
fn run_one_shot(args: &[&str]) -> (Option<i32>, String, String) {
    run_one_shot_with_input(args, None)
}

/// [`run_one_shot`] with `input` written to stdin and then closed.
fn run_one_shot_with_input(args: &[&str], input: Option<&str>) -> (Option<i32>, String, String) {
    run_one_shot_with(args, input, false, &[])
}

/// [`run_one_shot_with_input`] with extra environment, and with stdin kept
/// open (never closed, as a pipe whose writer is still alive) until the
/// command has exited when `hold_stdin_open` is set.
fn run_one_shot_with(
    args: &[&str],
    input: Option<&str>,
    hold_stdin_open: bool,
    envs: &[(&str, &std::ffi::OsStr)],
) -> (Option<i32>, String, String) {
    let log_dir = tempfile::tempdir().expect("log dir");
    let mut command = Command::new(mcp_bin());
    command.envs(envs.iter().copied());
    let mut child = command
        .args(args)
        .stdin(if input.is_some() || hold_stdin_open {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("GITPULSE_LOG_DIR", log_dir.path())
        // Not the terminal this test runs in: a terminal tool acts for the
        // GitPulse terminal named here, and the tests must not depend on one.
        .env_remove(waylode_lib::terminal::TERMINAL_SESSION_ENV)
        .env_remove(waylode_lib::alerts::bridge::SESSION_ENV)
        .env_remove(waylode_lib::terminal::orchestrate::SESSION_TOKEN_ENV)
        .spawn_locked()
        .unwrap_or_else(|e| panic!("spawn {}: {e}", mcp_bin()));
    let drain = |mut pipe: Box<dyn std::io::Read + Send>| {
        std::thread::spawn(move || {
            let mut text = String::new();
            std::io::Read::read_to_string(&mut pipe, &mut text).expect("read pipe");
            text
        })
    };
    let mut held_stdin = None;
    if let Some(input) = input {
        let mut stdin = child.stdin.take().expect("stdin");
        stdin.write_all(input.as_bytes()).expect("write stdin");
    } else if hold_stdin_open {
        held_stdin = child.stdin.take();
    }
    let stdout = drain(Box::new(child.stdout.take().expect("stdout")));
    let stderr = drain(Box::new(child.stderr.take().expect("stderr")));
    let deadline = Instant::now() + Duration::from_secs(60);
    let status = loop {
        if let Some(status) = child.try_wait().expect("wait") {
            break status;
        }
        if Instant::now() > deadline {
            child.kill().expect("kill");
            child.wait().expect("reap");
            panic!("gitpulse-mcp {args:?} did not exit within 60 s");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    drop(held_stdin);
    (
        status.code(),
        stdout.join().expect("stdout reader"),
        stderr.join().expect("stderr reader"),
    )
}

/// A `--json -` whose writer never closes the pipe is a stall, bounded by
/// `--timeout-ms` like a tool that never answers: exit 4 and nothing run, not
/// a hang until the harness gives up (feat/mcp-cli review, finding 4).
#[test]
fn a_stdin_that_never_closes_exits_four_at_the_timeout() {
    let (code, stdout, stderr) = run_one_shot_with(
        &[
            "call",
            "gitpulse_status",
            "--json",
            "-",
            "--timeout-ms",
            "800",
        ],
        None,
        true,
        &[],
    );
    assert_eq!(code, Some(4), "stdout: {stdout}\nstderr: {stderr}");
    assert!(stdout.is_empty(), "{stdout}");
    assert!(stderr.contains("--json -"), "{stderr}");
}

/// A timed-out call takes the git it started down with it: a `git` that
/// hangs is recorded by pid, the call times out (exit 4), and that pid is
/// gone afterwards. By pid, never by name.
#[test]
#[cfg(unix)]
fn a_timed_out_call_leaves_no_git_child_behind() {
    use std::os::unix::fs::PermissionsExt;
    let repo = tempfile::tempdir().expect("repo");
    assert!(Command::new("git")
        .args(["init", "-q"])
        .arg(repo.path())
        .status_locked()
        .expect("git init")
        .success());
    // Persist the grant so the out-of-process `gitpulse-mcp` shares the
    // hermetic trust decision.
    let repo_str = repo.path().to_str().expect("utf-8 path");
    let preview = waylode_lib::repository_trust::inspect(repo_str).expect("inspect repo");
    waylode_lib::repository_trust::grant(repo_str, &preview.identity, true).expect("trust");
    let stub = tempfile::tempdir().expect("stub dir");
    let pids = stub.path().join("pids");
    let script = stub.path().join("git");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\necho $$ >> '{}'\nexec sleep 300\n",
            pids.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut path = std::ffi::OsString::from(stub.path());
    path.push(":");
    path.push(std::env::var_os("PATH").unwrap_or_default());

    // The timeout stays under the gate's 2 s wait for a spawn token: past
    // that, a loaded host defers a second git and the call ends with exit 3
    // (incomplete) before the watchdog's exit 4, which this test must see.
    let repo_arg = format!("repo_path={}", repo.path().to_str().expect("utf-8 path"));
    let (code, stdout, stderr) = run_one_shot_with(
        &["call", "gitpulse_status", &repo_arg, "--timeout-ms", "1200"],
        None,
        false,
        &[("PATH", path.as_os_str())],
    );
    assert_eq!(code, Some(4), "stdout: {stdout}\nstderr: {stderr}");
    let started: Vec<String> = std::fs::read_to_string(&pids)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect();
    assert!(
        !started.is_empty(),
        "the call never started the stub git, so this proves nothing: {stderr}"
    );
    for pid in started {
        let alive = Command::new("kill")
            .args(["-0", &pid])
            .stderr(Stdio::null())
            .status_locked()
            .expect("kill -0")
            .success();
        assert!(!alive, "git child {pid} outlived the timed-out call");
    }
}

/// An agent whose MCP connection is not loaded still has a shell: the same
/// binary answers one request from the command line and exits, with nothing
/// but the answer on stdout and an exit status that tells a partial answer
/// from a complete one.
#[test]
fn one_shot_commands_answer_without_a_session() {
    let (code, stdout, stderr) = run_one_shot(&["tools"]);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        stdout.lines().any(|l| l.starts_with("waylode_insights ")),
        "{stdout}"
    );

    let (code, stdout, stderr) = run_one_shot(&["tools", "--json"]);
    assert_eq!(code, Some(0), "{stderr}");
    let catalog: Value = serde_json::from_str(&stdout).expect("tools --json is JSON");
    assert!(
        catalog
            .as_array()
            .is_some_and(|tools| tools.iter().any(|t| t["name"] == "waylode_status")),
        "{stdout}"
    );

    let (code, stdout, stderr) = run_one_shot(&["describe", "waylode_status"]);
    assert_eq!(code, Some(0), "{stderr}");
    let tool: Value = serde_json::from_str(&stdout).expect("describe prints JSON");
    assert!(
        tool["inputSchema"]["properties"]["repo_path"].is_object(),
        "{tool}"
    );

    // A facet that could not scan is not a clean answer: exit 3, and the
    // partial answer is still the only thing on stdout.
    let (code, stdout, stderr) = run_one_shot(&[
        "call",
        "waylode_insights",
        "repo_path=/no/such/waylode-cli-repo",
        "--compact",
    ]);
    assert_eq!(code, Some(3), "stdout: {stdout}\nstderr: {stderr}");
    let payload: Value = serde_json::from_str(&stdout).expect("call prints JSON");
    assert_eq!(payload["worktrees"]["ok"], false, "{payload}");
    assert!(stderr.contains("worktrees"), "{stderr}");

    // A write needs --write, and is refused before anything runs.
    let (code, stdout, stderr) = run_one_shot(&[
        "call",
        "waylode_add_task",
        "repo_path=/no/such/waylode-cli-repo",
        "title=t",
    ]);
    assert_eq!(code, Some(2), "{stderr}");
    assert!(stdout.is_empty(), "{stdout}");
    assert!(stderr.contains("--write"), "{stderr}");

    // The server's own schema check refuses a missing argument: exit 1.
    let (code, stdout, stderr) = run_one_shot(&["call", "waylode_status"]);
    assert_eq!(code, Some(1), "{stdout} {stderr}");
    assert!(stdout.is_empty(), "{stdout}");
    assert!(stderr.contains("repo_path"), "{stderr}");

    // A resource answers from the same binary.
    let (code, stdout, stderr) = run_one_shot(&["read", "waylode://server/manifest", "--compact"]);
    assert_eq!(code, Some(0), "{stderr}");
    let manifest: Value = serde_json::from_str(&stdout).expect("read prints JSON");
    assert!(manifest.is_object(), "{manifest}");

    // An unknown tool, key, resource or deadline is the command line's
    // fault: exit 2.
    for args in [
        &["call", "gitpulse_nope"][..],
        &["call", "gitpulse_status", "repo_pth=/x"],
        &["describe", "gitpulse_nope"],
        &["read", "gitpulse://no-such-resource"],
        &[
            "call",
            "gitpulse_status",
            "repo_path=/x",
            "--timeout-ms",
            "0",
        ],
    ] {
        let (code, stdout, stderr) = run_one_shot(args);
        assert_eq!(code, Some(2), "{args:?}: {stderr}");
        assert!(stdout.is_empty(), "{args:?}: {stdout}");
    }
}

/// The command line is not a way around repository trust: a write the server
/// would refuse for an untrusted repository is refused here too, with the
/// same code, and the argument object arrives whole through stdin.
#[test]
fn a_one_shot_write_is_held_to_repository_trust() {
    let repo = tempfile::tempdir().expect("repo");
    assert!(Command::new("git")
        .args(["init", "-q"])
        .arg(repo.path())
        .status_locked()
        .expect("git init")
        .success());
    let brief = json!({
        "repo_path": repo.path().to_str().expect("utf-8 path"),
        "title": "one-shot trust probe"
    })
    .to_string();
    let (code, stdout, stderr) = run_one_shot_with_input(
        &["call", "gitpulse_add_task", "--write", "--json", "-"],
        Some(&brief),
    );
    assert_eq!(code, Some(1), "stdout: {stdout}\nstderr: {stderr}");
    assert!(stdout.is_empty(), "{stdout}");
    assert!(stderr.contains("untrusted_repository"), "{stderr}");
}

/// A read that could not look must not exit like one that looked and found
/// nothing. On an untrusted repository the DevMap tools answer
/// `available: false` (exit 3), and `gitpulse_provenance` is a tool error
/// (exit 1) rather than an all-null "unexamined" record under exit 0.
#[test]
fn a_one_shot_read_on_an_untrusted_repository_is_not_exit_zero() {
    let repo = tempfile::tempdir().expect("repo");
    assert!(Command::new("git")
        .args(["init", "-q"])
        .arg(repo.path())
        .status_locked()
        .expect("git init")
        .success());
    let path = format!("repo_path={}", repo.path().to_str().expect("utf-8 path"));

    let (code, stdout, stderr) = run_one_shot(&["call", "devmap_status", &path]);
    assert_eq!(code, Some(3), "stdout: {stdout}\nstderr: {stderr}");
    assert!(stdout.contains("REPOSITORY_TRUST_REQUIRED"), "{stdout}");
    assert!(stderr.contains("available"), "{stderr}");

    let (code, stdout, stderr) = run_one_shot(&[
        "call",
        "gitpulse_provenance",
        &path,
        "commit_sha=0000000000000000000000000000000000000000",
    ]);
    assert_eq!(code, Some(1), "stdout: {stdout}\nstderr: {stderr}");
    assert!(stdout.is_empty(), "{stdout}");
    assert!(stderr.contains("REPOSITORY_TRUST_REQUIRED"), "{stderr}");

    // `gitpulse_landed` refuses the same way: a tool error, not an answer
    // whose only sign of refusal is a top-level `error`.
    let (code, stdout, stderr) = run_one_shot(&["call", "gitpulse_landed", &path, "commit=HEAD"]);
    assert_eq!(code, Some(1), "stdout: {stdout}\nstderr: {stderr}");
    assert!(stdout.is_empty(), "{stdout}");
    assert!(stderr.contains("REPOSITORY_TRUST_REQUIRED"), "{stderr}");
}

// ---------------------------------------------------------------------------
// Adversarial wire tests for typed tool arguments.
// ---------------------------------------------------------------------------

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

/// A live server on a hermetic home, spoken to a few requests at a time (the
/// server refuses past 32 in flight) and read until each id has answered.
struct Session {
    child: std::process::Child,
    stdin: Option<std::process::ChildStdin>,
    lines: Receiver<Value>,
    _home: tempfile::TempDir,
    unmatched: Vec<Value>,
}

impl Session {
    fn open() -> Self {
        let home = tempfile::tempdir().expect("home");
        let mut child = Command::new(mcp_bin())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .env("HOME", home.path())
            .env("GITPULSE_LOG_DIR", home.path())
            .spawn_locked()
            .unwrap_or_else(|e| panic!("spawn {}: {e}", mcp_bin()));
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().expect("stdout");
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for raw in BufReader::new(stdout).lines() {
                let Ok(raw) = raw else { break };
                if raw.trim().is_empty() {
                    continue;
                }
                let parsed = serde_json::from_str::<Value>(&raw)
                    .unwrap_or_else(|e| panic!("stdout is not JSON-RPC ({e}): {raw:.200}"));
                if tx.send(parsed).is_err() {
                    break;
                }
            }
        });
        Session {
            child,
            stdin,
            lines,
            _home: home,
            unmatched: Vec::new(),
        }
    }

    fn send_raw(&mut self, frame: &str) {
        let stdin = self.stdin.as_mut().expect("stdin open");
        stdin.write_all(frame.as_bytes()).expect("write");
        stdin.write_all(b"\n").expect("newline");
        stdin.flush().expect("flush");
    }

    /// Answers by id; responses with another id (a parse error answers `null`)
    /// are kept in `unmatched`.
    fn ask(&mut self, frames: Vec<(u64, String)>, within: Duration) -> HashMap<u64, Value> {
        let mut wanted: Vec<u64> = frames.iter().map(|(id, _)| *id).collect();
        for (_, frame) in &frames {
            self.send_raw(frame);
        }
        let deadline = Instant::now() + within;
        let mut got = HashMap::new();
        while !wanted.is_empty() {
            let left = deadline.saturating_duration_since(Instant::now());
            let Ok(reply) = self.lines.recv_timeout(left) else {
                panic!("no answer for ids {wanted:?} within {within:?}");
            };
            match reply["id"].as_u64() {
                Some(id) if wanted.contains(&id) => {
                    wanted.retain(|w| *w != id);
                    got.insert(id, reply);
                }
                _ => self.unmatched.push(reply),
            }
        }
        got
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        drop(self.stdin.take());
        let _ = self.child.wait();
    }
}

fn call_frame(id: u64, name: &str, arguments_text: &str) -> (u64, String) {
    (
        id,
        format!(
            r#"{{"jsonrpc":"2.0","id":{id},"method":"tools/call","params":{{"_meta":{},"name":{},"arguments":{arguments_text}}}}}"#,
            modern_meta(),
            json!(name)
        ),
    )
}

fn text_of(reply: &Value) -> String {
    reply["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

fn refused(reply: &Value) -> bool {
    reply["result"]["isError"] == true && text_of(reply).starts_with("invalid arguments for")
}

fn advertised_tools(session: &mut Session) -> Vec<Value> {
    let reply = session
        .ask(
            vec![(9000, line(9000, "tools/list", json!({})))],
            Duration::from_secs(20),
        )
        .remove(&9000)
        .expect("tools/list");
    reply["result"]["tools"].as_array().expect("tools").clone()
}

const WAIT: Duration = Duration::from_secs(30);

/// Arguments that satisfy `required` for `schema`, with `field` left out.
fn required_except(schema: &Value, field: &str) -> serde_json::Map<String, Value> {
    let mut arguments = serde_json::Map::new();
    for key in schema["required"].as_array().unwrap() {
        let key = key.as_str().unwrap();
        if key == field {
            continue;
        }
        let value = match key {
            "repo_path" => json!("/no/such/gitpulse-stdio-repo"),
            _ => match schema["properties"][key]["type"].as_str() {
                Some("integer") => schema["properties"][key]["minimum"].clone(),
                Some("array") if schema["properties"][key]["minItems"].is_null() => json!([]),
                Some("array") if schema["properties"][key]["items"]["type"] == "object" => {
                    json!([{ "task_id": "x" }])
                }
                Some("array") => json!(["x"]),
                _ => json!("x"),
            },
        };
        arguments.insert(key.to_string(), value);
    }
    arguments
}

#[test]
fn tool_arguments_that_are_not_an_object_are_protocol_errors_and_the_server_keeps_serving() {
    let mut session = Session::open();
    let meta = modern_meta();
    let shapes = [
        ("null", r#""arguments":null,"#),
        ("array", r#""arguments":[],"#),
        ("string", r#""arguments":"repo_path","#),
        ("number", r#""arguments":5,"#),
        ("bool", r#""arguments":true,"#),
    ];
    let mut frames = Vec::new();
    for (index, (_, member)) in shapes.iter().enumerate() {
        let id = 10 + index as u64;
        frames.push((
            id,
            format!(
                r#"{{"jsonrpc":"2.0","id":{id},"method":"tools/call","params":{{"_meta":{meta},{member}"name":"gitpulse_status"}}}}"#
            ),
        ));
    }
    let replies = session.ask(frames, WAIT);
    for (index, (what, _)) in shapes.iter().enumerate() {
        let reply = &replies[&(10 + index as u64)];
        assert_eq!(reply["error"]["code"], -32602, "{what}: {reply}");
        assert!(
            reply["error"]["message"]
                .as_str()
                .unwrap_or_default()
                .contains("arguments must be an object"),
            "{what}: {reply}"
        );
    }
    // Missing `arguments` is an empty object, so the missing repo_path is named.
    let missing = session.ask(
        vec![(
            20,
            format!(
                r#"{{"jsonrpc":"2.0","id":20,"method":"tools/call","params":{{"_meta":{meta},"name":"gitpulse_status"}}}}"#
            ),
        )],
        WAIT,
    );
    assert!(refused(&missing[&20]), "{}", missing[&20]);
    assert!(text_of(&missing[&20]).contains("repo_path"));
    assert!(!advertised_tools(&mut session).is_empty());
}

#[test]
fn a_frame_over_the_cap_is_answered_and_the_connection_continues() {
    let mut session = Session::open();
    let giant = format!(r#"{{"repo_path":"{}"}}"#, "a".repeat(4 * 1024 * 1024));
    let (_, frame) = call_frame(30, "gitpulse_status", &giant);
    session.send_raw(&frame);
    // Just under the cap: refused for its length, in a short message.
    let nearly = format!(r#"{{"repo_path":"{}"}}"#, "a".repeat(3_900_000));
    let replies = session.ask(vec![call_frame(31, "gitpulse_status", &nearly)], WAIT);
    let text = text_of(&replies[&31]);
    assert!(refused(&replies[&31]), "{text:.200}");
    assert!(
        text.contains("repo_path") && text.len() < 2048,
        "{} bytes",
        text.len()
    );
    assert!(!advertised_tools(&mut session).is_empty());
    assert!(
        session
            .unmatched
            .iter()
            .any(|r| r["error"]["code"] == -32700),
        "an over-cap frame was not answered: {:?}",
        session.unmatched
    );
}

#[test]
fn frames_json_cannot_carry_are_parse_errors_not_a_dead_connection() {
    let mut session = Session::open();
    let meta = modern_meta();
    for (index, bad) in [
        r#""repo_path":"\ud800""#,
        r#""repo_path":"\udc00x""#,
        r#""repo_path":1e400"#,
        r#""repo_path":-1e400"#,
        r#""repo_path":NaN"#,
        r#""repo_path":Infinity"#,
        r#""repo_path":0x10"#,
    ]
    .iter()
    .enumerate()
    {
        session.send_raw(&format!(
            r#"{{"jsonrpc":"2.0","id":{},"method":"tools/call","params":{{"_meta":{meta},"name":"gitpulse_status","arguments":{{{bad}}}}}}}"#,
            40 + index
        ));
    }
    // The connection is alive, and none of that reached a tool.
    assert!(!advertised_tools(&mut session).is_empty());
    let answered: Vec<&Value> = session
        .unmatched
        .iter()
        .filter(|r| r["result"]["isError"] == false)
        .collect();
    assert!(
        answered.is_empty(),
        "a malformed frame ran a tool: {answered:?}"
    );
}

#[test]
fn a_duplicate_key_is_decided_by_the_last_and_that_one_is_validated() {
    let mut session = Session::open();
    let replies = session.ask(
        vec![
            call_frame(
                50,
                "gitpulse_status",
                r#"{"repo_path":"/no/such/gitpulse-stdio-repo","repo_path":5}"#,
            ),
            call_frame(
                51,
                "gitpulse_status",
                r#"{"repo_path":5,"repo_path":"/no/such/gitpulse-stdio-repo"}"#,
            ),
        ],
        WAIT,
    );
    assert!(refused(&replies[&50]), "{}", replies[&50]);
    assert!(!refused(&replies[&51]), "{}", replies[&51]);
}

#[test]
fn a_refusal_with_a_hundred_thousand_violations_still_gets_an_answer() {
    let mut session = Session::open();
    let mut arguments = String::from(r#"{"repo_path":"/no/such/gitpulse-stdio-repo""#);
    for index in 0..150_000 {
        arguments.push_str(&format!(r#","k{index}":1"#));
    }
    arguments.push('}');
    let replies = session.ask(
        vec![call_frame(60, "gitpulse_status", &arguments)],
        Duration::from_secs(20),
    );
    let reply = &replies[&60];
    assert!(refused(reply), "{:.300}", reply.to_string());
    assert!(
        text_of(reply).contains("k0"),
        "the refusal names no argument"
    );
    assert!(
        reply.to_string().len() < 1_000_000,
        "the refusal is {} bytes",
        reply.to_string().len()
    );
}

/// Every integer argument of every advertised tool, at its edges, as written
/// by a client: an integer literal and a whole float.
#[test]
fn every_integer_argument_is_held_to_its_advertised_bounds_on_the_wire() {
    let mut session = Session::open();
    let tools = advertised_tools(&mut session);
    let mut id = 1000u64;
    let mut checked = 0;
    for tool in &tools {
        let name = tool["name"].as_str().unwrap();
        let schema = &tool["inputSchema"];
        for (field, prop) in schema["properties"].as_object().unwrap() {
            if prop["type"] != "integer" {
                continue;
            }
            let min = i128::from(prop["minimum"].as_i64().expect("minimum"));
            let max = i128::from(prop["maximum"].as_u64().expect("maximum"));
            let mut batch = Vec::new();
            let mut expectations = HashMap::new();
            for value in [min - 1, min, max, max + 1] {
                // The text goes on the wire as written. A float spelling of a
                // value past 10^15 - 1 is one the server refuses rather than misread.
                let spellings = [
                    (format!("{value}"), true),
                    (format!("{value}.0"), value.abs() <= 999_999_999_999_999),
                ];
                for (text, representable) in spellings {
                    let inside = (min..=max).contains(&value) && representable;
                    id += 1;
                    let mut arguments = format!(r#"{{{}:{text}"#, json!(field));
                    for (key, value) in required_except(schema, field) {
                        arguments.push_str(&format!(",{}:{}", json!(key), value));
                    }
                    arguments.push('}');
                    batch.push(call_frame(id, name, &arguments));
                    expectations.insert(id, (inside, format!("{name}.{field} = {text}")));
                }
            }
            let replies = session.ask(batch, WAIT);
            for (id, (inside, shown)) in expectations {
                let reply = &replies[&id];
                if inside {
                    assert!(
                        !refused(reply),
                        "{shown} was refused: {:.300}",
                        text_of(reply)
                    );
                } else {
                    assert!(
                        refused(reply),
                        "{shown} was accepted: {:.200}",
                        reply.to_string()
                    );
                    assert!(text_of(reply).contains(field.as_str()), "{shown}");
                }
                checked += 1;
            }
        }
    }
    assert!(checked > 100, "only {checked} wire probes ran");
}

/// Every string argument of every tool at its limit and one over, in ASCII, so
/// the bound a client reads is the bound the binary enforces.
#[test]
fn every_string_argument_is_held_to_its_advertised_limit_on_the_wire() {
    let mut session = Session::open();
    let tools = advertised_tools(&mut session);
    let mut id = 5000u64;
    let mut checked = 0;
    for tool in &tools {
        let name = tool["name"].as_str().unwrap();
        let schema = &tool["inputSchema"];
        for (field, prop) in schema["properties"].as_object().unwrap() {
            if prop["type"] != "string" || !prop["enum"].is_null() {
                continue;
            }
            let max = prop["maxLength"].as_u64().expect("maxLength") as usize;
            if max > 1_100_000 || field == "repo_path" {
                continue;
            }
            let mut batch = Vec::new();
            let mut over_ids = Vec::new();
            for (length, over) in [(max, false), (max + 1, true)] {
                id += 1;
                let mut arguments = required_except(schema, field);
                arguments.insert(field.clone(), json!("a".repeat(length)));
                batch.push(call_frame(id, name, &Value::Object(arguments).to_string()));
                if over {
                    over_ids.push(id);
                }
            }
            let replies = session.ask(batch, WAIT);
            for (id, reply) in &replies {
                if over_ids.contains(id) {
                    assert!(refused(reply), "{name}.{field}: {max}+1 chars accepted");
                    assert!(text_of(reply).contains(field.as_str()), "{name}.{field}");
                } else {
                    assert!(
                        !refused(reply),
                        "{name}.{field}: {max} chars refused: {:.200}",
                        text_of(reply)
                    );
                }
                checked += 1;
            }
        }
    }
    assert!(checked > 60, "only {checked} wire probes ran");
}

/// Was broken on 8026dd7e (fixed here): `devmap_parity::handle` answered
/// an unknown `devmap_*` name in-band with "Unknown DevMap tool", which `run` did not
/// recognise as the protocol error it maps "Unknown tool:" to.
#[test]
fn an_unknown_tool_is_a_protocol_error_whatever_its_prefix() {
    let mut session = Session::open();
    let replies = session.ask(
        vec![
            call_frame(70, "devmap_no_such_tool", r#"{"anything":[1,2,3]}"#),
            call_frame(71, "gitpulse_no_such_tool", r#"{"anything":[1,2,3]}"#),
        ],
        WAIT,
    );
    assert_eq!(replies[&71]["error"]["code"], -32602, "{}", replies[&71]);
    assert_eq!(replies[&70]["error"]["code"], -32602, "{}", replies[&70]);
}

/// Long multibyte argument names, some with control characters that JSON
/// escaping inflates: the refusal is cut at its byte bound, and the reply still
/// arrives and is bounded.
#[test]
fn a_refusal_over_the_byte_bound_still_gets_a_bounded_answer() {
    let mut session = Session::open();
    let mut arguments = String::from(r#"{"repo_path":"/no/such/gitpulse-stdio-repo""#);
    for index in 0..40 {
        let control = if index % 2 == 0 { "\u{1}\u{7}\n\t" } else { "" };
        let key = format!("{}{control}{index}", "\u{20ac}".repeat(1000));
        arguments.push_str(&format!(",{}:1", json!(key)));
    }
    arguments.push('}');
    let replies = session.ask(
        vec![call_frame(80, "gitpulse_status", &arguments)],
        Duration::from_secs(20),
    );
    let reply = &replies[&80];
    assert!(refused(reply), "{:.300}", reply.to_string());
    let text = text_of(reply);
    assert!(
        text.len() <= 8 * 1024,
        "the refusal text is {} bytes",
        text.len()
    );
    assert!(text.ends_with('\u{2026}'), "the cut is marked");
    // Escaping can inflate a control character six-fold, never more.
    assert!(
        reply.to_string().len() < 6 * 8 * 1024 + 4096,
        "the reply is {} bytes",
        reply.to_string().len()
    );
}
