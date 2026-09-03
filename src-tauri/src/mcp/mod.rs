//! MCP protocol surface for `gitpulse-mcp`.
//!
//! Speaks [MCP 2026-07-28](https://modelcontextprotocol.io/specification/2026-07-28)
//! as the modern era: every request carries protocol version and client
//! capabilities in `_meta`, `server/discover` is mandatory, and list results
//! are cacheable. Also answers the legacy `initialize` handshake so a client
//! still on 2024-11-05 / 2025-11-25 can connect (dual-era, as the versioning
//! spec permits).
//!
//! The tool set is read-only. A mutating tool must go through
//! `harness::guard_command` and must update the test that pins that.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::insights::{self, McpToolInfo};

/// MCP 2.0 / 2026-07-28. The only modern version this build speaks.
pub const PROTOCOL_VERSION: &str = "2026-07-28";
pub const SERVER_NAME: &str = "gitpulse-mcp";

/// Legacy handshake-based revisions this binary still answers.
const LEGACY_VERSIONS: &[&str] = &["2025-11-25", "2024-11-05"];

const TOOLS_TTL_MS: u64 = 3_600_000;
const DISCOVER_TTL_MS: u64 = 3_600_000;

pub fn server_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

fn server_info() -> Value {
    json!({ "name": SERVER_NAME, "version": server_version() })
}

fn result_meta() -> Value {
    json!({ "io.modelcontextprotocol/serverInfo": server_info() })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Era {
    Unknown,
    Modern,
    Legacy,
}

#[derive(Debug, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Serialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: &'static str,
    pub id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<Value>,
}

fn ok(id: Value, result: Value) -> JsonRpcResponse {
    JsonRpcResponse {
        jsonrpc: "2.0",
        id,
        result: Some(result),
        error: None,
    }
}

fn err(id: Value, code: i32, message: impl Into<String>, data: Option<Value>) -> JsonRpcResponse {
    let mut error = json!({ "code": code, "message": message.into() });
    if let Some(data) = data {
        error["data"] = data;
    }
    JsonRpcResponse {
        jsonrpc: "2.0",
        id,
        result: None,
        error: Some(error),
    }
}

fn tool_annotations() -> Value {
    json!({
        "readOnlyHint": true,
        "destructiveHint": false,
        "idempotentHint": true,
        "openWorldHint": false
    })
}

fn tool(name: &str, title: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    json!({
        "name": name,
        "title": title,
        "description": description,
        "inputSchema": {
            "type": "object",
            "properties": properties,
            "required": required,
            "additionalProperties": false
        },
        "annotations": tool_annotations()
    })
}

fn repo_prop() -> Value {
    json!({ "type": "string", "description": "Absolute path to a git repository" })
}

fn budget_prop() -> Value {
    json!({ "type": "number", "description": "Maximum tokens of results" })
}

