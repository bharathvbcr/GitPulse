//! MCP protocol surface for `gitpulse-mcp`.
//!
//! Speaks [MCP 2026-07-28](https://modelcontextprotocol.io/specification/2026-07-28)
//! as the modern era: every request carries protocol version and client
//! capabilities in `_meta`, `server/discover` is mandatory, and list results are
//! cacheable and paginated. Also answers the legacy `initialize` handshake so a
//! client still on 2024-11-05 / 2025-11-25 can connect — which the versioning
//! spec permits explicitly: *"An `initialize` request selects legacy semantics,
//! scoped to the stdio process."* That scoping is what [`Era`] is, and it is why
//! it latches rather than being recomputed per request.
//!
//! Four capabilities are declared and all four are answered: `tools`,
//! `resources`, `prompts`, `completions`. The method set is taken from the
//! canonical schema for this revision, which is also why there is no `ping` and
//! no `logging/setLevel` in the modern era — neither exists in 2026-07-28.
//!
//! The whole surface is read-only. A mutating tool must go through
//! `harness::guard_command` and must update the test that pins that.

pub mod complete;
pub mod page;
pub mod prompts;
pub mod resources;
pub mod uri;
pub mod validate;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::insights::{self, McpToolInfo};

/// MCP 2.0 / 2026-07-28. The only modern version this build speaks.
pub const PROTOCOL_VERSION: &str = "2026-07-28";
pub const SERVER_NAME: &str = "gitpulse-mcp";

/// Legacy handshake-based revisions this binary still answers.
pub const LEGACY_VERSIONS: &[&str] = &["2025-11-25", "2024-11-05"];

const TOOLS_TTL_MS: u64 = 3_600_000;
const DISCOVER_TTL_MS: u64 = 3_600_000;
/// Prompts and templates are compiled in, so they are as cacheable as the tool
/// list. The server-document list is too — it names documents, not their
/// contents, and `resources/read` is where freshness matters.
const LIST_TTL_MS: u64 = 3_600_000;

/// Ledger events returned when a caller does not say. Also the number the
/// `ledger` resource facet reads, so the tool and the document agree.
pub const LEDGER_DEFAULT_LIMIT: u32 = 50;
/// Ceiling on `gitpulse_ledger_events`. Without one, `limit: 4294967295`
/// materialises the entire ledger into a single stdio write.
pub const LEDGER_MAX_LIMIT: u32 = 1_000;
/// Ceiling on `gitpulse_active_changes`, matching what the backend enforces.
pub const CHANGES_MAX_LIMIT: u32 = 500;
/// Ceiling on the code-intelligence token budget.
pub const BUDGET_MAX: u32 = 200_000;
/// Longest `repo_path` / symbol argument accepted. Well past any real path,
/// short enough that a megabyte of `A`s is refused before it reaches git.
const MAX_ARG_CHARS: u32 = 4_096;

pub fn server_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

fn server_info() -> Value {
    json!({ "name": SERVER_NAME, "version": server_version() })
}

fn result_meta() -> Value {
    json!({ "io.modelcontextprotocol/serverInfo": server_info() })
}

/// What this server offers. One definition, used by `server/discover`, by the
/// legacy `initialize` reply, and by the `gitpulse://server/manifest` document,
/// so a capability can never be advertised in one place and missing in another.
pub fn capabilities() -> Value {
    // Every field is an empty object on purpose: `listChanged` and `subscribe`
    // would promise notifications this server never sends, and a client that
    // subscribed would wait forever for one.
    json!({
        "tools": {},
        "resources": {},
        "prompts": {},
        "completions": {}
    })
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
    /// `None` only when the field is **absent** — that is what makes a message
    /// a notification. An explicit `"id": null` is `Some(Value::Null)`.
    ///
    /// Serde's stock `Option` collapses both to `None`, which made a null id
    /// indistinguishable from a notification: the server answered neither, and
    /// a client that sent one waited forever for a response that the spec says
    /// should have been an error.
    #[serde(default, deserialize_with = "present_id")]
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

fn present_id<'de, D>(deserializer: D) -> Result<Option<Value>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Value::deserialize(deserializer).map(Some)
}

#[derive(Debug, Clone, Serialize)]
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

pub fn err(
    id: Value,
    code: i32,
    message: impl Into<String>,
    data: Option<Value>,
) -> JsonRpcResponse {
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

/// Finish a modern result: `resultType` is required on **every** modern result
/// (*"The `result` **MUST** include a `resultType` field"*), and `serverInfo`
/// is a SHOULD. A legacy result carries neither.
///
/// Going through one function is what stops a newly added method from shipping
/// a result that omits `resultType`; a test walks every method to prove it.
fn envelope(modern: bool, mut body: Value) -> Value {
    if modern {
        body["resultType"] = json!("complete");
        body["_meta"] = result_meta();
    }
    body
}

/// Attach caching hints to a list result. Modern only: `ttlMs` / `cacheScope`
/// do not exist in the legacy revisions.
fn cacheable(modern: bool, mut body: Value, ttl_ms: u64) -> Value {
    if modern {
        body["ttlMs"] = json!(ttl_ms);
        body["cacheScope"] = json!("public");
    }
    body
}

fn tool_annotations() -> Value {
    json!({
        "readOnlyHint": true,
        "destructiveHint": false,
        "idempotentHint": true,
        "openWorldHint": false
    })
}

fn tool(
    name: &str,
    title: &str,
    description: &str,
    properties: Value,
    required: &[&str],
    output: Value,
) -> Value {
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
        "outputSchema": output,
        "annotations": tool_annotations()
    })
}

fn repo_prop() -> Value {
    json!({
        "type": "string",
        "description": "Absolute path to a git repository",
        "minLength": 1,
        "maxLength": MAX_ARG_CHARS
    })
}

fn path_prop(description: &str) -> Value {
    json!({
        "type": "string",
        "description": description,
        "minLength": 1,
        "maxLength": MAX_ARG_CHARS
    })
}

fn budget_prop() -> Value {
    json!({
        "type": "integer",
        "description": "Maximum tokens of results",
        "minimum": 1,
        "maximum": BUDGET_MAX
    })
}

