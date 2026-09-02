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
                "name": "gitpulse_codeintel_dependencies",
                "description": "What a file depends on — the mirror of impact. Answers outgoing edges from the devmap code graph",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "repo_path": { "type": "string", "description": "Absolute path to git repository" },
                        "file_path": { "type": "string", "description": "Repo-relative file whose dependencies to read" },
                        "budget": { "type": "number", "description": "Maximum tokens of results" }
                    },
                    "required": ["repo_path", "file_path"]
                }
            },
            {
                "name": "gitpulse_codeintel_trace",
                "description": "Shortest edge path between two symbols in the devmap code graph",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "repo_path": { "type": "string", "description": "Absolute path to git repository" },
                        "from": { "type": "string", "description": "Symbol to trace from" },
                        "to": { "type": "string", "description": "Symbol to trace to" },
                        "budget": { "type": "number", "description": "Maximum tokens of results" }
                    },
                    "required": ["repo_path", "from", "to"]
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
        "gitpulse_codeintel_dependencies" => {
            let repo = arguments["repo_path"].as_str().ok_or("missing repo_path")?;
            let file = arguments["file_path"].as_str().ok_or("missing file_path")?;
            let budget = arguments["budget"].as_u64().map(|b| b as u32);
            let res = gitpulse_lib::codeintel::dependencies(repo, file, budget);
            Ok(json!(res))
        }
        "gitpulse_codeintel_trace" => {
            let repo = arguments["repo_path"].as_str().ok_or("missing repo_path")?;
            let from = arguments["from"].as_str().ok_or("missing from")?;
            let to = arguments["to"].as_str().ok_or("missing to")?;
            let budget = arguments["budget"].as_u64().map(|b| b as u32);
            let res = gitpulse_lib::codeintel::trace_between(repo, from, to, budget);
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

/// Answers one request, or `None` when the message was a notification.
///
/// JSON-RPC distinguishes the two by the presence of `id`, and a notification
/// must never be answered. MCP clients send `notifications/initialized`
/// immediately after the handshake, so a server that replies to it puts an
/// unmatched response on the wire before the first tool call — which a strict
/// client reads as a response to a request it never made.
fn process_request(req: JsonRpcRequest) -> Option<JsonRpcResponse> {
    let Some(id) = req.id.clone() else {
        // A notification. Nothing to answer, and nothing to log to stdout —
        // that channel carries protocol and nothing else.
        return None;
    };
    Some(match req.method.as_str() {
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
    })
}

fn main() {
    // stdout is the JSON-RPC channel and carries nothing else; the logger
    // writes to stderr and to its own file, so installing it here cannot
    // corrupt a response. A panic used to leave the client with a closed pipe
    // and no explanation on either end.
    gitpulse_lib::logging::init();
    gitpulse_lib::logging::install_panic_hook();
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

        // A notification produces nothing. Writing an empty line, or a
        // response with a null id, would both be protocol on a channel that
        // carries only protocol.
        if let Some(resp) = process_request(req) {
            let out = serde_json::to_string(&resp).unwrap();
            let _ = writeln!(stdout, "{out}");
            let _ = stdout.flush();
        }
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

    /// Advertised tools and dispatch arms are the same set.
    ///
    /// The two halves are written 100 lines apart, and drift either way is
    /// silent: a tool advertised with no arm answers "unknown tool" to a
    /// client that was told it existed, and an arm nobody advertises is code
    /// no client will ever reach. Derived from this file's own source rather
    /// than from a list kept here, so a tool added to one half and not the
    /// other fails without anyone remembering to update a fixture.
    #[test]
    fn every_advertised_tool_has_a_dispatch_arm_and_the_reverse() {
        let source = include_str!("gitpulse-mcp.rs");

        let advertised: std::collections::BTreeSet<String> = handle_tools_list()["tools"]
            .as_array()
            .expect("array")
            .iter()
            .map(|t| t["name"].as_str().expect("name").to_string())
            .collect();

        // Dispatch arms look like `        "gitpulse_x" => {` at one indent
        // level inside the match; the advertisement above uses `"name":`, so
        // the two cannot be confused.
        let dispatched: std::collections::BTreeSet<String> = source
            .lines()
            .filter_map(|line| {
                let t = line.trim();
                let rest = t.strip_prefix('"')?;
                let (name, tail) = rest.split_once('"')?;
                if !tail.trim_start().starts_with("=>") || !name.starts_with("gitpulse_") {
                    return None;
                }
                Some(name.to_string())
            })
            .collect();

        assert!(
            advertised.len() >= 6,
            "the scan found only {} advertised tools, which means it stopped working",
            advertised.len()
        );
        assert_eq!(
            advertised, dispatched,
            "advertised tools and dispatch arms have drifted"
        );
    }

    #[test]
    fn initialize_response_conforms_to_spec() {
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(1)),
            method: "initialize".into(),
            params: json!({}),
        };
        let resp = process_request(req).expect("a request with an id is answered");
        assert!(resp.error.is_none());
        assert_eq!(resp.result.unwrap()["serverInfo"]["name"], "gitpulse-mcp");
    }
}

