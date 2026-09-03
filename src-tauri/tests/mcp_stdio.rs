//! Agents attach to `gitpulse-mcp` over newline-delimited JSON-RPC on stdio.
//!
//! Library unit tests call `mcp::process_request` in-process. That cannot
//! prove the binary's stdout is a clean JSON-RPC channel: `logging::init`
//! writes stderr and a log file, and a stray print on stdout would make every
//! MCP client fail to parse the first response. This test launches the real
//! binary twice and reads the wire.

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
        .spawn()
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