/// Advertised tools, in a stable order so clients can cache the catalog.
pub fn tools() -> Vec<Value> {
    vec![
        tool(
            "gitpulse_insights",
            "Repository insights",
            "One-shot snapshot of worktrees, agent sessions, uncommitted changes, overlapping dirty files, ledger, and code-graph availability. Facets fail independently so a missed scan never looks clean.",
            json!({ "repo_path": repo_prop() }),
            &["repo_path"],
        ),
        tool(
            "gitpulse_status",
            "Repository status",
            "Inspect repository status, worktrees, ledger, and code graph availability",
            json!({ "repo_path": repo_prop() }),
            &["repo_path"],
        ),
        tool(
            "gitpulse_active_changes",
            "Active changes",
            "Working-tree file list for a worktree (path, staged, conflicted, churn), capped. A failed read is reported rather than an empty list.",
            json!({
                "repo_path": repo_prop(),
                "worktree_path": { "type": "string", "description": "Worktree to inspect; defaults to repo_path" },
                "limit": { "type": "number", "description": "Maximum files to return (default 200, max 500)" }
            }),
            &["repo_path"],
        ),
        tool(
            "gitpulse_collision_risk",
            "Collision risk",
            "Files with uncommitted changes in more than one worktree — parallel agent checkouts editing the same path. Unscanned worktrees are counted, never implied clean.",
            json!({ "repo_path": repo_prop() }),
            &["repo_path"],
        ),
        tool(
            "gitpulse_change_context",
            "Change context",
            "In-flight context for one worktree: branch, dirty files, parked merge/rebase, bound task, and collisions that involve it.",
            json!({
                "repo_path": repo_prop(),
                "worktree_path": { "type": "string", "description": "Worktree to describe; defaults to repo_path" }
            }),
            &["repo_path"],
        ),
        tool(
            "gitpulse_ledger_events",
            "Ledger events",
            "Read durable SQLite ledger event history (actor, tool, verdict, changes)",
            json!({
                "repo_path": repo_prop(),
                "limit": { "type": "number", "description": "Maximum number of events (default 50)" }
            }),
            &["repo_path"],
        ),
        tool(
            "gitpulse_task_view",
            "Task view",
            "Read task details, leases, and worktree binding from dc-store",
            json!({ "repo_path": repo_prop() }),
            &["repo_path"],
        ),
        tool(
            "gitpulse_codeintel_search",
            "Symbol search",
            "Search symbols across indexed code files via in-process devmap",
            json!({
                "repo_path": repo_prop(),
                "query": { "type": "string", "description": "Symbol name or prefix" },
                "budget": budget_prop()
            }),
            &["repo_path", "query"],
        ),
        tool(
            "gitpulse_codeintel_impact",
            "Impact / blast radius",
            "Compute downstream blast radius / affected callers for a symbol or file",
            json!({
                "repo_path": repo_prop(),
                "target": { "type": "string", "description": "Symbol or file path" },
                "budget": budget_prop()
            }),
            &["repo_path", "target"],
        ),
        tool(
            "gitpulse_codeintel_dependencies",
            "File dependencies",
            "What a file depends on — the mirror of impact. Answers outgoing edges from the devmap code graph",
            json!({
                "repo_path": repo_prop(),
                "file_path": { "type": "string", "description": "Repo-relative file whose dependencies to read" },
                "budget": budget_prop()
            }),
            &["repo_path", "file_path"],
        ),
        tool(
            "gitpulse_codeintel_trace",
            "Trace between symbols",
            "Shortest edge path between two symbols in the devmap code graph",
            json!({
                "repo_path": repo_prop(),
                "from": { "type": "string", "description": "Symbol to trace from" },
                "to": { "type": "string", "description": "Symbol to trace to" },
                "budget": budget_prop()
            }),
            &["repo_path", "from", "to"],
        ),
        tool(
            "gitpulse_codeintel_dead_symbols",
            "Dead symbols",
            "Unreferenced symbols in the indexed code graph. Unavailable is reported, never an empty list that reads as none found.",
            json!({
                "repo_path": repo_prop(),
                "budget": budget_prop()
            }),
            &["repo_path"],
        ),
        tool(
            "gitpulse_provenance",
            "Commit provenance",
            "Read Git-native verification notes and confidence decay for a commit",
            json!({
                "repo_path": repo_prop(),
                "commit_sha": { "type": "string", "description": "Commit SHA" },
                "base_branch": { "type": "string", "description": "Base branch to compute distance against (default HEAD)" }
            }),
            &["repo_path", "commit_sha"],
        ),
    ]
}

pub fn tool_catalog() -> Vec<McpToolInfo> {
    tools()
        .into_iter()
        .map(|t| McpToolInfo {
            name: t["name"].as_str().unwrap_or_default().to_string(),
            title: t["title"].as_str().unwrap_or_default().to_string(),
            description: t["description"].as_str().unwrap_or_default().to_string(),
        })
        .collect()
}

fn tools_list_result(modern: bool) -> Value {
    let tools = tools();
    if modern {
        json!({
            "resultType": "complete",
            "tools": tools,
            "ttlMs": TOOLS_TTL_MS,
            "cacheScope": "public",
            "_meta": result_meta()
        })
    } else {
        json!({ "tools": tools })
    }
}

fn discover_result() -> Value {
    json!({
        "resultType": "complete",
        "supportedVersions": [PROTOCOL_VERSION],
        "capabilities": { "tools": {} },
        "instructions": "Read-only GitPulse control plane. Start with gitpulse_insights for a repository snapshot (worktrees, agent sessions, collisions, ledger, code graph). Use gitpulse_change_context before editing, and gitpulse_collision_risk before parallel agent work. Pass absolute repo_path on every call. This server never mutates git state.",
        "ttlMs": DISCOVER_TTL_MS,
        "cacheScope": "public",
        "_meta": result_meta()
    })
}

