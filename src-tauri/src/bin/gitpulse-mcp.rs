//! GitPulse MCP Server.
//!
//! JSON-RPC over stdio. Protocol and tools live in `gitpulse_lib::mcp` so the
//! desktop app and this binary cannot advertise different catalogs.

use gitpulse_lib::mcp::{self, Era, JsonRpcRequest, JsonRpcResponse};
use serde_json::{json, Value};
use std::io::{self, BufRead, Write};

fn main() {
    gitpulse_lib::logging::init();
    gitpulse_lib::logging::install_panic_hook();
    // Same descriptor headroom the GUI takes: this server answers agent
    // traffic through the same git engine, and a launchd-inherited limit
    // of 256 fails its spawns exactly the same way.
    log::info!(target: "setup", "{}", gitpulse_lib::limits::raise_open_file_limit().describe());
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    let mut era = Era::Unknown;

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let req: JsonRpcRequest = match serde_json::from_str(trimmed) {
            Ok(r) => r,
            Err(e) => {
                let resp = JsonRpcResponse {
                    jsonrpc: "2.0",
                    id: Value::Null,
                    result: None,
                    error: Some(json!({ "code": -32700, "message": format!("Parse error: {e}") })),
                };
                let out = serde_json::to_string(&resp).unwrap();
                let _ = writeln!(stdout, "{out}");
                let _ = stdout.flush();
                continue;
            }
        };

        if let Some(resp) = mcp::process_request(req, &mut era) {
            let out = serde_json::to_string(&resp).unwrap();
            let _ = writeln!(stdout, "{out}");
            let _ = stdout.flush();
        }
    }
}
