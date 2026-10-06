//! Agents attach to `gitpulse-mcp` over newline-delimited JSON-RPC on stdio.
//!
//! Library unit tests call `mcp::process_request` in-process. That cannot
//! prove the binary's stdout is a clean JSON-RPC channel: `logging::init`
//! writes stderr and a log file, and a stray print on stdout would make every
//! MCP client fail to parse the first response. This test launches the real
//! binary twice and reads the wire.

use gitpulse_lib::procguard::LockedSpawn;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

fn mcp_bin() -> &'static str {
    env!("CARGO_BIN_EXE_gitpulse-mcp")
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
            names.contains(&"gitpulse_insights"),
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
        assert!(stdout.starts_with("gitpulse-mcp"), "{flag}: {stdout}");
        assert!(stdout.contains("--version"), "{flag}: {stdout}");
    }
    for flag in ["--version", "-V"] {
        let (code, stdout, stderr) = run_with_args(&[flag]);
        assert_eq!(code, Some(0), "{flag}: {stderr}");
        assert_eq!(
            stdout,
            format!("gitpulse-mcp {}\n", env!("CARGO_PKG_VERSION")),
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