fn complete_tool_result(payload: Value) -> Value {
    let text = serde_json::to_string_pretty(&payload).unwrap_or_default();
    json!({
        "resultType": "complete",
        "content": [{ "type": "text", "text": text }],
        "structuredContent": payload,
        "isError": false,
        "_meta": result_meta()
    })
}

fn tool_exec_error(message: impl Into<String>) -> Value {
    let message = message.into();
    json!({
        "resultType": "complete",
        "content": [{ "type": "text", "text": message }],
        "isError": true,
        "_meta": result_meta()
    })
}

fn handle_tool_call(name: &str, arguments: &Value) -> Result<Value, String> {
    match name {
        "gitpulse_insights" => {
            let repo = arguments["repo_path"].as_str().ok_or("missing repo_path")?;
            Ok(json!(insights::snapshot(repo)))
        }
        "gitpulse_status" => {
            let repo = arguments["repo_path"].as_str().ok_or("missing repo_path")?;
            let ledger_status = crate::ledger::status(repo);
            let codeintel_status = crate::codeintel::status(repo);
            const WORKTREE_CAP: usize = 64;
            let worktrees = match crate::engine::worktree::list_worktrees(repo) {
                Ok(list) => {
                    let total = list.len();
                    let truncated = total > WORKTREE_CAP;
                    let items: Vec<Value> = list
                        .into_iter()
                        .take(WORKTREE_CAP)
                        .map(|w| {
                            json!({
                                "path": w.path,
                                "name": w.name,
                                "branch": w.branch,
                                "is_main": w.is_main,
                                "is_bare": w.is_bare,
                                "dirty_files": w.dirty_files,
                            })
                        })
                        .collect();
                    json!({
                        "ok": true,
                        "count": total,
                        "truncated": truncated,
                        "items": items,
                    })
                }
                Err(error) => json!({
                    "ok": false,
                    "error": error,
                    "count": 0,
                    "truncated": false,
                    "items": [],
                }),
            };
            Ok(json!({
                "repo_path": repo,
                "ledger": ledger_status,
                "codeintel": codeintel_status,
                "worktrees": worktrees,
            }))
        }
        "gitpulse_active_changes" => {
            let repo = arguments["repo_path"].as_str().ok_or("missing repo_path")?;
            let worktree = arguments["worktree_path"].as_str();
            let limit = arguments["limit"].as_u64().map(|n| n as u32);
            Ok(json!(insights::active_changes(repo, worktree, limit)))
        }
        "gitpulse_collision_risk" => {
            let repo = arguments["repo_path"].as_str().ok_or("missing repo_path")?;
            Ok(json!(insights::collision_risk(repo)))
        }
        "gitpulse_change_context" => {
            let repo = arguments["repo_path"].as_str().ok_or("missing repo_path")?;
            let worktree = arguments["worktree_path"].as_str();
            Ok(json!(insights::change_context(repo, worktree)))
        }
        "gitpulse_ledger_events" => {
            let repo = arguments["repo_path"].as_str().ok_or("missing repo_path")?;
            let limit = arguments["limit"].as_u64().unwrap_or(50) as u32;
            let events = crate::ledger::tail(repo, 0, limit).map_err(|e| e.to_string())?;
            Ok(json!({
                "events": events,
                "total": events.len()
            }))
        }
        "gitpulse_task_view" => {
            let repo = arguments["repo_path"].as_str().ok_or("missing repo_path")?;
            let view = crate::tasks::view(repo);
            Ok(json!(view))
        }
        "gitpulse_codeintel_search" => {
            let repo = arguments["repo_path"].as_str().ok_or("missing repo_path")?;
            let query = arguments["query"].as_str().ok_or("missing query")?;
            let budget = arguments["budget"].as_u64().map(|b| b as u32);
            Ok(json!(crate::codeintel::search(repo, query, budget)))
        }
        "gitpulse_codeintel_impact" => {
            let repo = arguments["repo_path"].as_str().ok_or("missing repo_path")?;
            let target = arguments["target"].as_str().ok_or("missing target")?;
            let budget = arguments["budget"].as_u64().map(|b| b as u32);
            Ok(json!(crate::codeintel::impact(repo, target, budget)))
        }
        "gitpulse_codeintel_dependencies" => {
            let repo = arguments["repo_path"].as_str().ok_or("missing repo_path")?;
            let file = arguments["file_path"].as_str().ok_or("missing file_path")?;
            let budget = arguments["budget"].as_u64().map(|b| b as u32);
            Ok(json!(crate::codeintel::dependencies(repo, file, budget)))
        }
        "gitpulse_codeintel_trace" => {
            let repo = arguments["repo_path"].as_str().ok_or("missing repo_path")?;
            let from = arguments["from"].as_str().ok_or("missing from")?;
            let to = arguments["to"].as_str().ok_or("missing to")?;
            let budget = arguments["budget"].as_u64().map(|b| b as u32);
            Ok(json!(crate::codeintel::trace_between(
                repo, from, to, budget
            )))
        }
        "gitpulse_codeintel_dead_symbols" => {
            let repo = arguments["repo_path"].as_str().ok_or("missing repo_path")?;
            let budget = arguments["budget"].as_u64().map(|b| b as u32);
            Ok(json!(crate::codeintel::dead_symbols(repo, budget)))
        }
        "gitpulse_provenance" => {
            let repo = arguments["repo_path"].as_str().ok_or("missing repo_path")?;
            let commit_sha = arguments["commit_sha"]
                .as_str()
                .ok_or("missing commit_sha")?;
            let base = arguments["base_branch"].as_str();
            let freshness = crate::engine::provenance::compute_freshness(repo, commit_sha, base);
            Ok(json!(freshness))
        }
        _ => Err(format!("Unknown tool: {name}")),
    }
}