/// The shape every code-intelligence tool returns.
///
/// `available` is required because it is the field that separates "the graph
/// says there are none" from "there is no graph": an `items: []` on its own
/// reads as the first when it means the second.
fn codeintel_output() -> Value {
    json!({
        "type": "object",
        "properties": {
            "available": { "type": "boolean" },
            "reason": { "type": "string" },
            "items": { "type": "array" },
            "total": { "type": "integer" },
            "shown": { "type": "integer" },
            "truncated": { "type": "boolean" }
        },
        "required": ["available", "items", "total", "shown", "truncated"]
    })
}

/// The shape of a facet that reports its own scan failure.
fn ok_error_output(extra: Value, required: &[&str]) -> Value {
    let mut properties = json!({
        "ok": { "type": "boolean" },
        "error": { "type": "string" }
    });
    if let (Some(target), Some(source)) = (properties.as_object_mut(), extra.as_object()) {
        for (key, value) in source {
            target.insert(key.clone(), value.clone());
        }
    }
    let mut names = vec!["ok".to_string(), "error".to_string()];
    names.extend(required.iter().map(|s| s.to_string()));
    json!({ "type": "object", "properties": properties, "required": names })
}

/// Advertised tools, in a stable order so clients can cache the catalog.
///
/// Built once. The catalog is ~11 KB of `json!` and every `tools/call` used to
/// rebuild all of it just to find one tool's `inputSchema` to validate against
/// — measured at 68 µs per call, which is most of the cost of rejecting a bad
/// argument. It is immutable and compiled in, so there is nothing to invalidate.
pub fn tools() -> Vec<Value> {
    catalog().to_vec()
}

/// The catalog itself, borrowed. Callers that only read it should use this and
/// skip the clone.
fn catalog() -> &'static [Value] {
    static CATALOG: std::sync::OnceLock<Vec<Value>> = std::sync::OnceLock::new();
    CATALOG.get_or_init(build_tools)
}

