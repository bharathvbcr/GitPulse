//! GitPulse MCP Server.
//!
//! Exposes GitPulse control plane (ledger, tasks, code graph, provenance, git reader)
//! as an MCP tool provider on stdio.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{self, BufRead, Write};

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Serialize)]
#[allow(dead_code)]
struct JsonRpcResponse {
    pub jsonrpc: &'static str,
    pub id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<Value>,
}

fn handle_tools_list() -> Value {
    json!({
        "tools": [
            {
                "name": "gitpulse_status",
                "description": "Inspect repository status, worktrees, ledger, and code graph availability",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "repo_path": { "type": "string", "description": "Absolute path to git repository" }
                    },
                    "required": ["repo_path"]
                }
            },
            {
                "name": "gitpulse_ledger_events",
                "description": "Read durable SQLite ledger event history (actor, tool, verdict, changes)",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "repo_path": { "type": "string", "description": "Absolute path to git repository" },
                        "limit": { "type": "number", "description": "Maximum number of events (default 50)" }
                    },
                    "required": ["repo_path"]
                }
            },
            {
                "name": "gitpulse_task_view",
                "description": "Read task details, leases, and worktree binding from dc-store",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "repo_path": { "type": "string", "description": "Absolute path to git repository" },
                        "task_id": { "type": "string", "description": "Task identifier" }
                    },
                    "required": ["repo_path", "task_id"]
                }
            },
            {
                "name": "gitpulse_codeintel_search",
                "description": "Search symbols across indexed code files via in-process devmap",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "repo_path": { "type": "string", "description": "Absolute path to git repository" },
                        "query": { "type": "string", "description": "Symbol name or prefix" },
                        "budget": { "type": "number", "description": "Token budget" }
                    },
                    "required": ["repo_path", "query"]
                }
            },
            {
                "name": "gitpulse_codeintel_impact",
                "description": "Compute downstream blast radius / affected callers for a symbol or file",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "repo_path": { "type": "string", "description": "Absolute path to git repository" },
                        "target": { "type": "string", "description": "Symbol or file path" },
                        "budget": { "type": "number", "description": "Token budget" }
                    },
                    "required": ["repo_path", "target"]
                }
            },
            {
                "name": "gitpulse_provenance",
                "description": "Read Git-native verification notes and confidence decay for a commit",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "repo_path": { "type": "string", "description": "Absolute path to git repository" },
                        "commit_sha": { "type": "string", "description": "Commit SHA" },
                        "base_branch": { "type": "string", "description": "Base branch to compute distance against (default HEAD)" }
                    },
                    "required": ["repo_path", "commit_sha"]
                }
            }
        ]
    })
}

fn handle_tool_call(name: &str, arguments: &Value) -> Result<Value, String> {
    match name {
        "gitpulse_status" => {
            let repo = arguments["repo_path"].as_str().ok_or("missing repo_path")?;
            let ledger_status = gitpulse_lib::ledger::status(repo);
            let codeintel_status = gitpulse_lib::codeintel::status(repo);
            Ok(json!({
                "repo_path": repo,
                "ledger": ledger_status,
                "codeintel": codeintel_status,
            }))
        }
        "gitpulse_ledger_events" => {
            let repo = arguments["repo_path"].as_str().ok_or("missing repo_path")?;
            let limit = arguments["limit"].as_u64().unwrap_or(50) as u32;
            let events = gitpulse_lib::ledger::tail(repo, 0, limit).map_err(|e| e.to_string())?;
            Ok(json!({
                "events": events,
                "total": events.len()
            }))
        }
        "gitpulse_task_view" => {
            let repo = arguments["repo_path"].as_str().ok_or("missing repo_path")?;
            let view = gitpulse_lib::tasks::view(repo);
            Ok(json!(view))
        }
        "gitpulse_codeintel_search" => {
            let repo = arguments["repo_path"].as_str().ok_or("missing repo_path")?;
            let query = arguments["query"].as_str().ok_or("missing query")?;
            let budget = arguments["budget"].as_u64().map(|b| b as u32);
            let res = gitpulse_lib::codeintel::search(repo, query, budget);
            Ok(json!(res))
        }
        "gitpulse_codeintel_impact" => {
            let repo = arguments["repo_path"].as_str().ok_or("missing repo_path")?;
            let target = arguments["target"].as_str().ok_or("missing target")?;
            let budget = arguments["budget"].as_u64().map(|b| b as u32);
            let res = gitpulse_lib::codeintel::impact(repo, target, budget);
            Ok(json!(res))
        }
        "gitpulse_provenance" => {
            let repo = arguments["repo_path"].as_str().ok_or("missing repo_path")?;
            let commit_sha = arguments["commit_sha"]
                .as_str()
                .ok_or("missing commit_sha")?;
            let base = arguments["base_branch"].as_str();
            let freshness =
                gitpulse_lib::engine::provenance::compute_freshness(repo, commit_sha, base);
            Ok(json!(freshness))
        }
        _ => Err(format!("Unknown tool: {name}")),
    }
}

fn process_request(req: JsonRpcRequest) -> JsonRpcResponse {
    let id = req.id.clone().unwrap_or(Value::Null);
    match req.method.as_str() {
        "initialize" => JsonRpcResponse {
            jsonrpc: "2.0",
            id,
            result: Some(json!({
                "protocolVersion": "2024-11-05",
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "gitpulse-mcp", "version": "0.0.3" }
            })),
            error: None,
        },
        "notifications/initialized" => JsonRpcResponse {
            jsonrpc: "2.0",
            id,
            result: Some(json!({})),
            error: None,
        },
        "tools/list" => JsonRpcResponse {
            jsonrpc: "2.0",
            id,
            result: Some(handle_tools_list()),
            error: None,
        },
        "tools/call" => {
            let name = req.params["name"].as_str().unwrap_or("");
            let arguments = &req.params["arguments"];
            match handle_tool_call(name, arguments) {
                Ok(content) => JsonRpcResponse {
                    jsonrpc: "2.0",
                    id,
                    result: Some(json!({
                        "content": [
                            {
                                "type": "text",
                                "text": serde_json::to_string_pretty(&content).unwrap_or_default()
                            }
                        ]
                    })),
                    error: None,
                },
                Err(err) => JsonRpcResponse {
                    jsonrpc: "2.0",
                    id,
                    result: None,
                    error: Some(json!({
                        "code": -32603,
                        "message": err
                    })),
                },
            }
        }
        _ => JsonRpcResponse {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(json!({
                "code": -32601,
                "message": format!("Method not found: {}", req.method)
            })),
        },
    }
}

fn main() {
    let stdin = io::stdin();
    let mut stdout = io::stdout();

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

        let resp = process_request(req);
        let out = serde_json::to_string(&resp).unwrap();
        let _ = writeln!(stdout, "{out}");
        let _ = stdout.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tools_list_contains_expected_tools() {
        let tools = handle_tools_list();
        let list = tools["tools"].as_array().expect("array");
        assert!(list.iter().any(|t| t["name"] == "gitpulse_status"));
        assert!(list.iter().any(|t| t["name"] == "gitpulse_ledger_events"));
        assert!(list
            .iter()
            .any(|t| t["name"] == "gitpulse_codeintel_search"));
        assert!(list.iter().any(|t| t["name"] == "gitpulse_provenance"));
    }

    #[test]
    fn initialize_response_conforms_to_spec() {
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(1)),
            method: "initialize".into(),
            params: json!({}),
        };
        let resp = process_request(req);
        assert!(resp.error.is_none());
        assert_eq!(resp.result.unwrap()["serverInfo"]["name"], "gitpulse-mcp");
    }
}