fn meta_object(params: &Value) -> Option<&Value> {
    params.get("_meta")
}

fn requested_version(params: &Value) -> Option<&str> {
    meta_object(params)?
        .get("io.modelcontextprotocol/protocolVersion")?
        .as_str()
}

fn has_client_capabilities(params: &Value) -> bool {
    meta_object(params)
        .and_then(|m| m.get("io.modelcontextprotocol/clientCapabilities"))
        .is_some()
}

fn is_modern_request(params: &Value) -> bool {
    requested_version(params).is_some()
}

fn unsupported_version(id: Value, requested: &str) -> JsonRpcResponse {
    err(
        id,
        -32022,
        "Unsupported protocol version",
        Some(json!({
            "supported": [PROTOCOL_VERSION],
            "requested": requested
        })),
    )
}

fn missing_meta(id: Value, field: &str) -> JsonRpcResponse {
    err(
        id,
        -32602,
        format!("Invalid params: missing _meta.{field}"),
        None,
    )
}

/// Answers one request, or `None` when the message was a notification.
pub fn process_request(req: JsonRpcRequest, era: &mut Era) -> Option<JsonRpcResponse> {
    let id = req.id.clone()?;

    if req.method == "initialize" {
        *era = Era::Legacy;
        let requested = req.params["protocolVersion"]
            .as_str()
            .unwrap_or("2024-11-05");
        let protocol_version = if LEGACY_VERSIONS.contains(&requested) {
            requested
        } else if requested == PROTOCOL_VERSION {
            // A modern client that still sends initialize: answer the handshake
            // they asked for so they are not stuck, and name the modern version.
            PROTOCOL_VERSION
        } else {
            LEGACY_VERSIONS[1]
        };
        return Some(ok(
            id,
            json!({
                "protocolVersion": protocol_version,
                "capabilities": { "tools": {} },
                "serverInfo": server_info()
            }),
        ));
    }

    let modern = is_modern_request(&req.params);
    if modern {
        *era = Era::Modern;
        let version = requested_version(&req.params).unwrap_or("");
        if version != PROTOCOL_VERSION {
            return Some(unsupported_version(id, version));
        }
        if !has_client_capabilities(&req.params) {
            return Some(missing_meta(
                id,
                "io.modelcontextprotocol/clientCapabilities",
            ));
        }
    } else if *era != Era::Legacy && req.method != "notifications/initialized" {
        // Modern clients must send _meta on every request. A dual-era process
        // that already answered initialize may receive legacy calls without it.
        return Some(missing_meta(id, "io.modelcontextprotocol/protocolVersion"));
    }

    Some(match req.method.as_str() {
        "server/discover" => ok(id, discover_result()),
        "notifications/initialized" => ok(id, json!({})),
        "tools/list" => ok(id, tools_list_result(modern || *era == Era::Modern)),
        "tools/call" => {
            let name = req.params["name"].as_str().unwrap_or("");
            let arguments = req.params.get("arguments").cloned().unwrap_or(json!({}));
            match handle_tool_call(name, &arguments) {
                Ok(payload) => {
                    if modern || *era == Era::Modern {
                        ok(id, complete_tool_result(payload))
                    } else {
                        ok(
                            id,
                            json!({
                                "content": [{
                                    "type": "text",
                                    "text": serde_json::to_string_pretty(&payload).unwrap_or_default()
                                }]
                            }),
                        )
                    }
                }
                Err(message) if message.starts_with("Unknown tool:") => {
                    err(id, -32602, message, None)
                }
                Err(message) => {
                    if modern || *era == Era::Modern {
                        ok(id, tool_exec_error(message))
                    } else {
                        err(id, -32603, message, None)
                    }
                }
            }
        }
        _ => err(
            id,
            -32601,
            format!("Method not found: {}", req.method),
            None,
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn modern_meta() -> Value {
        json!({
            "io.modelcontextprotocol/protocolVersion": PROTOCOL_VERSION,
            "io.modelcontextprotocol/clientInfo": { "name": "gitpulse-test", "version": "0" },
            "io.modelcontextprotocol/clientCapabilities": {}
        })
    }

    fn request(method: &str, id: Option<Value>, params: Value) -> JsonRpcRequest {
        JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id,
            method: method.into(),
            params,
        }
    }

    fn call(method: &str, params: Value) -> JsonRpcResponse {
        let mut era = Era::Unknown;
        process_request(request(method, Some(json!(1)), params), &mut era).expect("answered")
    }

    #[test]
    fn discover_is_implemented_and_cacheable() {
        let resp = call("server/discover", json!({ "_meta": modern_meta() }));
        let result = resp.result.expect("result");
        assert_eq!(result["resultType"], "complete");
        assert_eq!(result["supportedVersions"], json!([PROTOCOL_VERSION]));
        assert_eq!(result["ttlMs"], DISCOVER_TTL_MS);
        assert_eq!(result["cacheScope"], "public");
        assert_eq!(
            result["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
            SERVER_NAME
        );
        assert!(result["capabilities"]["tools"].is_object());
        assert!(result["instructions"]
            .as_str()
            .unwrap()
            .contains("gitpulse_insights"));
    }

    #[test]
    fn modern_tools_list_is_cacheable_and_ordered() {
        let resp = call("tools/list", json!({ "_meta": modern_meta() }));
        let result = resp.result.expect("result");
        assert_eq!(result["resultType"], "complete");
        assert_eq!(result["ttlMs"], TOOLS_TTL_MS);
        assert_eq!(result["cacheScope"], "public");
        let names: Vec<&str> = result["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(names[0], "gitpulse_insights");
        assert!(names.contains(&"gitpulse_collision_risk"));
        assert!(names.contains(&"gitpulse_change_context"));
        assert!(names.contains(&"gitpulse_codeintel_dead_symbols"));
        let listed = tools();
        let again: Vec<&str> = listed.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(names, again);
    }

    #[test]
    fn every_advertised_tool_has_a_dispatch_arm_and_the_reverse() {
        let source = include_str!("mod.rs");
        let advertised: std::collections::BTreeSet<String> = tools()
            .into_iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect();
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
            advertised.len() >= 8,
            "scan found only {}",
            advertised.len()
        );
        assert_eq!(advertised, dispatched);
    }

    #[test]
    fn missing_meta_is_invalid_params() {
        let resp = call("tools/list", json!({}));
        assert_eq!(resp.error.expect("error")["code"], -32602);
    }

    #[test]
    fn unsupported_protocol_version_names_what_we_speak() {
        let mut meta = modern_meta();
        meta["io.modelcontextprotocol/protocolVersion"] = json!("1900-01-01");
        let resp = call("tools/list", json!({ "_meta": meta }));
        let error = resp.error.expect("error");
        assert_eq!(error["code"], -32022);
        assert_eq!(error["data"]["supported"], json!([PROTOCOL_VERSION]));
        assert_eq!(error["data"]["requested"], "1900-01-01");
    }

    #[test]
    fn a_notification_gets_no_response() {
        let mut era = Era::Unknown;
        assert!(process_request(
            request("notifications/initialized", None, json!({})),
            &mut era
        )
        .is_none());
        assert!(process_request(
            request("notifications/cancelled", None, json!({})),
            &mut era
        )
        .is_none());
    }

    #[test]
    fn initialize_still_answers_legacy_clients() {
        let mut era = Era::Unknown;
        let resp = process_request(
            request(
                "initialize",
                Some(json!(1)),
                json!({ "protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": { "name": "old", "version": "1" } }),
            ),
            &mut era,
        )
        .expect("answered");
        assert_eq!(era, Era::Legacy);
        let result = resp.result.expect("result");
        assert_eq!(result["protocolVersion"], "2024-11-05");
        assert_eq!(result["serverInfo"]["name"], SERVER_NAME);
        assert!(result.get("resultType").is_none());
        let list = process_request(request("tools/list", Some(json!(2)), json!({})), &mut era)
            .expect("legacy list");
        assert!(list.result.unwrap()["tools"].is_array());
    }

    #[test]
    fn unknown_tool_is_a_protocol_error() {
        let resp = call(
            "tools/call",
            json!({
                "name": "gitpulse_not_a_tool",
                "arguments": {},
                "_meta": modern_meta()
            }),
        );
        assert!(resp.result.is_none());
        assert_eq!(resp.error.expect("error")["code"], -32602);
    }

    #[test]
    fn missing_tool_argument_is_an_execution_error_not_a_silent_success() {
        let resp = call(
            "tools/call",
            json!({
                "name": "gitpulse_insights",
                "arguments": {},
                "_meta": modern_meta()
            }),
        );
        let result = resp.result.expect("tool result");
        assert_eq!(result["isError"], true);
        assert_eq!(result["resultType"], "complete");
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("repo_path"), "{text}");
    }

    #[test]
    fn gitpulse_status_always_reports_a_worktrees_facet() {
        let payload = handle_tool_call("gitpulse_status", &json!({ "repo_path": "/no/such/repo" }))
            .expect("the tool itself answers");
        assert_eq!(payload["worktrees"]["ok"], false);
        assert!(payload["worktrees"]["error"]
            .as_str()
            .is_some_and(|e| !e.is_empty()));
    }

    #[test]
    fn gitpulse_task_view_required_args_match_the_handler() {
        let tool = tools()
            .into_iter()
            .find(|t| t["name"] == "gitpulse_task_view")
            .unwrap();
        let required: Vec<&str> = tool["inputSchema"]["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(required, vec!["repo_path"]);
        assert!(tool["inputSchema"]["properties"]["task_id"].is_null());
    }

    #[test]
    fn no_advertised_tool_offers_an_ungated_mutation() {
        for tool in tools() {
            let name = tool["name"].as_str().unwrap();
            for verb in [
                "write", "commit", "push", "checkout", "apply", "delete", "revert", "ingest",
                "bind", "revoke",
            ] {
                assert!(!name.contains(verb), "{name} looks like a mutation");
            }
            assert_eq!(tool["annotations"]["readOnlyHint"], true);
        }
    }

    #[test]
    fn every_advertised_tool_declares_a_schema_and_its_required_arguments() {
        for tool in tools() {
            let name = tool["name"].as_str().unwrap();
            assert!(tool["description"].as_str().unwrap().len() > 20);
            assert!(tool["title"].as_str().unwrap().len() > 2);
            let schema = &tool["inputSchema"];
            assert_eq!(schema["type"], "object");
            let required = schema["required"].as_array().unwrap();
            for field in required {
                let field = field.as_str().unwrap();
                assert!(
                    schema["properties"][field].is_object(),
                    "{name} requires {field} with no schema"
                );
            }
        }
    }

    #[test]
    fn tool_names_are_unique() {
        let mut seen = std::collections::BTreeSet::new();
        for tool in tools() {
            assert!(seen.insert(tool["name"].as_str().unwrap().to_string()));
        }
    }
}