fn build_tools() -> Vec<Value> {
    vec![
        tool(
            "gitpulse_insights",
            "Repository insights",
            "One-shot snapshot of worktrees, agent sessions, uncommitted changes, overlapping dirty files, ledger, and code-graph availability. Facets fail independently so a missed scan never looks clean.",
            json!({ "repo_path": repo_prop() }),
            &["repo_path"],
            json!({
                "type": "object",
                "properties": {
                    "repo_path": { "type": "string" },
                    "worktrees": { "type": "object" },
                    "agents": { "type": "object" },
                    "changes": { "type": "object" },
                    "collisions": { "type": "object" },
                    "ledger": { "type": "object" },
                    "codeintel": { "type": "object" }
                },
                "required": ["repo_path", "worktrees", "agents", "changes", "collisions", "ledger", "codeintel"]
            }),
        ),
        tool(
            "gitpulse_status",
            "Repository status",
            "Inspect repository status, worktrees, ledger, and code graph availability",
            json!({ "repo_path": repo_prop() }),
            &["repo_path"],
            json!({
                "type": "object",
                "properties": {
                    "repo_path": { "type": "string" },
                    "ledger": { "type": "object" },
                    "codeintel": { "type": "object" },
                    "worktrees": { "type": "object" }
                },
                "required": ["repo_path", "ledger", "codeintel", "worktrees"]
            }),
        ),
        tool(
            "gitpulse_active_changes",
            "Active changes",
            "Working-tree file list for a worktree (path, staged, conflicted, churn), capped. A failed read is reported rather than an empty list.",
            json!({
                "repo_path": repo_prop(),
                "worktree_path": path_prop("Worktree to inspect; defaults to repo_path"),
                "limit": {
                    "type": "integer",
                    "description": "Maximum files to return (default 200, max 500)",
                    "minimum": 1,
                    "maximum": CHANGES_MAX_LIMIT
                }
            }),
            &["repo_path"],
            ok_error_output(
                json!({
                    "repo_path": { "type": "string" },
                    "worktree_path": { "type": "string" },
                    "files": { "type": "array" },
                    "total": { "type": "integer" },
                    "shown": { "type": "integer" },
                    "truncated": { "type": "boolean" }
                }),
                &["repo_path", "files", "total", "shown", "truncated"],
            ),
        ),
        tool(
            "gitpulse_collision_risk",
            "Collision risk",
            "Files with uncommitted changes in more than one worktree — parallel agent checkouts editing the same path. Unscanned worktrees are counted, never implied clean.",
            json!({ "repo_path": repo_prop() }),
            &["repo_path"],
            ok_error_output(
                json!({
                    "overlapping_files": { "type": "integer" },
                    "worktrees_involved": { "type": "integer" },
                    "scanned_worktrees": { "type": "integer" },
                    "unscanned_worktrees": { "type": "integer" },
                    "truncated": { "type": "boolean" },
                    "items": { "type": "array" }
                }),
                &["overlapping_files", "scanned_worktrees", "unscanned_worktrees", "items"],
            ),
        ),
        tool(
            "gitpulse_change_context",
            "Change context",
            "In-flight context for one worktree: branch, dirty files, parked merge/rebase, bound task, and collisions that involve it.",
            json!({
                "repo_path": repo_prop(),
                "worktree_path": path_prop("Worktree to describe; defaults to repo_path")
            }),
            &["repo_path"],
            json!({
                "type": "object",
                "properties": {
                    "repo_path": { "type": "string" },
                    "worktree": { "type": "object" },
                    "task_id": { "type": "string" },
                    "changes": { "type": "object" },
                    "collisions": { "type": "array" }
                },
                "required": ["repo_path", "worktree", "task_id", "changes", "collisions"]
            }),
        ),
        tool(
            "gitpulse_ledger_events",
            "Ledger events",
            "Read durable ledger event history (actor, tool, verdict, changes), oldest first from `cursor`. Page by passing the last `id` you saw back as `cursor`; `truncated` says whether more follow.",
            json!({
                "repo_path": repo_prop(),
                "cursor": {
                    "type": "integer",
                    "description": "Return events with an id greater than this. 0 (the default) starts at the beginning of history.",
                    "minimum": 0,
                    "maximum": i64::MAX as u64
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of events (default 50, max 1000)",
                    "minimum": 1,
                    "maximum": LEDGER_MAX_LIMIT
                }
            }),
            &["repo_path"],
            json!({
                "type": "object",
                "properties": {
                    "ok": { "type": "boolean" },
                    "error": { "type": "string" },
                    "events": { "type": "array" },
                    "returned": { "type": "integer" },
                    "truncated": { "type": "boolean" },
                    "next_cursor": { "type": "integer" }
                },
                "required": ["ok", "events", "returned", "truncated"]
            }),
        ),
        tool(
            "gitpulse_task_view",
            "Task view",
            "Read task details, leases, and worktree binding from dc-store",
            json!({ "repo_path": repo_prop() }),
            &["repo_path"],
            json!({ "type": "object" }),
        ),
        tool(
            "gitpulse_codeintel_search",
            "Symbol search",
            "Search symbols across indexed code files via in-process devmap",
            json!({
                "repo_path": repo_prop(),
                "query": path_prop("Symbol name or prefix"),
                "budget": budget_prop()
            }),
            &["repo_path", "query"],
            codeintel_output(),
        ),
        tool(
            "gitpulse_codeintel_impact",
            "Impact / blast radius",
            "Compute downstream blast radius / affected callers for a symbol or file",
            json!({
                "repo_path": repo_prop(),
                "target": path_prop("Symbol or file path"),
                "budget": budget_prop()
            }),
            &["repo_path", "target"],
            codeintel_output(),
        ),
        tool(
            "gitpulse_codeintel_dependencies",
            "File dependencies",
            "What a file depends on — the mirror of impact. Answers outgoing edges from the devmap code graph",
            json!({
                "repo_path": repo_prop(),
                "file_path": path_prop("Repo-relative file whose dependencies to read"),
                "budget": budget_prop()
            }),
            &["repo_path", "file_path"],
            codeintel_output(),
        ),
        tool(
            "gitpulse_codeintel_trace",
            "Trace between symbols",
            "Shortest edge path between two symbols in the devmap code graph",
            json!({
                "repo_path": repo_prop(),
                "from": path_prop("Symbol to trace from"),
                "to": path_prop("Symbol to trace to"),
                "budget": budget_prop()
            }),
            &["repo_path", "from", "to"],
            codeintel_output(),
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
            codeintel_output(),
        ),
        tool(
            "gitpulse_provenance",
            "Commit provenance",
            "Read Git-native verification notes and confidence decay for a commit",
            json!({
                "repo_path": repo_prop(),
                "commit_sha": path_prop("Commit SHA"),
                "base_branch": path_prop("Base branch to compute distance against (default HEAD)")
            }),
            &["repo_path", "commit_sha"],
            json!({ "type": "object" }),
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

fn discover_result(modern: bool) -> Value {
    cacheable(
        modern,
        envelope(
            modern,
            json!({
                "supportedVersions": [PROTOCOL_VERSION],
                "capabilities": capabilities(),
                "instructions": "Read-only GitPulse control plane. Start with gitpulse_insights for a repository snapshot (worktrees, agent sessions, collisions, ledger, code graph). Use gitpulse_change_context before editing, and gitpulse_collision_risk before parallel agent work. The same views are addressable as gitpulse://<facet>{+repo_path} resources; gitpulse://server/manifest describes the whole surface. Pass absolute repo_path on every call. This server never mutates git state.",
            }),
        ),
        DISCOVER_TTL_MS,
    )
}

/// One paginated list result, in whichever era's shape applies.
fn list_result(
    modern: bool,
    key: &str,
    items: Vec<Value>,
    params: &Value,
    list: &'static str,
    ttl_ms: u64,
) -> Result<Value, page::CursorError> {
    // A cursor that is present but not a string is malformed, not absent:
    // silently treating `{"cursor": 7}` as page one would restart a listing the
    // client believed it was continuing.
    let cursor = match params.get("cursor") {
        None | Some(Value::Null) => None,
        Some(Value::String(text)) => Some(text.as_str()),
        Some(_) => return Err(page::CursorError::Malformed),
    };
    let paged = page::slice(items, cursor, list)?;
    let mut body = json!({ key: paged.items });
    if let Some(next) = paged.next_cursor {
        body["nextCursor"] = json!(next);
    }
    Ok(cacheable(modern, envelope(modern, body), ttl_ms))
}

fn complete_tool_result(modern: bool, payload: Value) -> Value {
    // The serialized JSON also goes in a text block: the spec asks for it
    // ("a tool that returns structured content SHOULD also return the
    // serialized JSON in a TextContent block") and legacy clients read nothing
    // else.
    let text = serde_json::to_string_pretty(&payload).unwrap_or_else(|error| {
        json!({ "ok": false, "error": format!("result could not be serialized: {error}") })
            .to_string()
    });
    let mut body = json!({
        "content": [{ "type": "text", "text": text }],
        "isError": false
    });
    if modern {
        body["structuredContent"] = payload;
    }
    envelope(modern, body)
}

/// A tool *execution* error: reported inside the result so a model can
/// self-correct, in both eras. The spec names input-validation failures as
/// exactly this kind of error, not a protocol one.
fn tool_exec_error(modern: bool, message: impl Into<String>) -> Value {
    envelope(
        modern,
        json!({
            "content": [{ "type": "text", "text": message.into() }],
            "isError": true
        }),
    )
}

/// Check `arguments` against the named tool's declared `inputSchema`.
///
/// This is the spec's *"Servers **MUST**: Validate all tool inputs"*. Before it
/// existed the schema was decoration and a wrong-typed argument silently became
/// a default.
fn validate_arguments(name: &str, arguments: &Value) -> Result<(), String> {
    let Some(tool) = catalog().iter().find(|t| t["name"] == name) else {
        return Ok(());
    };
    let violations = validate::validate(arguments, &tool["inputSchema"]);
    if violations.is_empty() {
        return Ok(());
    }
    Err(format!(
        "invalid arguments for {name}: {}",
        violations
            .iter()
            .map(validate::Violation::render)
            .collect::<Vec<_>>()
            .join("; ")
    ))
}

fn handle_tool_call(name: &str, arguments: &Value) -> Result<Value, String> {
    // Every arm below reads arguments the schema has already accepted, so a
    // missing required field is unreachable here and the `ok_or` fallbacks are
    // belt-and-braces rather than the real check.
    validate_arguments(name, arguments)?;
    match name {
        "gitpulse_insights" => {
            let repo = arguments["repo_path"].as_str().ok_or("missing repo_path")?;
            Ok(json!(insights::snapshot(repo)))
        }
        "gitpulse_status" => {
            let repo = arguments["repo_path"].as_str().ok_or("missing repo_path")?;
            resources::status_document(repo)
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
            let limit = arguments["limit"]
                .as_u64()
                .map(|n| n as u32)
                .unwrap_or(LEDGER_DEFAULT_LIMIT);
            let address = crate::ledger::bindings::repository_address(repo)
                .map_err(|error| error.to_string())?;
            // `tail` pages *forward* from a cursor — deliberately, so a
            // consumer holding the last id it saw gets the same answer whether
            // it has been listening for an hour or just opened. This tool
            // pinned the cursor at 0 and exposed no way to move it, so it
            // always returned the oldest events with no way to reach recent
            // ones. `total` was the returned count wearing the name of the
            // history size, which reads as a complete answer over a ledger of
            // any size; `returned` says what it is, `truncated` says when more
            // follow, and `next_cursor` is what to pass to get them.
            let cursor = arguments["cursor"].as_i64().unwrap_or(0);
            match crate::ledger::tail_readonly(&address.anchor, cursor, limit)
                .map_err(|e| e.to_string())?
            {
                Some(events) => {
                    let truncated = events.len() as u32 >= limit;
                    let next_cursor = events.last().map(|event| event.id);
                    Ok(json!({
                        "ok": true,
                        "returned": events.len(),
                        "truncated": truncated,
                        "next_cursor": next_cursor,
                        "events": events,
                    }))
                }
                None => Ok(json!({
                    "ok": false,
                    "error": "no ledger in this repository yet; nothing has been recorded",
                    "returned": 0,
                    "truncated": false,
                    "events": [],
                })),
            }
        }
        "gitpulse_task_view" => {
            let repo = arguments["repo_path"].as_str().ok_or("missing repo_path")?;
            let address = crate::ledger::bindings::repository_address(repo)
                .map_err(|error| error.to_string())?;
            let view = crate::tasks::view(&address.anchor);
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

fn invalid_params(id: Value, message: impl Into<String>) -> JsonRpcResponse {
    err(id, -32602, message, None)
}

/// A request that cleared every header check and is ready to be executed.
///
/// The split exists so the transport can run the *body* of a request off the
/// read loop while the *header* work — era latching, version and capability
/// checks — stays strictly ordered on it. Header work is a few field reads;
/// the body can spawn three hundred git processes.
#[derive(Debug)]
pub struct Ready {
    id: Value,
    modern: bool,
    method: String,
    params: Value,
}

impl Ready {
    pub fn method(&self) -> &str {
        &self.method
    }

    /// The JSON-RPC id this request must be answered with.
    pub fn id(&self) -> &Value {
        &self.id
    }

    /// The answer to send when this request outlives its deadline.
    ///
    /// `-32603` rather than a code of our own: the spec reserves `-32000` to
    /// `-32099` for itself and says new codes **SHOULD** be allocated outside
    /// the JSON-RPC reserved range, and a bespoke code no client recognises is
    /// worse than the standard "the server could not complete this".
    pub fn timed_out(&self, budget: std::time::Duration) -> JsonRpcResponse {
        err(
            self.id.clone(),
            -32603,
            format!(
                "{} exceeded the {} ms server budget and was abandoned",
                self.method,
                budget.as_millis()
            ),
            Some(json!({ "method": self.method, "timeoutMs": budget.as_millis() as u64 })),
        )
    }

    /// The answer to send when running this request panicked.
    ///
    /// A panic used to take the whole process with it, which a client sees as a
    /// dead server rather than a failed call.
    pub fn panicked(&self) -> JsonRpcResponse {
        err(
            self.id.clone(),
            -32603,
            format!("{} panicked; the server is still running", self.method),
            Some(json!({ "method": self.method })),
        )
    }
}

/// What [`accept`] decided about a message.
#[derive(Debug)]
pub enum Accepted {
    /// A notification: JSON-RPC forbids a response.
    Silent,
    /// Answered from the header alone — a protocol error, or `initialize`.
    Answered(JsonRpcResponse),
    /// Cleared for execution by [`run`].
    Ready(Ready),
}

/// Answers one request, or `None` when the message was a notification.
///
/// Equivalent to [`accept`] followed by [`run`]; the transport uses the two
/// halves separately so it can bound and isolate the second.
pub fn process_request(req: JsonRpcRequest, era: &mut Era) -> Option<JsonRpcResponse> {
    match accept(req, era) {
        Accepted::Silent => None,
        Accepted::Answered(response) => Some(response),
        Accepted::Ready(ready) => Some(run(ready)),
    }
}

/// Header checks: JSON-RPC well-formedness, era latching, version and
/// capability negotiation. Cheap, ordered, and never touches the filesystem.
pub fn accept(req: JsonRpcRequest, era: &mut Era) -> Accepted {
    let Some(id) = req.id.clone() else {
        return Accepted::Silent;
    };

    // JSON-RPC 2.0 is the only version MCP speaks, and an `id` of `null` is
    // explicitly forbidden ("Unlike base JSON-RPC, the ID MUST NOT be null").
    // Neither was checked before; a `"jsonrpc": "1.0"` message was served as if
    // it were well formed.
    if req.jsonrpc != "2.0" {
        return Accepted::Answered(err(
            id,
            -32600,
            format!(
                "Invalid Request: jsonrpc must be \"2.0\", got {:?}",
                req.jsonrpc
            ),
            None,
        ));
    }
    if id.is_null() {
        return Accepted::Answered(err(
            Value::Null,
            -32600,
            "Invalid Request: id must not be null",
            None,
        ));
    }
    if !(id.is_string() || id.is_number()) {
        return Accepted::Answered(err(
            Value::Null,
            -32600,
            "Invalid Request: id must be a string or a number",
            None,
        ));
    }

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
        return Accepted::Answered(ok(
            id,
            json!({
                "protocolVersion": protocol_version,
                "capabilities": capabilities(),
                "serverInfo": server_info()
            }),
        ));
    }

    let modern = is_modern_request(&req.params);
    if modern {
        *era = Era::Modern;
        let version = requested_version(&req.params).unwrap_or("");
        if version != PROTOCOL_VERSION {
            return Accepted::Answered(unsupported_version(id, version));
        }
        if !has_client_capabilities(&req.params) {
            return Accepted::Answered(missing_meta(
                id,
                "io.modelcontextprotocol/clientCapabilities",
            ));
        }
    } else if *era != Era::Legacy && req.method != "notifications/initialized" {
        // Modern clients must send _meta on every request. A dual-era process
        // that already answered initialize may receive legacy calls without it.
        return Accepted::Answered(missing_meta(id, "io.modelcontextprotocol/protocolVersion"));
    }

    Accepted::Ready(Ready {
        id,
        modern: modern || *era == Era::Modern,
        method: req.method,
        params: req.params,
    })
}

/// Execute an accepted request. This is the half that reads git and SQLite, so
/// the transport runs it under a deadline and a panic guard.
pub fn run(ready: Ready) -> JsonRpcResponse {
    let Ready {
        id,
        modern,
        method,
        params,
    } = ready;
    let params = &params;

    let paged = |key: &str, items: Vec<Value>, list: &'static str, ttl: u64| {
        list_result(modern, key, items, params, list, ttl)
    };

    match method.as_str() {
        "server/discover" => ok(id, discover_result(modern)),
        "notifications/initialized" => ok(id, envelope(modern, json!({}))),
        // Not a 2026-07-28 method, but 2025-11-25 and earlier have it and some
        // legacy clients use it as a liveness check. Answering it in the legacy
        // era only keeps those working without inventing a modern method.
        "ping" if !modern => ok(id, json!({})),
        "tools/list" => match paged("tools", tools(), "tools", TOOLS_TTL_MS) {
            Ok(result) => ok(id, result),
            Err(error) => invalid_params(id, error.message()),
        },
        "resources/list" => match paged("resources", resources::list(), "resources", LIST_TTL_MS) {
            Ok(result) => ok(id, result),
            Err(error) => invalid_params(id, error.message()),
        },
        "resources/templates/list" => match paged(
            "resourceTemplates",
            resources::templates(),
            "resources:templates",
            LIST_TTL_MS,
        ) {
            Ok(result) => ok(id, result),
            Err(error) => invalid_params(id, error.message()),
        },
        "prompts/list" => match paged("prompts", prompts::list(), "prompts", LIST_TTL_MS) {
            Ok(result) => ok(id, result),
            Err(error) => invalid_params(id, error.message()),
        },
        "resources/read" => match params.get("uri").and_then(Value::as_str) {
            None => invalid_params(id, "Invalid params: uri must be a string"),
            Some(target) => match resources::read(target) {
                Ok(contents) => ok(id, envelope(modern, json!({ "contents": contents }))),
                // A URI this server does not address is the caller's to fix; a
                // backend that broke is not. Collapsing them into one code
                // would send a client to rewrite a URI that was correct.
                Err(resources::ReadError::NotFound(message)) => {
                    err(id, -32602, message, Some(json!({ "uri": target })))
                }
                Err(resources::ReadError::Internal(message)) => {
                    err(id, -32603, message, Some(json!({ "uri": target })))
                }
            },
        },
        "prompts/get" => match params.get("name").and_then(Value::as_str) {
            None => invalid_params(id, "Invalid params: name must be a string"),
            Some(name) => {
                let arguments = params.get("arguments").cloned().unwrap_or(json!({}));
                match prompts::get(name, &arguments) {
                    Ok(result) => ok(id, envelope(modern, result)),
                    Err(error) => invalid_params(id, error.message()),
                }
            }
        },
        "completion/complete" => match complete::complete(params) {
            Ok(result) => ok(id, envelope(modern, result)),
            Err(error) => invalid_params(id, error.message()),
        },
        "tools/call" => {
            let name = params["name"].as_str().unwrap_or("");
            let arguments = params.get("arguments").cloned().unwrap_or(json!({}));
            // Arguments that are present but not an object are malformed against
            // CallToolRequest itself, which the spec classes as a protocol error.
            if !arguments.is_object() {
                return invalid_params(
                    id,
                    format!(
                        "Invalid params: arguments must be an object, got {}",
                        if arguments.is_null() {
                            "null"
                        } else {
                            "a non-object"
                        }
                    ),
                );
            }
            match handle_tool_call(name, &arguments) {
                Ok(payload) => ok(id, complete_tool_result(modern, payload)),
                // Unknown tool is a protocol error: no argument the model could
                // change would make the call succeed.
                Err(message) if message.starts_with("Unknown tool:") => {
                    err(id, -32602, message, None)
                }
                // Everything else — including argument validation — is a tool
                // execution error, which the spec asks be returned in-band so
                // the model can correct itself and retry.
                Err(message) => ok(id, tool_exec_error(modern, message)),
            }
        }
        _ => err(id, -32601, format!("Method not found: {method}"), None),
    }
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

    fn modern_call(method: &str, mut params: Value) -> Value {
        params["_meta"] = modern_meta();
        call(method, params).result.expect("result")
    }

    /// Every modern request method this server answers, with params that reach
    /// a success path.
    fn modern_methods() -> Vec<(&'static str, Value)> {
        vec![
            ("server/discover", json!({})),
            ("tools/list", json!({})),
            ("resources/list", json!({})),
            ("resources/templates/list", json!({})),
            ("prompts/list", json!({})),
            (
                "resources/read",
                json!({ "uri": "gitpulse://server/manifest" }),
            ),
            (
                "prompts/get",
                json!({ "name": "gitpulse_preflight", "arguments": { "repo_path": "/tmp" } }),
            ),
            (
                "completion/complete",
                json!({
                    "ref": { "type": "ref/prompt", "name": "gitpulse_preflight" },
                    "argument": { "name": "repo_path", "value": "" }
                }),
            ),
            (
                "tools/call",
                json!({ "name": "gitpulse_insights", "arguments": { "repo_path": "/tmp" } }),
            ),
        ]
    }

    #[test]
    fn every_modern_result_carries_result_type_and_server_info() {
        // `resultType` is a MUST on every modern result, not only on the ones
        // that happened to be written with it. Driving the whole method list is
        // what stops a newly added method from shipping without it.
        for (method, params) in modern_methods() {
            let result = modern_call(method, params);
            assert_eq!(result["resultType"], "complete", "{method}");
            assert_eq!(
                result["_meta"]["io.modelcontextprotocol/serverInfo"]["name"], SERVER_NAME,
                "{method}"
            );
        }
    }

    #[test]
    fn discover_is_implemented_and_cacheable() {
        let result = modern_call("server/discover", json!({}));
        assert_eq!(result["supportedVersions"], json!([PROTOCOL_VERSION]));
        assert_eq!(result["ttlMs"], DISCOVER_TTL_MS);
        assert_eq!(result["cacheScope"], "public");
        assert!(result["instructions"]
            .as_str()
            .unwrap()
            .contains("gitpulse_insights"));
    }

    #[test]
    fn discover_advertises_every_capability_the_server_actually_answers() {
        // The drift this catches is a capability declared and not implemented,
        // which a client discovers as a -32601 after it has already committed
        // to using the feature.
        let result = modern_call("server/discover", json!({}));
        let declared = result["capabilities"].as_object().expect("capabilities");
        let probes: std::collections::BTreeMap<&str, &str> = [
            ("tools", "tools/list"),
            ("resources", "resources/list"),
            ("prompts", "prompts/list"),
            ("completions", "completion/complete"),
        ]
        .into_iter()
        .collect();
        for capability in declared.keys() {
            let method = probes
                .get(capability.as_str())
                .unwrap_or_else(|| panic!("{capability} is declared with no probe in this test"));
            let mut params = modern_methods()
                .into_iter()
                .find(|(name, _)| name == method)
                .expect("probe params")
                .1;
            params["_meta"] = modern_meta();
            let response = call(method, params);
            assert!(
                response.error.is_none(),
                "{capability} is declared but {method} answered {:?}",
                response.error
            );
        }
        assert_eq!(declared.len(), probes.len());
    }

    #[test]
    fn modern_tools_list_is_cacheable_and_ordered() {
        let result = modern_call("tools/list", json!({}));
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
    fn missing_client_capabilities_is_invalid_params() {
        // Required on every request; the spec names -32602 for a missing one.
        let mut meta = modern_meta();
        meta.as_object_mut()
            .unwrap()
            .remove("io.modelcontextprotocol/clientCapabilities");
        let resp = call("tools/list", json!({ "_meta": meta }));
        let error = resp.error.expect("error");
        assert_eq!(error["code"], -32602);
        assert!(error["message"]
            .as_str()
            .unwrap()
            .contains("clientCapabilities"));
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
    fn a_wrong_jsonrpc_version_is_an_invalid_request() {
        let mut era = Era::Unknown;
        let mut req = request(
            "tools/list",
            Some(json!(1)),
            json!({ "_meta": modern_meta() }),
        );
        req.jsonrpc = "1.0".into();
        let resp = process_request(req, &mut era).expect("answered");
        assert_eq!(resp.error.expect("error")["code"], -32600);
    }

    #[test]
    fn a_null_or_non_scalar_id_is_an_invalid_request_not_a_silent_drop() {
        // A request the server neither answers nor rejects leaves the client
        // waiting on a response that will never come.
        let mut era = Era::Unknown;
        for id in [json!([1]), json!({ "a": 1 }), json!(true)] {
            let resp = process_request(
                request(
                    "tools/list",
                    Some(id.clone()),
                    json!({ "_meta": modern_meta() }),
                ),
                &mut era,
            )
            .expect("answered");
            assert_eq!(resp.error.expect("error")["code"], -32600, "{id}");
            assert_eq!(resp.id, Value::Null);
        }
    }

    #[test]
    fn an_explicit_null_id_survives_deserialization_as_a_present_id() {
        // Serde's stock `Option` maps JSON null to `None`, which is also how an
        // absent field arrives — so `"id": null` was indistinguishable from a
        // notification and the server answered neither. Only a round trip
        // through `from_str` can catch this; a hand-built struct cannot.
        let absent: JsonRpcRequest =
            serde_json::from_str(r#"{"jsonrpc":"2.0","method":"tools/list"}"#).expect("parses");
        assert!(absent.id.is_none(), "an absent id is a notification");

        let null: JsonRpcRequest =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":null,"method":"tools/list"}"#)
                .expect("parses");
        assert_eq!(null.id, Some(Value::Null), "an explicit null id is present");

        let mut era = Era::Unknown;
        let resp = process_request(null, &mut era).expect("a null id is answered, not dropped");
        assert_eq!(resp.error.expect("error")["code"], -32600);
    }

    #[test]
    fn a_notification_gets_no_response() {
        let mut era = Era::Unknown;
        for method in [
            "notifications/initialized",
            "notifications/cancelled",
            "notifications/progress",
        ] {
            assert!(
                process_request(request(method, None, json!({})), &mut era).is_none(),
                "{method}"
            );
        }
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
        let result = list.result.unwrap();
        assert!(result["tools"].is_array());
        // Modern-only fields must not leak into a legacy result.
        for modern_only in ["resultType", "ttlMs", "cacheScope", "_meta"] {
            assert!(result.get(modern_only).is_none(), "{modern_only} leaked");
        }
    }

    #[test]
    fn the_legacy_handshake_advertises_the_same_capabilities_as_discover() {
        let mut era = Era::Unknown;
        let resp = process_request(
            request(
                "initialize",
                Some(json!(1)),
                json!({ "protocolVersion": "2025-11-25" }),
            ),
            &mut era,
        )
        .expect("answered");
        assert_eq!(resp.result.unwrap()["capabilities"], capabilities());
    }

    #[test]
    fn a_legacy_client_can_reach_every_capability_too() {
        let mut era = Era::Unknown;
        process_request(request("initialize", Some(json!(0)), json!({})), &mut era);
        for (method, params) in modern_methods() {
            let resp = process_request(request(method, Some(json!(1)), params), &mut era)
                .expect("answered");
            assert!(resp.error.is_none(), "{method}: {:?}", resp.error);
            // The legacy shape carries no modern envelope.
            assert!(resp.result.unwrap().get("resultType").is_none(), "{method}");
        }
    }

    #[test]
    fn ping_answers_a_legacy_client_and_is_unknown_to_a_modern_one() {
        let mut era = Era::Unknown;
        process_request(request("initialize", Some(json!(0)), json!({})), &mut era);
        assert!(
            process_request(request("ping", Some(json!(1)), json!({})), &mut era)
                .expect("answered")
                .error
                .is_none()
        );
        // `ping` is not a 2026-07-28 method; answering it there would be inventing one.
        let resp = call("ping", json!({ "_meta": modern_meta() }));
        assert_eq!(resp.error.expect("error")["code"], -32601);
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
        let result = modern_call(
            "tools/call",
            json!({ "name": "gitpulse_insights", "arguments": {} }),
        );
        assert_eq!(result["isError"], true);
        assert_eq!(result["resultType"], "complete");
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("repo_path"), "{text}");
    }

    #[test]
    fn a_wrong_typed_argument_is_refused_rather_than_silently_defaulted() {
        // The regression: `"500"` reached `as_u64()`, produced None, and became
        // the default of 200 — a request answered with different work than it
        // asked for, reported as success.
        let result = modern_call(
            "tools/call",
            json!({
                "name": "gitpulse_active_changes",
                "arguments": { "repo_path": "/tmp", "limit": "500" }
            }),
        );
        assert_eq!(result["isError"], true);
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("limit"), "{text}");
        assert!(text.contains("integer"), "{text}");
    }

    #[test]
    fn an_out_of_range_argument_is_refused_at_the_boundary() {
        for (tool, arguments, needle) in [
            (
                "gitpulse_active_changes",
                json!({ "repo_path": "/tmp", "limit": 100_000 }),
                "500",
            ),
            (
                "gitpulse_ledger_events",
                json!({ "repo_path": "/tmp", "limit": 0 }),
                "1",
            ),
            (
                "gitpulse_codeintel_search",
                json!({ "repo_path": "/tmp", "query": "x", "budget": -5 }),
                "1",
            ),
        ] {
            let result = modern_call(
                "tools/call",
                json!({ "name": tool, "arguments": arguments }),
            );
            assert_eq!(result["isError"], true, "{tool}");
            let text = result["content"][0]["text"].as_str().unwrap();
            assert!(text.contains(needle), "{tool}: {text}");
        }
    }

    #[test]
    fn an_unknown_argument_is_refused_because_the_schema_says_it_is_closed() {
        let result = modern_call(
            "tools/call",
            json!({
                "name": "gitpulse_insights",
                "arguments": { "repo_path": "/tmp", "repo": "/tmp" }
            }),
        );
        assert_eq!(result["isError"], true);
        assert!(result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("unknown argument"));
    }

    #[test]
    fn non_object_tool_arguments_are_a_protocol_error() {
        for arguments in [json!("repo_path=/tmp"), json!([1, 2]), json!(7)] {
            let resp = call(
                "tools/call",
                json!({
                    "name": "gitpulse_insights",
                    "arguments": arguments,
                    "_meta": modern_meta()
                }),
            );
            assert_eq!(resp.error.expect("error")["code"], -32602, "{arguments}");
        }
    }

    #[test]
    fn gitpulse_status_refuses_an_invalid_repository_instead_of_creating_state() {
        let error = handle_tool_call("gitpulse_status", &json!({ "repo_path": "/no/such/repo" }))
            .expect_err("an MCP read must validate its repository boundary");
        assert!(error.contains("invalid_worktree"), "{error}");
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
    fn every_tool_schema_is_one_the_validator_actually_understands() {
        // Without this, a schema could grow a keyword `validate` ignores and the
        // tool would look validated while that constraint was never applied.
        for tool in tools() {
            let name = tool["name"].as_str().unwrap();
            for (which, schema) in [
                ("inputSchema", &tool["inputSchema"]),
                ("outputSchema", &tool["outputSchema"]),
            ] {
                assert_eq!(
                    validate::unsupported_keywords(schema),
                    Vec::<String>::new(),
                    "{name}.{which} uses a keyword the validator ignores"
                );
            }
        }
    }

    #[test]
    fn every_tool_declares_an_output_schema_and_bounds_every_numeric_argument() {
        for tool in tools() {
            let name = tool["name"].as_str().unwrap();
            assert_eq!(
                tool["outputSchema"]["type"], "object",
                "{name} declares no outputSchema"
            );
            let properties = tool["inputSchema"]["properties"].as_object().unwrap();
            for (argument, schema) in properties {
                match schema["type"].as_str() {
                    // An unbounded integer argument is how `limit: 4294967295`
                    // becomes a single unbounded read.
                    Some("integer" | "number") => {
                        assert!(
                            schema["minimum"].is_number() && schema["maximum"].is_number(),
                            "{name}.{argument} is numeric with no bounds"
                        );
                    }
                    Some("string") => {
                        assert!(
                            schema["maxLength"].is_number(),
                            "{name}.{argument} is a string with no maxLength"
                        );
                    }
                    other => panic!("{name}.{argument} has unexpected type {other:?}"),
                }
            }
        }
    }

    #[test]
    fn no_tool_annotated_read_only_writes_anything_into_the_repository() {
        // `readOnlyHint: true, destructiveHint: false` is a claim MCP clients
        // gate user approval on. It was false: `gitpulse_insights`,
        // `gitpulse_status`, `gitpulse_ledger_events` and
        // `gitpulse_change_context` each created `.devcouncil/ledger.sqlite`
        // (plus its `-wal` and `-shm`) in a fresh checkout, because the ledger
        // is probed by opening it and opening creates it — and
        // `repository_status` additionally ran a legacy row migration.
        //
        // Derived from the catalog rather than a hand-written list, so a tool
        // added later is covered the moment it exists.
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().to_string_lossy().into_owned();
        let init = std::process::Command::new("git")
            .args(["init", "-q", &repo])
            .output()
            .expect("git init");
        assert!(init.status.success(), "git init failed");

        let before: std::collections::BTreeSet<String> = std::fs::read_dir(dir.path())
            .expect("read dir")
            .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
            .collect();

        for tool in tools() {
            let name = tool["name"].as_str().expect("name");
            assert_eq!(
                tool["annotations"]["readOnlyHint"], true,
                "{name} is not annotated read-only"
            );
            // Every tool takes repo_path; the ones with more required arguments
            // are exercised with placeholders, because the point is the side
            // effect, not the answer.
            let mut arguments = json!({ "repo_path": repo });
            for extra in ["query", "target", "file_path", "from", "to", "commit_sha"] {
                if tool["inputSchema"]["properties"][extra].is_object() {
                    arguments[extra] = json!("probe");
                }
            }
            let _ = handle_tool_call(name, &arguments);

            let after: std::collections::BTreeSet<String> = std::fs::read_dir(dir.path())
                .expect("read dir")
                .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
                .collect();
            let created: Vec<&String> = after.difference(&before).collect();
            assert!(
                created.is_empty(),
                "{name} is annotated read-only but created {created:?} in the repository"
            );
        }
    }

    #[test]
    fn ledger_events_can_reach_recent_history_not_only_the_oldest_page() {
        // `tail` pages forward from a cursor by design. This tool pinned that
        // cursor at 0 and exposed no way to move it, so it always returned the
        // oldest events ever recorded — for a tool described as reading
        // history — with no way to reach recent ones.
        let tool = tools()
            .into_iter()
            .find(|t| t["name"] == "gitpulse_ledger_events")
            .expect("tool");
        let cursor = &tool["inputSchema"]["properties"]["cursor"];
        assert_eq!(cursor["type"], "integer", "no cursor argument");
        assert_eq!(cursor["minimum"], 0);
        // And the result has to hand back the cursor to continue with,
        // otherwise a caller has to reach into the rows to find it.
        let required: Vec<&str> = tool["outputSchema"]["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert!(required.contains(&"truncated"), "{required:?}");
        assert!(tool["outputSchema"]["properties"]["next_cursor"].is_object());
        // `total` used to be the returned count wearing the name of the
        // history size. It must not come back.
        assert!(tool["outputSchema"]["properties"]["total"].is_null());
    }

    #[test]
    fn tool_names_are_unique() {
        let mut seen = std::collections::BTreeSet::new();
        for tool in tools() {
            assert!(seen.insert(tool["name"].as_str().unwrap().to_string()));
        }
    }

    #[test]
    fn tool_names_satisfy_the_specs_character_and_length_rules() {
        for tool in tools() {
            let name = tool["name"].as_str().unwrap();
            assert!(
                (1..=128).contains(&name.len()),
                "{name} is {} chars",
                name.len()
            );
            assert!(
                name.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')),
                "{name} has a character the spec does not allow"
            );
        }
    }

    #[test]
    fn an_invalid_cursor_is_invalid_params_on_every_paginated_list() {
        for method in [
            "tools/list",
            "resources/list",
            "resources/templates/list",
            "prompts/list",
        ] {
            let resp = call(
                method,
                json!({ "cursor": "not-a-cursor", "_meta": modern_meta() }),
            );
            assert_eq!(resp.error.expect("error")["code"], -32602, "{method}");
        }
    }

    #[test]
    fn a_non_string_cursor_is_refused_rather_than_restarting_the_listing() {
        let resp = call("tools/list", json!({ "cursor": 7, "_meta": modern_meta() }));
        assert_eq!(resp.error.expect("error")["code"], -32602);
    }

    #[test]
    fn resources_read_distinguishes_a_bad_uri_from_a_broken_backend() {
        // -32602 tells the client to fix its URI; -32603 tells it not to bother.
        let bad = call(
            "resources/read",
            json!({ "uri": "gitpulse://nope/tmp", "_meta": modern_meta() }),
        );
        assert_eq!(bad.error.expect("error")["code"], -32602);

        let broken = call(
            "resources/read",
            json!({ "uri": "gitpulse://status/no/such/repo", "_meta": modern_meta() }),
        );
        assert_eq!(broken.error.expect("error")["code"], -32603);
    }

    #[test]
    fn resources_read_never_answers_a_missing_resource_with_empty_contents() {
        let resp = call(
            "resources/read",
            json!({ "uri": "gitpulse://server/nope", "_meta": modern_meta() }),
        );
        assert!(
            resp.result.is_none(),
            "a missing resource returned a result"
        );
        assert_eq!(resp.error.expect("error")["code"], -32602);
    }

    #[test]
    fn every_listed_resource_and_template_is_reachable_through_the_wire() {
        let listed = modern_call("resources/list", json!({}));
        for resource in listed["resources"].as_array().unwrap() {
            let target = resource["uri"].as_str().unwrap();
            let read = call(
                "resources/read",
                json!({ "uri": target, "_meta": modern_meta() }),
            );
            assert!(read.error.is_none(), "{target}: {:?}", read.error);
            let contents = read.result.unwrap()["contents"].clone();
            assert_eq!(contents.as_array().unwrap().len(), 1, "{target}");
        }
        let templates = modern_call("resources/templates/list", json!({}));
        assert!(!templates["resourceTemplates"]
            .as_array()
            .unwrap()
            .is_empty());
    }

    #[test]
    fn a_prompt_resolves_over_the_wire_with_its_arguments_enforced() {
        let resolved = modern_call(
            "prompts/get",
            json!({ "name": "gitpulse_preflight", "arguments": { "repo_path": "/tmp" } }),
        );
        assert!(!resolved["messages"].as_array().unwrap().is_empty());

        let missing = call(
            "prompts/get",
            json!({ "name": "gitpulse_preflight", "_meta": modern_meta() }),
        );
        assert_eq!(missing.error.expect("error")["code"], -32602);
    }

    #[test]
    fn malformed_method_params_are_invalid_params_not_internal_errors() {
        for (method, params) in [
            ("resources/read", json!({})),
            ("resources/read", json!({ "uri": 7 })),
            ("prompts/get", json!({})),
            ("prompts/get", json!({ "name": 7 })),
            ("completion/complete", json!({})),
        ] {
            let mut params = params;
            params["_meta"] = modern_meta();
            let resp = call(method, params.clone());
            assert_eq!(
                resp.error.expect("error")["code"],
                -32602,
                "{method} {params}"
            );
        }
    }

    #[test]
    fn an_unknown_method_is_method_not_found() {
        let resp = call("tools/nope", json!({ "_meta": modern_meta() }));
        assert_eq!(resp.error.expect("error")["code"], -32601);
    }

    #[test]
    fn no_response_ever_carries_both_a_result_and_an_error() {
        for (method, params) in modern_methods() {
            let mut params = params;
            params["_meta"] = modern_meta();
            let resp = call(method, params);
            assert!(
                resp.result.is_some() ^ resp.error.is_some(),
                "{method} answered with both or neither"
            );
        }
    }

    #[test]
    fn every_response_echoes_the_request_id() {
        for (method, params) in modern_methods() {
            let mut params = params;
            params["_meta"] = modern_meta();
            let mut era = Era::Unknown;
            let resp = process_request(request(method, Some(json!("abc-123")), params), &mut era)
                .expect("answered");
            assert_eq!(resp.id, json!("abc-123"), "{method}");
        }
    }
}