#[cfg(test)]
mod protocol_tests {
    use super::*;

    fn request(method: &str, id: Option<Value>, params: Value) -> JsonRpcRequest {
        JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id,
            method: method.into(),
            params,
        }
    }

    /// A notification is never answered.
    ///
    /// MCP clients send `notifications/initialized` straight after the
    /// handshake. A server that replies puts an unmatched response on the wire
    /// before the first tool call, which a strict client reads as an answer to
    /// a request it never sent.
    #[test]
    fn a_notification_gets_no_response() {
        assert!(
            process_request(request("notifications/initialized", None, json!({}))).is_none(),
            "a message with no id was answered"
        );
        assert!(
            process_request(request("notifications/cancelled", None, json!({}))).is_none(),
            "every id-less message is a notification, not only the ones we know"
        );
    }

    /// ...and a request always is, including one this build does not implement.
    #[test]
    fn every_request_with_an_id_is_answered() {
        for method in ["initialize", "tools/list", "tools/call", "no/such/method"] {
            let resp = process_request(request(method, Some(json!(7)), json!({})));
            let resp = resp.unwrap_or_else(|| panic!("{method} went unanswered"));
            assert_eq!(resp.id, json!(7), "{method} answered the wrong id");
            assert!(
                resp.result.is_some() || resp.error.is_some(),
                "{method} answered with neither a result nor an error"
            );
        }
    }

    #[test]
    fn an_unknown_method_is_an_error_not_a_silent_success() {
        let resp = process_request(request("no/such/method", Some(json!(1)), json!({})))
            .expect("answered");
        assert!(resp.result.is_none());
        assert_eq!(resp.error.expect("error")["code"], -32601);
    }

    #[test]
    fn an_unknown_tool_is_refused_rather_than_answered_emptily() {
        // A tool call that silently returned nothing would read to an agent as
        // "the repository has nothing to report", which is a different claim.
        let resp = process_request(request(
            "tools/call",
            Some(json!(2)),
            json!({ "name": "gitpulse_not_a_tool", "arguments": {} }),
        ))
        .expect("answered");
        assert!(resp.result.is_none(), "an unknown tool returned a result");
        assert!(resp.error.is_some());
    }

    #[test]
    fn every_advertised_tool_declares_a_schema_and_its_required_arguments() {
        // A tool an agent cannot call correctly is worse than one that is
        // absent: the agent tries, fails, and cannot tell why.
        let tools = handle_tools_list();
        let list = tools["tools"].as_array().expect("array");
        assert!(!list.is_empty());
        for tool in list {
            let name = tool["name"].as_str().expect("every tool is named");
            assert!(
                tool["description"].as_str().is_some_and(|d| d.len() > 20),
                "{name} has no useful description"
            );
            let schema = &tool["inputSchema"];
            assert_eq!(schema["type"], "object", "{name} has no object schema");
            assert!(
                schema["properties"].is_object(),
                "{name} declares no properties"
            );
            let required = schema["required"].as_array().expect("required list");
            for field in required {
                let field = field.as_str().expect("required names a field");
                assert!(
                    schema["properties"][field].is_object(),
                    "{name} requires {field}, which its schema does not describe"
                );
            }
        }
    }

    #[test]
    fn tool_names_are_unique() {
        let tools = handle_tools_list();
        let list = tools["tools"].as_array().expect("array");
        let mut seen = std::collections::BTreeSet::new();
        for tool in list {
            let name = tool["name"].as_str().expect("named");
            assert!(seen.insert(name), "two tools are called {name}");
        }
    }

    /// This build's surface is read-only, and that is pinned rather than
    /// assumed.
    ///
    /// Not a permanent prohibition: the migration plan anticipates
    /// `checkpoint_workspace`, and a mutation here is legitimate *provided it
    /// goes through `harness::guard_command`* like every other write. What this
    /// test prevents is one arriving that does not — an agent mutating through
    /// MCP while the ledger and the gate know nothing about it. Adding a
    /// mutating tool means changing this test deliberately, with the routing
    /// in place.
    #[test]
    fn no_advertised_tool_offers_an_ungated_mutation() {
        let tools = handle_tools_list();
        for tool in tools["tools"].as_array().expect("array") {
            let name = tool["name"].as_str().expect("named");
            for verb in [
                "write", "commit", "push", "checkout", "apply", "delete", "revert", "ingest",
                "bind", "revoke",
            ] {
                assert!(
                    !name.contains(verb),
                    "{name} looks like a mutation. This surface is read-only in this \
                     build; a mutating tool must route through harness::guard_command \
                     so the ledger and the gate see it, and must update this test"
                );
            }
        }
    }
}
