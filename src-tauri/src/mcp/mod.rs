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
//! Nothing here writes git. The only writes are the task-board tools
//! (`gitpulse_add_task`, `gitpulse_import_tasks`, `gitpulse_complete_task`,
//! `gitpulse_delete_task`), which write the GitPulse task profile behind the
//! repository trust gate. `gitpulse_delete_task` is the one destructive tool:
//! a soft delete, annotated `destructiveHint: true`.
//! Any tool that would run a git mutation must go through
//! `harness::guard_command`, and every writing tool must be added to the
//! allowlist in `no_advertised_tool_offers_an_ungated_mutation`.

pub mod complete;
pub mod devmap_parity;
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
pub(crate) const MAX_ARG_CHARS: u32 = 4_096;

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

pub(crate) fn tool(
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

fn mutating_tool_annotations(idempotent: bool) -> Value {
    json!({
        "readOnlyHint": false,
        "destructiveHint": false,
        "idempotentHint": idempotent,
        "openWorldHint": false
    })
}

pub(crate) fn mutating_tool(
    name: &str,
    title: &str,
    description: &str,
    properties: Value,
    required: &[&str],
    output: Value,
    idempotent: bool,
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
        "annotations": mutating_tool_annotations(idempotent)
    })
}

/// A mutating tool that removes something. MCP clients gate their own
/// approval prompt on `destructiveHint`, so it must say so.
fn destructive_tool(
    name: &str,
    title: &str,
    description: &str,
    properties: Value,
    required: &[&str],
    output: Value,
) -> Value {
    let mut tool = mutating_tool(name, title, description, properties, required, output, true);
    tool["annotations"]["destructiveHint"] = json!(true);
    tool
}

pub(crate) fn bounded_string_prop(description: &str, min_len: usize, max_len: usize) -> Value {
    let mut obj = json!({
        "type": "string",
        "description": description,
        "maxLength": max_len
    });
    if min_len > 0 {
        obj["minLength"] = json!(min_len);
    }
    obj
}

pub(crate) fn string_array_prop(description: &str, max_items: usize, max_item_len: usize) -> Value {
    json!({
        "type": "array",
        "description": description,
        "items": {
            "type": "string",
            "maxLength": max_item_len
        },
        "maxItems": max_items
    })
}

pub(crate) fn repo_prop() -> Value {
    json!({
        "type": "string",
        "description": "Absolute path to a git repository",
        "minLength": 1,
        "maxLength": MAX_ARG_CHARS
    })
}

pub(crate) fn path_prop(description: &str) -> Value {
    json!({
        "type": "string",
        "description": description,
        "minLength": 1,
        "maxLength": MAX_ARG_CHARS
    })
}

pub(crate) fn budget_prop() -> Value {
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
            "source_freshness": {
                "type": "object",
                "description": "Whole-tree freshness for the answered generation. fresh=null plus reason means the check did not run.",
                "properties": {
                    "fresh": { "type": ["boolean", "null"] },
                    "generation_id": { "type": ["integer", "null"] },
                    "reason": { "type": ["string", "null"] }
                },
                "required": ["fresh"]
            },
            "reason": { "type": ["string", "null"] },
            "items": { "type": "array" },
            "total": { "type": "integer" },
            "shown": { "type": "integer" },
            "truncated": { "type": "boolean" }
        },
        "required": ["available", "source_freshness", "items", "total", "shown", "truncated"]
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
    let mut tools = vec![
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
            "Read task leases plus evidence, gaps, and verification runs from dc-store. Never acquires a lease.",
            json!({
                "repo_path": repo_prop(),
                "task_id": path_prop("Optional task id to filter evidence, gaps, and runs")
            }),
            &["repo_path"],
            json!({ "type": "object" }),
        ),
        mutating_tool(
            "gitpulse_add_task",
            "Add task",
            "File a task on the GitPulse task board for this repository — the board a person sees and launches agents from. Prefer fewer, larger tasks: before filing, call gitpulse_list_tasks and fold related work into one task (one coherent concern each) rather than one task per finding. A new task is refused with related_tasks_exist while open tasks look like the same work; the refusal lists them. Then either fold yours into one (call again with that task's item_id as task_id and overwrite: true, carrying its content plus yours), or, if it is genuinely separate, pass reviewed_related with every listed item_id. Requires the repository to be trusted in GitPulse. task_id is the stable key: adding the same task_id again is refused unless overwrite is true, so a retry never makes a duplicate; overwrite also accepts a board item_id. Planned files are kept as a section of the description. Recorded in the board's event history.",
            json!({
                "repo_path": repo_prop(),
                "title": bounded_string_prop("Task title: one line, at most 300 characters", 1, 1200),
                "description": bounded_string_prop("Background and problem statement (Markdown)", 0, crate::tasks::file_tasks::MAX_TASK_DESCRIPTION),
                "task_id": bounded_string_prop("Stable key for this task (letters, digits, '-', '_', '.'), e.g. 'gp-auth-flow'. Defaults to 'gp-' plus the title's slug.", 1, crate::tasks::file_tasks::MAX_TASK_KEY),
                "status": { "type": "string", "enum": crate::tasks::file_tasks::STATUSES, "maxLength": 32, "description": "Board column (default inbox)" },
                "priority": {
                    "type": "integer",
                    "description": "Priority: 0 (urgent), 1 (high), 2 (normal, default), 3 (low)",
                    "minimum": 0,
                    "maximum": 3
                },
                "severity": { "type": "string", "enum": ["none", "low", "medium", "high", "critical"], "maxLength": 32, "description": "Severity (default none)" },
                "kind": bounded_string_prop("Type, e.g. feature (default), bug, refactor, chore, spike", 1, crate::tasks::file_tasks::MAX_KIND_BYTES),
                "owner": bounded_string_prop("Assignee, e.g. '@alice'", 1, crate::tasks::file_tasks::MAX_OWNER_BYTES),
                "due": bounded_string_prop("Due date as YYYY-MM-DD, or Unix seconds", 1, 32),
                "labels": string_array_prop("Labels", crate::tasks::file_tasks::MAX_TASK_LABELS, crate::tasks::file_tasks::MAX_LABEL_BYTES),
                "repositories": string_array_prop("Other related repository names; kept as a section of the description", crate::tasks::file_tasks::MAX_TASK_REPOSITORIES, crate::tasks::file_tasks::MAX_REPOSITORY_BYTES),
                "planned_files": string_array_prop("Files the work is expected to touch; kept as a section of the description", crate::tasks::file_tasks::MAX_TASK_PLANNED_FILES, crate::tasks::file_tasks::MAX_PLANNED_FILE_BYTES),
                "acceptance_criteria": string_array_prop("Definition of done, one item each", crate::tasks::file_tasks::MAX_TASK_CRITERIA, crate::tasks::file_tasks::MAX_CRITERION_BYTES),
                "logs": bounded_string_prop("Raw logs or traces kept verbatim as evidence (up to 256 KiB)", 0, crate::tasks::file_tasks::MAX_TASK_LOGS),
                "overwrite": {
                    "type": "boolean",
                    "description": "Replace the content of the task already filed under this task_id, or of the board task whose item_id is task_id (default false). Its column position and links are kept. This is how related work is folded into an existing task: send its current content plus yours."
                },
                "reviewed_related": string_array_prop("item_ids of the open tasks a related_tasks_exist refusal listed, once you have read them and this task is genuinely separate work", crate::workbench::intake::MAX_RELATED, 128)
            }),
            &["repo_path", "title"],
            json!({
                "type": "object",
                "properties": {
                    "ok": { "type": "boolean" },
                    "outcome": { "type": "string", "enum": ["created", "updated", "unchanged"] },
                    "task_id": { "type": "string" },
                    "item_id": { "type": "string" },
                    "revision": { "type": "integer" },
                    "title": { "type": "string" },
                    "status": { "type": "string" },
                    "priority": { "type": "integer" },
                    "repository": { "type": "object" },
                    "folded_into_description": { "type": "object" },
                    "related_check": { "type": ["object", "null"] }
                },
                "required": ["ok", "outcome", "task_id", "item_id", "title", "status", "priority", "repository"]
            }),
            true,
        ),
        mutating_tool(
            "gitpulse_import_tasks",
            "Import task briefs",
            "Put the Markdown task briefs in the repository's tasks folder onto the GitPulse task board. Requires the repository to be trusted. Idempotent: a brief already on the board is left alone unless replace is true, and a task deleted on the board stays deleted. Every file is accounted for: a brief that cannot be read is reported by name with the reason, and ok is true only when every file was placed.",
            json!({
                "repo_path": repo_prop(),
                "tasks_dir": bounded_string_prop("Tasks directory relative to the repository root (default 'tasks')", 1, crate::tasks::file_tasks::MAX_TASKS_DIR),
                "replace": {
                    "type": "boolean",
                    "description": "Overwrite the board copy of briefs already imported with the file's current content (default false)"
                }
            }),
            &["repo_path"],
            json!({
                "type": "object",
                "properties": {
                    "ok": { "type": "boolean" },
                    "repository": { "type": ["object", "null"] },
                    "tasks_dir": { "type": "string" },
                    "directory_exists": { "type": "boolean" },
                    "found": { "type": "integer" },
                    "read": { "type": "integer" },
                    "truncated": { "type": "boolean" },
                    "created": { "type": "integer" },
                    "updated": { "type": "integer" },
                    "unchanged": { "type": "integer" },
                    "already_present": { "type": "integer" },
                    "deleted_on_board": { "type": "integer" },
                    "invalid": { "type": "integer" },
                    "failed": { "type": "integer" },
                    "entries": { "type": "array" }
                },
                "required": ["ok", "repository", "tasks_dir", "directory_exists", "found", "read", "truncated", "created", "updated", "unchanged", "already_present", "deleted_on_board", "invalid", "failed", "entries"]
            }),
            true,
        ),
        tool(
            "gitpulse_list_tasks",
            "List tasks",
            "List the tasks on the GitPulse task board for this repository, in board order. repository null means no task has ever been filed under it — not that its tasks are done.",
            json!({
                "repo_path": repo_prop(),
                "status": { "type": "string", "enum": crate::tasks::file_tasks::STATUSES, "maxLength": 32, "description": "Only this column" },
                "limit": {
                    "type": "integer",
                    "description": "Maximum tasks to return (default 50, max 200)",
                    "minimum": 1,
                    "maximum": 200
                },
                "cursor": bounded_string_prop("next_cursor from a previous page", 1, 180)
            }),
            &["repo_path"],
            json!({
                "type": "object",
                "properties": {
                    "ok": { "type": "boolean" },
                    "repository": { "type": ["object", "null"] },
                    "returned": { "type": "integer" },
                    "total": { "type": "integer" },
                    "has_more": { "type": "boolean" },
                    "next_cursor": { "type": ["string", "null"] },
                    "tasks": { "type": "array" }
                },
                "required": ["ok", "repository", "returned", "total", "has_more", "tasks"]
            }),
        ),
        tool(
            "gitpulse_get_task",
            "Get task",
            "Read one task from the GitPulse task board with its canonical agent brief — the same Markdown GitPulse hands an agent it launches. Accepts the task_id it was added with, or an item_id from gitpulse_list_tasks.",
            json!({
                "repo_path": repo_prop(),
                "task_id": bounded_string_prop("The task_id it was added or imported with, or its board item_id", 1, 128)
            }),
            &["repo_path", "task_id"],
            json!({
                "type": "object",
                "properties": {
                    "ok": { "type": "boolean" },
                    "item_id": { "type": "string" },
                    "repository": { "type": "object" },
                    "task": { "type": "object" },
                    "brief": { "type": "string" }
                },
                "required": ["ok", "item_id", "repository", "task", "brief"]
            }),
        ),
        mutating_tool(
            "gitpulse_complete_task",
            "Complete task",
            "Move your GitPulse board task to done when the work is finished (or to review to hand it to a person, or in_progress when you start). Pass the task id from your brief's 'Task:' line, or the task_id it was filed with. Only the status changes, plus an optional summary appended to the task's logs so the person sees what you did; a person's concurrent edit is never overwritten. Call it only when the acceptance criteria are met and your verification passed. Idempotent: the same move twice is reported as unchanged. A task already done is never reopened. Requires the repository to be trusted in GitPulse.",
            json!({
                "repo_path": repo_prop(),
                "task_id": bounded_string_prop("The task id from your brief ('Task: <id>'), its board item_id, or the task_id it was filed with", 1, 128),
                "status": { "type": "string", "enum": crate::workbench::intake::AGENT_STATUSES, "maxLength": 32, "description": "Where the task goes (default done)" },
                "summary": bounded_string_prop("What you changed and how you verified it, in a few lines; appended to the task's logs", 1, crate::workbench::intake::MAX_SUMMARY_CHARS * 4),
                "expected_revision": {
                    "type": "integer",
                    "description": "Only move it if the task is still at this revision (from your brief or gitpulse_get_task); otherwise it is refused so you can re-read it",
                    "minimum": 1,
                    "maximum": 9_007_199_254_740_991_i64
                }
            }),
            &["repo_path", "task_id"],
            json!({
                "type": "object",
                "properties": {
                    "ok": { "type": "boolean" },
                    "outcome": { "type": "string", "enum": ["updated", "unchanged"] },
                    "item_id": { "type": "string" },
                    "title": { "type": "string" },
                    "previous_status": { "type": "string" },
                    "status": { "type": "string" },
                    "revision": { "type": "integer" },
                    "summary_recorded": { "type": "boolean" },
                    "repository": { "type": "object" }
                },
                "required": ["ok", "outcome", "item_id", "previous_status", "status", "revision", "summary_recorded", "repository"]
            }),
            true,
        ),
        destructive_tool(
            "gitpulse_delete_task",
            "Delete task",
            "Delete a task card from the GitPulse task board — the same delete as the board's own Delete. Pass the task_id it was filed with or its board item_id, and a reason: the reason is appended to the task's logs first, so it stays in the task's history. The delete is soft (the row and its history stay in the GitPulse profile and the id is never reused), but there is no undelete over MCP; only the person can bring it back. Use it only for a task that should not exist — a duplicate, or one merged into another — never to finish work (use gitpulse_complete_task). A task linked to other repositories as well is refused (shared_task). Idempotent: a task already deleted is reported as unchanged. With expected_revision, a task that changed since you read it is refused so you can re-read it. Requires the repository to be trusted in GitPulse.",
            json!({
                "repo_path": repo_prop(),
                "task_id": bounded_string_prop("The task_id it was filed with, or its board item_id from gitpulse_list_tasks", 1, 128),
                "reason": bounded_string_prop("Why the task is being deleted, in a line or two; recorded in its history", 1, crate::workbench::intake::MAX_REASON_CHARS * 4),
                "expected_revision": {
                    "type": "integer",
                    "description": "Only delete it if the task is still at this revision (from gitpulse_list_tasks or gitpulse_get_task); otherwise it is refused so you can re-read it",
                    "minimum": 1,
                    "maximum": 9_007_199_254_740_991_i64
                }
            }),
            &["repo_path", "task_id", "reason"],
            json!({
                "type": "object",
                "properties": {
                    "ok": { "type": "boolean" },
                    "outcome": { "type": "string", "enum": ["deleted", "unchanged"] },
                    "item_id": { "type": "string" },
                    "title": { "type": "string" },
                    "status": { "type": "string" },
                    "deleted": { "type": "boolean" },
                    "revision": { "type": "integer" },
                    "reason_recorded": { "type": "boolean" },
                    "repository": { "type": "object" },
                    "sequence": { "type": ["integer", "null"] }
                },
                "required": ["ok", "outcome", "item_id", "status", "deleted", "revision", "reason_recorded", "repository"]
            }),
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
            "gitpulse_codeintel_suspects",
            "Regression suspects",
            "Which commits since `since` could have caused a symptom. Blames only the lines belonging to symbols the symptom depends on, so the answer is the dependency cone rather than the file's recent history. The window ends at the commit the index was built at, never HEAD. An empty list with available=true means nothing in the window touched the cone; refusals are reported in walk_incomplete and make the list a lower bound.",
            json!({
                "repo_path": repo_prop(),
                "symptom": path_prop("Symbol that is misbehaving — a qualified node id, or a bare name"),
                "since": path_prop("Revision the window opens at (a commit, tag or branch)"),
                // Bounded, like every other numeric argument here. The cone
                // grows with depth and each file it reaches costs a `git blame`
                // subprocess, so an unbounded depth is an unbounded amount of
                // work asked for by one integer. Ten is already far past the
                // point where a dependency chain explains a regression.
                "depth": {
                    "type": "integer",
                    "description": "How many call edges out from the symptom to follow (default 3)",
                    "minimum": 1,
                    "maximum": 10
                }
            }),
            &["repo_path", "symptom", "since"],
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
    ];
    tools.extend(devmap_parity::tool_definitions());
    tools
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
                "instructions": "GitPulse control plane. It never mutates git state; its only writes are the task-board tools — gitpulse_add_task, gitpulse_import_tasks, gitpulse_complete_task and gitpulse_delete_task — which a person sees on the GitPulse board. gitpulse_delete_task removes a card (soft, with a required reason kept in its history, and no undelete over MCP); use it only for a task that should not exist, never to finish one. Keep the board small: before gitpulse_add_task, read gitpulse_list_tasks and group related findings into one task, or fold them into an existing one with overwrite; gitpulse_add_task refuses a new task that looks like open work until you have reviewed it. Start with gitpulse_insights for a repository snapshot (worktrees, agent sessions, collisions, ledger, code graph). Use gitpulse_change_context before editing, and gitpulse_collision_risk before parallel agent work. The same views are addressable as gitpulse://<facet>{+repo_path} resources; gitpulse://server/manifest describes the whole surface. Pass absolute repo_path on every call. When GitPulse launched you on a task, your brief names it on its Task: line; when the work is finished and verified, call gitpulse_complete_task with that id and a short summary.",
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

fn parse_string_vec(val: &Value) -> Option<Vec<String>> {
    val.as_array().map(|arr| {
        arr.iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect()
    })
}

/// Open the task board's profile. `create` is for writes; a read of a machine
/// with no board yet answers `None` rather than creating one.
fn open_task_profile(create: bool) -> Result<Option<dc_store::Store>, String> {
    let path = task_profile_path()?;
    if !create && !path.exists() {
        return Ok(None);
    }
    if create {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
        }
        return dc_store::Store::open(&path)
            .map(Some)
            .map_err(|e| e.to_string());
    }
    dc_store::Store::open_existing(&path)
        .map(Some)
        .map_err(|e| e.to_string())
}

#[cfg(not(test))]
fn task_profile_path() -> Result<std::path::PathBuf, String> {
    crate::workbench::intake::default_profile_path().map_err(|e| e.message)
}

#[cfg(test)]
thread_local! {
    static PROFILE_OVERRIDE: std::cell::RefCell<Option<std::path::PathBuf>> = const { std::cell::RefCell::new(None) };
}

/// A test never reaches the person's real board: without an override it fails.
#[cfg(test)]
fn task_profile_path() -> Result<std::path::PathBuf, String> {
    PROFILE_OVERRIDE
        .with(|cell| cell.borrow().clone())
        .ok_or_else(|| {
            "test did not set PROFILE_OVERRIDE; refusing to touch the real task profile".to_string()
        })
}

fn workbench_message(error: crate::workbench::WorkbenchError) -> String {
    format!("{}: {}", error.code, error.message)
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
            let task_id = arguments["task_id"].as_str();
            let view = crate::tasks::view_filtered(&address.anchor, task_id);
            Ok(json!(view))
        }
        "gitpulse_add_task" => {
            let repo = arguments["repo_path"].as_str().ok_or("missing repo_path")?;
            let title = arguments["title"].as_str().ok_or("missing title")?;
            let text = |key: &str| arguments[key].as_str().map(str::to_string);
            let key = match arguments["task_id"].as_str() {
                Some(key) => key.to_string(),
                None => format!("gp-{}", crate::tasks::file_tasks::slugify(title)),
            };
            let task = crate::workbench::intake::ExternalTask {
                key,
                title: title.to_string(),
                description: text("description").unwrap_or_default(),
                status: text("status"),
                priority: arguments["priority"]
                    .as_u64()
                    .map(|p| p.min(u64::from(u8::MAX)) as u8),
                severity: text("severity"),
                kind: text("kind"),
                owner: text("owner"),
                due: text("due"),
                labels: parse_string_vec(&arguments["labels"]).unwrap_or_default(),
                acceptance_criteria: parse_string_vec(&arguments["acceptance_criteria"])
                    .unwrap_or_default(),
                planned_files: parse_string_vec(&arguments["planned_files"]).unwrap_or_default(),
                repositories: parse_string_vec(&arguments["repositories"]).unwrap_or_default(),
                logs: text("logs"),
            };
            let overwrite = arguments["overwrite"].as_bool().unwrap_or(false);
            let reviewed = parse_string_vec(&arguments["reviewed_related"]).unwrap_or_default();
            let store =
                open_task_profile(true)?.ok_or("the GitPulse task profile could not be created")?;
            crate::workbench::intake::add_task(&store, repo, task, overwrite, &reviewed)
                .map_err(workbench_message)
        }
        "gitpulse_import_tasks" => {
            let repo = arguments["repo_path"].as_str().ok_or("missing repo_path")?;
            let replace = arguments["replace"].as_bool().unwrap_or(false);
            let store =
                open_task_profile(true)?.ok_or("the GitPulse task profile could not be created")?;
            crate::workbench::intake::import_briefs(
                &store,
                repo,
                arguments["tasks_dir"].as_str(),
                replace,
            )
            .map(|report| json!(report))
            .map_err(workbench_message)
        }
        "gitpulse_list_tasks" => {
            let repo = arguments["repo_path"].as_str().ok_or("missing repo_path")?;
            let limit = arguments["limit"].as_u64().unwrap_or(50);
            let Some(store) = open_task_profile(false)? else {
                return Ok(json!({
                    "ok": true, "repository": null, "returned": 0, "total": 0, "has_more": false,
                    "next_cursor": null, "tasks": [],
                    "note": "GitPulse has no task board on this machine yet: nothing has been filed. That is not the same as a board whose tasks are all done.",
                }));
            };
            crate::workbench::intake::list_tasks(
                &store,
                repo,
                arguments["status"].as_str(),
                limit,
                arguments["cursor"].as_str(),
            )
            .map_err(workbench_message)
        }
        "gitpulse_get_task" => {
            let repo = arguments["repo_path"].as_str().ok_or("missing repo_path")?;
            let task = arguments["task_id"].as_str().ok_or("missing task_id")?;
            let store = open_task_profile(false)?
                .ok_or("GitPulse has no task board on this machine yet: nothing has been filed.")?;
            crate::workbench::intake::get_task(&store, repo, task).map_err(workbench_message)
        }
        "gitpulse_complete_task" => {
            let repo = arguments["repo_path"].as_str().ok_or("missing repo_path")?;
            let task = arguments["task_id"].as_str().ok_or("missing task_id")?;
            let status = arguments["status"].as_str().unwrap_or("done");
            // Reading, not creating: a task to complete has to exist already.
            let store = open_task_profile(false)?
                .ok_or("GitPulse has no task board on this machine yet: nothing has been filed.")?;
            crate::workbench::intake::complete_task(
                &store,
                repo,
                task,
                status,
                arguments["expected_revision"].as_i64(),
                arguments["summary"].as_str(),
            )
            .map_err(workbench_message)
        }
        "gitpulse_delete_task" => {
            let repo = arguments["repo_path"].as_str().ok_or("missing repo_path")?;
            let task = arguments["task_id"].as_str().ok_or("missing task_id")?;
            let reason = arguments["reason"].as_str().ok_or("missing reason")?;
            // Reading, not creating: a task to delete has to exist already.
            let store = open_task_profile(false)?
                .ok_or("GitPulse has no task board on this machine yet: nothing has been filed.")?;
            crate::workbench::intake::delete_task(
                &store,
                repo,
                task,
                arguments["expected_revision"].as_i64(),
                reason,
            )
            .map_err(workbench_message)
        }
        name if name.starts_with("devmap_") => devmap_parity::handle(name, arguments),
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
        "gitpulse_codeintel_suspects" => {
            let repo = arguments["repo_path"].as_str().ok_or("missing repo_path")?;
            let symptom = arguments["symptom"].as_str().ok_or("missing symptom")?;
            let since = arguments["since"].as_str().ok_or("missing since")?;
            let depth = arguments["depth"].as_u64().map(|d| d as u32);
            Ok(json!(crate::codeintel::suspects(
                repo, symptom, since, depth
            )))
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
        let mut dispatched: std::collections::BTreeSet<String> = source
            .lines()
            .filter_map(|line| {
                let t = line.trim();
                let rest = t.strip_prefix('"')?;
                let (name, tail) = rest.split_once('"')?;
                if !tail.trim_start().starts_with("=>")
                    || !(name.starts_with("gitpulse_") || name.starts_with("devmap_"))
                {
                    return None;
                }
                Some(name.to_string())
            })
            .collect();
        // DevMap-parity tools share one prefix arm; count them as dispatched.
        if source.contains("name if name.starts_with(\"devmap_\")") {
            for name in devmap_parity::TOOL_NAMES {
                dispatched.insert((*name).to_string());
            }
        }
        assert!(
            advertised.len() >= 8,
            "scan found only {}",
            advertised.len()
        );
        assert_eq!(advertised, dispatched);
    }

    #[test]
    fn tools_list_covers_every_devmap_tool_name() {
        let listed: std::collections::BTreeSet<String> = tools()
            .into_iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect();
        for name in devmap_parity::TOOL_NAMES {
            assert!(
                listed.contains(*name),
                "tools/list is missing {name}; parity with devmap_serve::mcp::TOOL_NAMES"
            );
        }
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
        assert!(tool["inputSchema"]["properties"]["task_id"].is_object());
    }

    #[test]
    fn no_advertised_tool_offers_an_ungated_mutation() {
        for tool in tools() {
            let name = tool["name"].as_str().unwrap();
            let read_only = tool["annotations"]["readOnlyHint"]
                .as_bool()
                .unwrap_or(false);
            if !read_only {
                // Mutating tools must be in an explicit allowlist and policy-gated
                assert!(
                    matches!(
                        name,
                        "gitpulse_add_task"
                            | "gitpulse_import_tasks"
                            | "gitpulse_complete_task"
                            | "gitpulse_delete_task"
                    ),
                    "unexpected mutating tool {name}"
                );
                // Deleting a card is the one destructive write, and it must
                // say so: clients gate their approval prompt on this hint.
                assert_eq!(
                    tool["annotations"]["destructiveHint"],
                    name == "gitpulse_delete_task",
                    "{name} has the wrong destructiveHint"
                );
                continue;
            }
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
    fn staleness_audit_navigation_payloads_match_the_advertised_field_types() {
        let schema = codeintel_output();
        for response in [
            crate::codeintel::CodeintelResponse::<Value>::ok(Vec::new(), 0, 0, false),
            crate::codeintel::CodeintelResponse::<Value>::unavailable("source is unavailable"),
        ] {
            let payload = serde_json::to_value(response).unwrap();
            for (name, field_schema) in schema["properties"].as_object().unwrap() {
                let violations = validate::validate(&payload[name], field_schema);
                assert!(violations.is_empty(), "{name}: {violations:?}");
            }
        }
        for (name, wrong_type) in [("reason", json!(42)), ("source_freshness", json!("true"))] {
            assert!(!validate::validate(&wrong_type, &schema["properties"][name]).is_empty());
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
                    Some("array") => {
                        assert!(
                            schema["maxItems"].is_number(),
                            "{name}.{argument} is an array with no maxItems"
                        );
                    }
                    Some("boolean") => {}
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
            if tool["annotations"]["readOnlyHint"] != true {
                continue;
            }
            assert_eq!(
                tool["annotations"]["readOnlyHint"], true,
                "{name} is not annotated read-only"
            );
            // Every tool takes repo_path; the ones with more required arguments
            // are exercised with placeholders, because the point is the side
            // effect, not the answer.
            let mut arguments = json!({ "repo_path": repo });
            for extra in [
                "query",
                "target",
                "file_path",
                "from",
                "to",
                "commit_sha",
                "task_id",
            ] {
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

    fn tool_json(name: &str, arguments: Value) -> (bool, Value) {
        let res = modern_call(
            "tools/call",
            json!({ "name": name, "arguments": arguments }),
        );
        let text = res["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let is_error = res["isError"].as_bool().unwrap_or(false);
        let body = serde_json::from_str(&text).unwrap_or(Value::String(text));
        (is_error, body)
    }

    fn task_fixture() -> (tempfile::TempDir, tempfile::TempDir, String) {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().to_string_lossy().into_owned();
        let init = std::process::Command::new("git")
            .args(["init", "-q", &repo])
            .output()
            .expect("git init");
        assert!(init.status.success(), "git init failed");
        let profile = tempfile::tempdir().expect("profile dir");
        let path = profile.path().join("workbench.sqlite");
        PROFILE_OVERRIDE.with(|cell| *cell.borrow_mut() = Some(path));
        (dir, profile, repo)
    }

    /// The defect this replaced: `gitpulse_add_task` reported success and the
    /// task appeared on no board. So the assertion is made through the exact
    /// calls the board makes — `registerRepository`, then a repository-scoped
    /// `items.list` — on the same profile, not through the tool's own echo.
    #[test]
    fn an_added_task_is_on_the_board_the_person_sees() {
        let (dir, profile, repo) = task_fixture();
        crate::test_support::trust_repo(dir.path());

        let (error, added) = tool_json(
            "gitpulse_add_task",
            json!({
                "repo_path": repo,
                "title": "Build authentication flow",
                "description": "Implement OAuth2 and passkey support.",
                "status": "in_progress",
                "priority": 1,
                "severity": "high",
                "kind": "feature",
                "owner": "@alice",
                "due": "2026-04-01",
                "labels": ["auth", "security"],
                "repositories": ["GitPulse"],
                "planned_files": ["src/auth.rs"],
                "acceptance_criteria": ["Support PKCE", "Verify tokens"],
                "logs": "Traceback:\n  File auth.rs line 12"
            }),
        );
        assert!(!error, "add_task failed: {added}");
        assert_eq!(added["outcome"], "created");
        assert_eq!(added["task_id"], "gp-build-authentication-flow");
        assert_eq!(added["folded_into_description"]["planned_files"], 1);
        let (error, _) = tool_json(
            "gitpulse_add_task",
            json!({ "repo_path": repo, "task_id": "gp-fix-critical-leak", "title": "Fix critical memory leak", "status": "ready", "priority": 0 }),
        );
        assert!(!error);

        // What the board does when this repository is opened.
        let board =
            crate::workbench::WorkbenchState::for_profile(&profile.path().join("workbench.sqlite"));
        let registered = board
            .board_register(&repo, "board-new-id", "board-request")
            .unwrap();
        let repository_id = registered["repository"]["id"].as_str().unwrap().to_string();
        assert_eq!(
            repository_id, added["repository"]["id"],
            "the board and the agent share one repository record"
        );
        let page = board
            .board_request(
                "items.list",
                &json!({"repository_id": repository_id, "limit": 50}).to_string(),
            )
            .unwrap();
        let titles: Vec<&str> = page["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["title"].as_str().unwrap())
            .collect();
        assert_eq!(page["total"], 2, "{titles:?}");
        assert!(
            titles.contains(&"Build authentication flow")
                && titles.contains(&"Fix critical memory leak")
        );
        let item = board
            .board_request("items.get", &json!({"id": added["item_id"]}).to_string())
            .unwrap()["item"]
            .clone();
        assert_eq!(item["status"], "in_progress");
        assert_eq!(item["owner"], "@alice");
        assert_eq!(item["severity"], "high");
        assert_eq!(item["due_at"], 1_775_044_800, "2026-04-01 at 12:00 UTC");
        assert_eq!(
            item["acceptance_criteria"],
            json!(["Support PKCE", "Verify tokens"])
        );
        assert_eq!(item["logs"], "Traceback:\n  File auth.rs line 12");
        let description = item["description"].as_str().unwrap();
        assert!(
            description.contains("## Planned files\n- src/auth.rs"),
            "{description}"
        );
        assert!(
            description.contains("## Related repositories\n- GitPulse"),
            "{description}"
        );

        // An agent reads the board back, by its own key.
        let (error, listed) = tool_json(
            "gitpulse_list_tasks",
            json!({ "repo_path": repo, "status": "ready" }),
        );
        assert!(!error, "{listed}");
        assert_eq!(listed["returned"], 1);
        assert_eq!(listed["tasks"][0]["title"], "Fix critical memory leak");
        let (error, fetched) = tool_json(
            "gitpulse_get_task",
            json!({ "repo_path": repo, "task_id": "gp-build-authentication-flow" }),
        );
        assert!(!error, "{fetched}");
        assert!(fetched["brief"]
            .as_str()
            .unwrap()
            .starts_with("# Task brief v1"));
        let (error, by_item) = tool_json(
            "gitpulse_get_task",
            json!({ "repo_path": repo, "task_id": added["item_id"] }),
        );
        assert!(!error && by_item["item_id"] == added["item_id"]);

        // A retry is refused, not duplicated; overwrite replaces content only.
        let (error, duplicate) = tool_json(
            "gitpulse_add_task",
            json!({ "repo_path": repo, "task_id": "gp-fix-critical-leak", "title": "Duplicate" }),
        );
        assert!(
            error && duplicate.as_str().unwrap().contains("already_exists"),
            "{duplicate}"
        );
        let (error, replaced) = tool_json(
            "gitpulse_add_task",
            json!({ "repo_path": repo, "task_id": "gp-fix-critical-leak", "title": "Updated title", "status": "ready", "priority": 0, "overwrite": true }),
        );
        assert!(!error, "{replaced}");
        assert_eq!(replaced["outcome"], "updated");
        let (_, again) = tool_json(
            "gitpulse_add_task",
            json!({ "repo_path": repo, "task_id": "gp-fix-critical-leak", "title": "Updated title", "status": "ready", "priority": 0, "overwrite": true }),
        );
        assert_eq!(again["outcome"], "unchanged", "{again}");
        let page = board
            .board_request(
                "items.list",
                &json!({"repository_id": repository_id, "limit": 50}).to_string(),
            )
            .unwrap();
        assert_eq!(page["total"], 2, "overwrite never adds a card");
    }

    /// The skill is what agents read; it once promised limits, a sort order
    /// and a ledger record the code did not have. Its numbers and tool names
    /// are derived from the code here rather than trusted.
    #[test]
    fn the_tasks_skill_states_the_limits_and_tools_the_code_enforces() {
        use crate::tasks::file_tasks as ft;
        let skill = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../plugins/gitpulse/skills/gitpulse-tasks/SKILL.md"),
        )
        .unwrap();
        let grouped = |n: usize| {
            let digits = n.to_string();
            let mut out = String::new();
            for (i, c) in digits.chars().enumerate() {
                if i > 0 && (digits.len() - i).is_multiple_of(3) {
                    out.push(',');
                }
                out.push(c);
            }
            out
        };
        for claim in [
            format!("at most {} characters", ft::MAX_TASK_TITLE),
            format!("at most {} bytes. Defaults", ft::MAX_TASK_KEY),
            format!(
                "at most {} bytes including folded",
                grouped(ft::MAX_TASK_DESCRIPTION)
            ),
            format!(
                "At most {}, each at most {} bytes",
                ft::MAX_TASK_LABELS,
                ft::MAX_LABEL_BYTES
            ),
            format!(
                "At most {}, each at most {} bytes",
                ft::MAX_TASK_CRITERIA,
                grouped(ft::MAX_CRITERION_BYTES)
            ),
            format!("At most {}. The board", ft::MAX_TASK_PLANNED_FILES),
            format!("at most {} KiB", ft::MAX_TASK_LOGS / 1024),
            format!("over {} MiB", ft::MAX_BRIEF_BYTES / 1024 / 1024),
            format!("at most {} bytes; default", ft::MAX_KIND_BYTES),
            format!("At most {} bytes. |", ft::MAX_OWNER_BYTES),
            format!(
                "At most {} `item_id`s",
                crate::workbench::intake::MAX_RELATED
            ),
            format!(
                "`reason` is required (at most {} characters)",
                grouped(crate::workbench::intake::MAX_REASON_CHARS)
            ),
        ] {
            assert!(skill.contains(&claim), "SKILL.md does not state {claim:?}");
        }
        for status in ft::STATUSES {
            assert!(
                skill.contains(&format!("`{status}`")),
                "SKILL.md omits status {status}"
            );
        }
        let task_tools: Vec<String> = tools()
            .into_iter()
            .filter_map(|t| t["name"].as_str().map(str::to_string))
            .filter(|n| {
                (n.ends_with("_task") || n.ends_with("_tasks")) && n != "gitpulse_task_view"
            })
            .collect();
        assert_eq!(task_tools.len(), 6, "{task_tools:?}");
        for name in &task_tools {
            assert!(
                skill.contains(&format!("| `{name}` |")),
                "SKILL.md table omits {name}"
            );
        }
    }

    /// A task the person made on the board, the way the board makes it: a
    /// random id, linked to the repository the board registered.
    fn board_task(
        profile: &std::path::Path,
        repo: &str,
        id: &str,
    ) -> (crate::workbench::WorkbenchState, String) {
        let board =
            crate::workbench::WorkbenchState::for_profile(&profile.join("workbench.sqlite"));
        let registered = board
            .board_register(repo, "board-repo", "board-request")
            .unwrap();
        let repository_id = registered["repository"]["id"].as_str().unwrap().to_string();
        board
            .board_request(
                "items.put",
                &json!({
                    "id": id, "request_id": format!("put-{id}"), "expected_revision": 0,
                    "title": "Make the importer resumable", "description": "Background",
                    "status": "in_progress", "priority": 1, "severity": "high", "owner": "@sam",
                    "labels": ["import"], "acceptance_criteria": ["Resumes after a crash"],
                    "logs": "first trace",
                    "repository_ids": [repository_id], "primary_repository_id": repository_id,
                    "position": 7
                })
                .to_string(),
            )
            .unwrap();
        (board, repository_id)
    }

    #[test]
    fn a_finished_task_reaches_done_on_the_board_with_only_its_status_changed() {
        let (dir, profile, repo) = task_fixture();
        crate::test_support::trust_repo(dir.path());
        let id = "6f1c2a7e-0d4b-4c1e-9a55-3b0e8f2d9c11";
        let (board, _) = board_task(profile.path(), &repo, id);
        let before = board
            .board_request("items.get", &json!({"id": id}).to_string())
            .unwrap()["item"]
            .clone();

        // The id a launched agent reads off its brief's `Task:` line.
        let (error, brief) = tool_json(
            "gitpulse_get_task",
            json!({"repo_path": repo, "task_id": id}),
        );
        assert!(!error, "{brief}");
        assert!(brief["brief"]
            .as_str()
            .unwrap()
            .contains(&format!("Task: {id} (revision 1)")));

        let summary = "Checkpointed every 500 rows; resumed import passes the crash test.";
        let (error, done) = tool_json(
            "gitpulse_complete_task",
            json!({"repo_path": repo, "task_id": id, "summary": summary, "expected_revision": 1}),
        );
        assert!(!error, "{done}");
        assert_eq!(done["outcome"], "updated");
        assert_eq!(done["previous_status"], "in_progress");
        assert_eq!(done["status"], "done");
        assert_eq!(done["summary_recorded"], true);

        // What the person sees: the card is Done, and nothing else moved.
        let after = board
            .board_request("items.get", &json!({"id": id}).to_string())
            .unwrap()["item"]
            .clone();
        assert_eq!(after["status"], "done");
        assert_eq!(after["revision"], 2);
        for key in [
            "title",
            "description",
            "priority",
            "severity",
            "owner",
            "labels",
            "acceptance_criteria",
            "repository_ids",
            "primary_repository_id",
            "position",
            "kind",
        ] {
            assert_eq!(after[key], before[key], "{key} changed");
        }
        let logs = after["logs"].as_str().unwrap();
        assert!(
            logs.starts_with("first trace\n\n--- Agent moved this task to done ("),
            "{logs}"
        );
        assert!(logs.ends_with(summary), "{logs}");

        // A retry changes nothing and records nothing twice.
        let (error, again) = tool_json(
            "gitpulse_complete_task",
            json!({"repo_path": repo, "task_id": id, "summary": summary}),
        );
        assert!(!error, "{again}");
        assert_eq!(again["outcome"], "unchanged");
        let replayed = board
            .board_request("items.get", &json!({"id": id}).to_string())
            .unwrap()["item"]
            .clone();
        assert_eq!(replayed["revision"], 2);
        assert_eq!(
            replayed["logs"].as_str().unwrap().matches(summary).count(),
            1
        );

        // Done is the person's to undo, not the agent's.
        let (error, reopened) = tool_json(
            "gitpulse_complete_task",
            json!({"repo_path": repo, "task_id": id, "status": "review"}),
        );
        assert!(
            error && reopened.as_str().unwrap().contains("already_done"),
            "{reopened}"
        );
    }

    /// A task launched while its checkout was busy runs in a worktree of its
    /// own, so the agent's `repo_path` is that linked worktree, not the
    /// checkout the board registered. It is still the same repository.
    #[test]
    fn an_agent_in_its_own_worktree_marks_the_task_of_its_repository_done() {
        let (dir, profile, repo) = task_fixture();
        crate::test_support::trust_repo(dir.path());
        crate::test_support::git_in(dir.path(), &["commit", "-q", "--allow-empty", "-m", "base"]);
        let id = "0b7d4e2a-91c3-4f6e-8a2d-5c4b3a291e70";
        let (board, _) = board_task(profile.path(), &repo, id);
        let lane = dir.path().join(".gitpulse/worktrees/import-0b7d4e2a");
        crate::test_support::git_in(
            dir.path(),
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "gitpulse/import-0b7d4e2a",
                lane.to_str().unwrap(),
            ],
        );

        let (error, done) = tool_json(
            "gitpulse_complete_task",
            json!({"repo_path": lane.to_str().unwrap(), "task_id": id, "summary": "Done in the lane."}),
        );
        assert!(!error, "{done}");
        assert_eq!(done["status"], "done");
        let after = board
            .board_request("items.get", &json!({"id": id}).to_string())
            .unwrap()["item"]
            .clone();
        assert_eq!(after["status"], "done");
    }

    #[test]
    fn completion_respects_a_changed_task_and_refuses_everything_it_should() {
        let (dir, profile, repo) = task_fixture();
        crate::test_support::trust_repo(dir.path());
        let id = "board-task-1";
        let (board, _) = board_task(profile.path(), &repo, id);
        // The person edited it after the agent read revision 1.
        let current = board
            .board_request("items.get", &json!({"id": id}).to_string())
            .unwrap()["item"]
            .clone();
        let mut edit = current.clone();
        for key in ["revision", "updated_at", "created_at", "locked_fields"] {
            edit.as_object_mut().unwrap().remove(key);
        }
        edit["request_id"] = json!("person-edit");
        edit["expected_revision"] = json!(1);
        edit["title"] = json!("Make the importer resumable and fast");
        board.board_request("items.put", &edit.to_string()).unwrap();

        let (error, stale) = tool_json(
            "gitpulse_complete_task",
            json!({"repo_path": repo, "task_id": id, "expected_revision": 1}),
        );
        assert!(
            error && stale.as_str().unwrap().contains("revision_conflict"),
            "{stale}"
        );
        let unchanged = board
            .board_request("items.get", &json!({"id": id}).to_string())
            .unwrap()["item"]
            .clone();
        assert_eq!(unchanged["status"], "in_progress");

        // Without a pinned revision the move goes ahead on top of their edit.
        let (error, moved) = tool_json(
            "gitpulse_complete_task",
            json!({"repo_path": repo, "task_id": id, "status": "review"}),
        );
        assert!(!error, "{moved}");
        let saved = board
            .board_request("items.get", &json!({"id": id}).to_string())
            .unwrap()["item"]
            .clone();
        assert_eq!(saved["status"], "review");
        assert_eq!(
            saved["title"], "Make the importer resumable and fast",
            "the person's edit survives"
        );
        assert_eq!(saved["logs"], "first trace", "no summary, no log entry");

        for (arguments, expected) in [
            (
                json!({"repo_path": repo, "task_id": id, "status": "inbox"}),
                "status",
            ),
            (
                json!({"repo_path": repo, "task_id": "no-such-task"}),
                "not_found",
            ),
            (
                json!({"repo_path": repo, "task_id": id, "summary": "x".repeat(4001)}),
                "4000",
            ),
        ] {
            let (error, message) = tool_json("gitpulse_complete_task", arguments);
            assert!(error, "{message}");
            assert!(
                message.to_string().contains(expected),
                "{expected}: {message}"
            );
        }
        let (_, final_state) = tool_json(
            "gitpulse_get_task",
            json!({"repo_path": repo, "task_id": id}),
        );
        assert_eq!(final_state["task"]["status"], "review");
    }

    #[test]
    fn completion_never_reaches_a_task_of_another_repository() {
        let (dir, profile, repo) = task_fixture();
        crate::test_support::trust_repo(dir.path());
        let other = tempfile::tempdir().unwrap();
        let other_repo = other.path().to_string_lossy().into_owned();
        assert!(std::process::Command::new("git")
            .args(["init", "-q", &other_repo])
            .status()
            .unwrap()
            .success());
        crate::test_support::trust_repo(other.path());
        let (board, _) = board_task(profile.path(), &other_repo, "other-task");
        // This repository needs a board record to be judged at all.
        board.board_register(&repo, "mine", "mine-request").unwrap();
        let (error, message) = tool_json(
            "gitpulse_complete_task",
            json!({"repo_path": repo, "task_id": "other-task"}),
        );
        assert!(
            error && message.as_str().unwrap().contains("not_found"),
            "{message}"
        );
        assert_eq!(
            board
                .board_request("items.get", r#"{"id":"other-task"}"#)
                .unwrap()["item"]["status"],
            "in_progress"
        );
    }

    /// Every revision the store recorded for an item, deleted or not.
    fn item_history(board: &crate::workbench::WorkbenchState, id: &str) -> Vec<Value> {
        board
            .board_request("items.history", &json!({"id": id}).to_string())
            .unwrap()["items"]
            .as_array()
            .unwrap()
            .clone()
    }

    #[test]
    fn a_filed_task_deleted_by_its_task_id_leaves_the_board_and_stays_deleted() {
        let (dir, profile, repo) = task_fixture();
        crate::test_support::trust_repo(dir.path());
        let (error, added) = tool_json(
            "gitpulse_add_task",
            json!({ "repo_path": repo, "task_id": "gp-dup-login", "title": "Fix login (duplicate)", "status": "done" }),
        );
        assert!(!error, "{added}");
        let item_id = added["item_id"].as_str().unwrap().to_owned();
        let (error, _) = tool_json(
            "gitpulse_add_task",
            json!({ "repo_path": repo, "task_id": "gp-keep", "title": "Keep me" }),
        );
        assert!(!error);

        let reason = "MERGED, NOT COMPLETED: folded into gp-login-overhaul.";
        let (error, deleted) = tool_json(
            "gitpulse_delete_task",
            json!({ "repo_path": repo, "task_id": "gp-dup-login", "reason": reason }),
        );
        assert!(!error, "{deleted}");
        assert_eq!(deleted["outcome"], "deleted");
        assert_eq!(deleted["item_id"], item_id.as_str());
        assert_eq!(deleted["deleted"], true);
        assert_eq!(deleted["reason_recorded"], true);
        assert_eq!(deleted["status"], "done");
        // One write for the reason, one for the delete.
        assert_eq!(deleted["revision"], 3);
        assert!(deleted["sequence"].as_u64().is_some(), "{deleted}");

        // The board no longer lists it; its sibling is untouched.
        let (error, listed) = tool_json("gitpulse_list_tasks", json!({ "repo_path": repo }));
        assert!(!error, "{listed}");
        let titles: Vec<&str> = listed["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["title"].as_str().unwrap())
            .collect();
        assert_eq!(titles, vec!["Keep me"]);
        assert_eq!(listed["total"], 1);
        let (error, gone) = tool_json(
            "gitpulse_get_task",
            json!({ "repo_path": repo, "task_id": "gp-dup-login" }),
        );
        assert!(
            error && gone.as_str().unwrap().contains("not_found"),
            "{gone}"
        );

        // The reason is in the history, on the deleted revision itself.
        let board =
            crate::workbench::WorkbenchState::for_profile(&profile.path().join("workbench.sqlite"));
        let history = item_history(&board, &item_id);
        let last = history.last().unwrap();
        assert_eq!(last["deleted"], true);
        assert_eq!(last["revision"], 3);
        let logs = last["logs"].as_str().unwrap();
        assert!(
            logs.starts_with("--- Agent is deleting this task ("),
            "{logs}"
        );
        assert!(logs.ends_with(reason), "{logs}");

        // A second delete changes nothing and records nothing twice.
        let (error, again) = tool_json(
            "gitpulse_delete_task",
            json!({ "repo_path": repo, "task_id": item_id, "reason": reason }),
        );
        assert!(!error, "{again}");
        assert_eq!(again["outcome"], "unchanged");
        assert_eq!(again["revision"], 3);
        assert_eq!(again["reason_recorded"], true);
        assert_eq!(item_history(&board, &item_id).len(), 3);

        // The id is never handed out again.
        let (error, refiled) = tool_json(
            "gitpulse_add_task",
            json!({ "repo_path": repo, "task_id": "gp-dup-login", "title": "Fix login again" }),
        );
        assert!(
            error && refiled.as_str().unwrap().contains("deleted_on_board"),
            "{refiled}"
        );
        let (error, overwritten) = tool_json(
            "gitpulse_add_task",
            json!({ "repo_path": repo, "task_id": "gp-dup-login", "title": "Fix login again", "overwrite": true }),
        );
        assert!(
            error && overwritten.as_str().unwrap().contains("deleted_on_board"),
            "{overwritten}"
        );
        assert_eq!(item_history(&board, &item_id).len(), 3);
    }

    #[test]
    fn a_board_task_deleted_by_its_item_id_honours_the_revision_it_was_read_at() {
        let (dir, profile, repo) = task_fixture();
        crate::test_support::trust_repo(dir.path());
        let id = "board-dup-7";
        let (board, _) = board_task(profile.path(), &repo, id);
        let before = item_history(&board, id);

        // Read at revision 1, then the person edits it.
        let mut edit = board
            .board_request("items.get", &json!({"id": id}).to_string())
            .unwrap()["item"]
            .clone();
        for key in ["revision", "updated_at", "created_at", "locked_fields"] {
            edit.as_object_mut().unwrap().remove(key);
        }
        edit["request_id"] = json!("person-edit");
        edit["expected_revision"] = json!(1);
        edit["title"] = json!("Make the importer resumable (kept)");
        board.board_request("items.put", &edit.to_string()).unwrap();

        let (error, stale) = tool_json(
            "gitpulse_delete_task",
            json!({ "repo_path": repo, "task_id": id, "reason": "Duplicate.", "expected_revision": 1 }),
        );
        assert!(
            error && stale.as_str().unwrap().contains("revision_conflict"),
            "{stale}"
        );
        // Refused before anything was written: no reason block, no delete.
        let current = board
            .board_request("items.get", &json!({"id": id}).to_string())
            .unwrap()["item"]
            .clone();
        assert_eq!(current["revision"], 2);
        assert_eq!(current["logs"], "first trace");
        assert_eq!(item_history(&board, id).len(), before.len() + 1);

        // Re-read at revision 2, the delete goes through.
        let (error, deleted) = tool_json(
            "gitpulse_delete_task",
            json!({ "repo_path": repo, "task_id": id, "reason": "Duplicate of board-1.", "expected_revision": 2 }),
        );
        assert!(!error, "{deleted}");
        assert_eq!(deleted["outcome"], "deleted");
        assert_eq!(deleted["item_id"], id);
        assert_eq!(deleted["title"], "Make the importer resumable (kept)");
        assert_eq!(deleted["revision"], 4);
        let err = board
            .board_request("items.get", &json!({"id": id}).to_string())
            .unwrap_err();
        assert_eq!(err.code, "not_found");
        let last = item_history(&board, id).last().unwrap().clone();
        assert_eq!(last["deleted"], true);
        let logs = last["logs"].as_str().unwrap();
        assert!(
            logs.starts_with("first trace\n\n--- Agent is deleting this task ("),
            "{logs}"
        );
        assert!(logs.ends_with("Duplicate of board-1."), "{logs}");
    }

    #[test]
    fn a_reason_left_by_a_refused_delete_is_not_recorded_twice() {
        let (dir, profile, repo) = task_fixture();
        crate::test_support::trust_repo(dir.path());
        let id = "board-dup-8";
        let (board, _) = board_task(profile.path(), &repo, id);
        // A previous attempt recorded its reason and was then refused, so the
        // task is still live and already carries the block.
        let reason = "Merged into board-1.";
        let mut edit = board
            .board_request("items.get", &json!({"id": id}).to_string())
            .unwrap()["item"]
            .clone();
        for key in ["revision", "updated_at", "created_at", "locked_fields"] {
            edit.as_object_mut().unwrap().remove(key);
        }
        edit["request_id"] = json!("earlier-attempt");
        edit["expected_revision"] = json!(1);
        edit["logs"] = json!(format!(
            "first trace\n\n--- Agent is deleting this task (2026-10-06T00:00:00Z) ---\n{reason}"
        ));
        board.board_request("items.put", &edit.to_string()).unwrap();

        let (error, deleted) = tool_json(
            "gitpulse_delete_task",
            json!({ "repo_path": repo, "task_id": id, "reason": reason, "expected_revision": 2 }),
        );
        assert!(!error, "{deleted}");
        assert_eq!(deleted["outcome"], "deleted");
        // Straight to the delete: no second reason write.
        assert_eq!(deleted["revision"], 3);
        let last = item_history(&board, id).last().unwrap().clone();
        assert_eq!(last["logs"].as_str().unwrap().matches(reason).count(), 1);
    }

    #[test]
    fn deletion_never_reaches_a_task_of_another_repository_or_a_shared_one() {
        let (dir, profile, repo) = task_fixture();
        crate::test_support::trust_repo(dir.path());
        let other = tempfile::tempdir().unwrap();
        let other_repo = other.path().to_string_lossy().into_owned();
        assert!(std::process::Command::new("git")
            .args(["init", "-q", &other_repo])
            .status()
            .unwrap()
            .success());
        crate::test_support::trust_repo(other.path());
        let (board, other_id) = board_task(profile.path(), &other_repo, "other-task");
        let mine = board.board_register(&repo, "mine", "mine-request").unwrap();
        let mine_id = mine["repository"]["id"].as_str().unwrap().to_owned();

        let (error, message) = tool_json(
            "gitpulse_delete_task",
            json!({"repo_path": repo, "task_id": "other-task", "reason": "Not mine."}),
        );
        assert!(
            error && message.as_str().unwrap().contains("not_found"),
            "{message}"
        );
        let survivor = board
            .board_request("items.get", r#"{"id":"other-task"}"#)
            .unwrap()["item"]
            .clone();
        assert_eq!(survivor["revision"], 1);
        assert_eq!(survivor["logs"], "first trace");

        // Linked to both: deleting it here would delete it there too.
        board
            .board_request(
                "items.put",
                &json!({
                    "id": "shared-task", "request_id": "put-shared", "expected_revision": 0,
                    "title": "Shared", "status": "ready",
                    "repository_ids": [mine_id, other_id], "primary_repository_id": mine_id,
                })
                .to_string(),
            )
            .unwrap();
        let (error, message) = tool_json(
            "gitpulse_delete_task",
            json!({"repo_path": repo, "task_id": "shared-task", "reason": "Duplicate."}),
        );
        assert!(
            error && message.as_str().unwrap().contains("shared_task"),
            "{message}"
        );
        assert_eq!(
            board
                .board_request("items.get", r#"{"id":"shared-task"}"#)
                .unwrap()["item"]["revision"],
            1
        );

        for (arguments, expected) in [
            (
                json!({"repo_path": repo, "task_id": "shared-task", "reason": "   "}),
                "reason is required",
            ),
            (
                json!({"repo_path": repo, "task_id": "shared-task", "reason": "x".repeat(1001)}),
                "1000",
            ),
            (
                json!({"repo_path": repo, "task_id": "no-such-task", "reason": "Gone."}),
                "not_found",
            ),
        ] {
            let (error, message) = tool_json("gitpulse_delete_task", arguments);
            assert!(error, "{message}");
            assert!(
                message.to_string().contains(expected),
                "{expected}: {message}"
            );
        }
    }

    /// The defect this gate answers: agents filed one task per finding — 39 on
    /// one board, 31 of which were later merged away — because nothing made
    /// them read the board before adding to it.
    #[test]
    fn a_task_like_open_work_is_refused_until_it_is_folded_in_or_reviewed() {
        let (dir, _profile, repo) = task_fixture();
        crate::test_support::trust_repo(dir.path());
        // A finished task never gates; an unrelated one is listed nowhere.
        let (error, done) = tool_json(
            "gitpulse_add_task",
            json!({ "repo_path": repo, "task_id": "dm-old", "title": "DevMap receiver attribution for method calls, first pass", "status": "done" }),
        );
        assert!(!error, "{done}");
        assert_eq!(
            done["related_check"],
            Value::Null,
            "the repository's first task has nothing to be checked against"
        );
        let (error, first) = tool_json(
            "gitpulse_add_task",
            json!({ "repo_path": repo, "task_id": "dm-crate-calls", "title": "DevMap: Rust calls into another crate get no caller edge", "labels": ["devmap", "resolver"] }),
        );
        assert!(!error, "{first}");
        assert_eq!(
            first["related_check"]["related_open_tasks"], 0,
            "done work is not open work"
        );
        let first_id = first["item_id"].as_str().unwrap().to_owned();
        let (error, unrelated) = tool_json(
            "gitpulse_add_task",
            json!({ "repo_path": repo, "task_id": "ci-windows", "title": "No Windows CI job runs the Go host tests" }),
        );
        assert!(!error, "{unrelated}");
        assert_eq!(unrelated["related_check"]["related_open_tasks"], 0);
        assert_eq!(unrelated["related_check"]["scan_complete"], true);
        assert_eq!(
            unrelated["related_check"]["open_tasks_compared"], 1,
            "the done task is not compared"
        );
        // A shared subsystem prefix that is also the shared label is one
        // coincidence, not two: every DevMap card would otherwise gate.
        let (error, corpus) = tool_json(
            "gitpulse_add_task",
            json!({ "repo_path": repo, "task_id": "dm-corpus", "title": "DevMap: precision corpus is too thin", "labels": ["devmap"] }),
        );
        assert!(!error, "{corpus}");
        assert_eq!(corpus["related_check"]["related_open_tasks"], 0);

        // The same kind of finding again: refused, and told where it belongs.
        let second = json!({ "repo_path": repo, "task_id": "dm-receiver", "title": "DevMap: method calls on a typed receiver get no caller edge", "labels": ["devmap"] });
        let (error, refused) = tool_json("gitpulse_add_task", second.clone());
        assert!(error, "{refused}");
        let message = refused.as_str().unwrap();
        assert!(message.starts_with("related_tasks_exist"), "{message}");
        assert!(message.contains(&first_id), "{message}");
        assert!(!message.contains("Windows"), "{message}");
        assert!(message.contains("overwrite: true"), "{message}");
        let (_, listed) = tool_json("gitpulse_list_tasks", json!({ "repo_path": repo }));
        assert_eq!(listed["total"], 4, "a refusal files nothing");

        // Reviewing something else does not count.
        let mut wrong = second.clone();
        wrong["reviewed_related"] = json!(["ft-0000"]);
        let (error, still) = tool_json("gitpulse_add_task", wrong);
        assert!(
            error && still.as_str().unwrap().starts_with("related_tasks_exist"),
            "{still}"
        );

        // Folding it in: the existing task, by its item_id, takes both.
        let (error, folded) = tool_json(
            "gitpulse_add_task",
            json!({
                "repo_path": repo, "task_id": first_id, "overwrite": true,
                "title": "DevMap: cross-crate and typed-receiver calls get no caller edge",
                "labels": ["devmap", "resolver"],
                "acceptance_criteria": ["`other_crate::f` gets an edge", "`recv.method()` on a typed receiver gets an edge"],
            }),
        );
        assert!(!error, "{folded}");
        assert_eq!(folded["outcome"], "updated");
        assert_eq!(folded["item_id"], first_id.as_str());
        assert_eq!(
            folded["related_check"],
            Value::Null,
            "an overwrite is never gated"
        );
        let (_, listed) = tool_json("gitpulse_list_tasks", json!({ "repo_path": repo }));
        assert_eq!(listed["total"], 4, "folded, not added");

        // Or, having read it and judged it separate, filed with the review named.
        let mut reviewed = second;
        reviewed["reviewed_related"] = json!([first_id]);
        let (error, filed) = tool_json("gitpulse_add_task", reviewed);
        assert!(!error, "{filed}");
        assert_eq!(filed["outcome"], "created");
        assert_eq!(filed["related_check"]["related_open_tasks"], 1);
    }

    #[test]
    fn related_work_folds_into_a_task_the_person_made_on_the_board() {
        let (dir, profile, repo) = task_fixture();
        crate::test_support::trust_repo(dir.path());
        let id = "6a1d0c2e-board-made";
        let (board, _) = board_task(profile.path(), &repo, id);
        // Every related task must be named; naming one of two is not enough.
        let (error, _) = tool_json(
            "gitpulse_add_task",
            json!({ "repo_path": repo, "task_id": "imp-crash", "title": "Importer loses rows after a crash", "labels": ["import"], "reviewed_related": [id] }),
        );
        assert!(!error);
        let (error, refused) = tool_json(
            "gitpulse_add_task",
            json!({ "repo_path": repo, "task_id": "imp-resume", "title": "Importer should be resumable after a crash", "labels": ["import"], "reviewed_related": [id] }),
        );
        assert!(error, "{refused}");
        let message = refused.as_str().unwrap();
        assert!(message.contains("1 open task "), "{message}");
        assert!(!message.contains(id), "{message}");

        let (error, folded) = tool_json(
            "gitpulse_add_task",
            json!({
                "repo_path": repo, "task_id": id, "overwrite": true,
                "title": "Make the importer resumable", "description": "Background\n\nAlso: resume must not re-import committed rows.",
                "acceptance_criteria": ["Resumes after a crash", "Never re-imports a committed row"],
            }),
        );
        assert!(!error, "{folded}");
        assert_eq!(folded["outcome"], "updated");
        let item = board
            .board_request("items.get", &json!({"id": id}).to_string())
            .unwrap()["item"]
            .clone();
        assert!(item["description"].as_str().unwrap().contains("re-import"));
        assert_eq!(
            item["acceptance_criteria"],
            json!(["Resumes after a crash", "Never re-imports a committed row"])
        );
        assert_eq!(item["position"], 7, "where it sits on the board is kept");
        assert_eq!(item["revision"], 2);
        // What the agent did not send is the person's, and stays as they left it.
        assert_eq!(item["status"], "in_progress");
        assert_eq!(item["priority"], 1);
        assert_eq!(item["severity"], "high");
        assert_eq!(item["owner"], "@sam");
        assert_eq!(item["labels"], json!(["import"]));
        assert_eq!(item["logs"], "first trace");
    }

    #[test]
    fn task_writes_are_refused_for_an_untrusted_repository() {
        let (_dir, profile, repo) = task_fixture();
        let (error, message) = tool_json(
            "gitpulse_add_task",
            json!({ "repo_path": repo, "title": "Injected" }),
        );
        assert!(
            error && message.as_str().unwrap().contains("untrusted_repository"),
            "{message}"
        );
        let (error, message) = tool_json("gitpulse_import_tasks", json!({ "repo_path": repo }));
        assert!(
            error && message.as_str().unwrap().contains("untrusted_repository"),
            "{message}"
        );
        let (error, message) = tool_json(
            "gitpulse_complete_task",
            json!({ "repo_path": repo, "task_id": "anything" }),
        );
        assert!(
            error && message.as_str().unwrap().contains("untrusted_repository"),
            "{message}"
        );
        let (error, message) = tool_json(
            "gitpulse_delete_task",
            json!({ "repo_path": repo, "task_id": "anything", "reason": "Duplicate." }),
        );
        assert!(
            error && message.as_str().unwrap().contains("untrusted_repository"),
            "{message}"
        );
        // Refused before anything was registered under the repository.
        let store = dc_store::Store::open(profile.path().join("workbench.sqlite")).unwrap();
        let repos: Value =
            serde_json::from_str(&store.workbench_request("repositories.list", "{}").unwrap())
                .unwrap();
        assert_eq!(repos["total"], 0);
    }

    #[test]
    fn reads_of_a_machine_with_no_board_create_nothing() {
        let (_dir, profile, repo) = task_fixture();
        let (error, listed) = tool_json("gitpulse_list_tasks", json!({ "repo_path": repo }));
        assert!(!error);
        assert_eq!(listed["repository"], Value::Null);
        assert!(listed["note"].as_str().unwrap().contains("not the same as"));
        let (error, _) = tool_json(
            "gitpulse_get_task",
            json!({ "repo_path": repo, "task_id": "gp-x" }),
        );
        assert!(error);
        assert!(!profile.path().join("workbench.sqlite").exists());
    }

    #[test]
    fn imported_briefs_reach_the_board_and_every_file_is_accounted_for() {
        let (dir, profile, repo) = task_fixture();
        crate::test_support::trust_repo(dir.path());
        let tasks = dir.path().join("tasks");
        std::fs::create_dir(&tasks).unwrap();
        std::fs::write(
            tasks.join("gp-a.md"),
            "---\nid: gp-a\ntitle: Alpha\nstatus: ready\npriority: 1\n---\n",
        )
        .unwrap();
        std::fs::write(
            tasks.join("gp-b.md"),
            "---\nid: gp-b\ntitle: Beta\nstatus: backlog\npriority: 0\n---\n",
        )
        .unwrap();
        std::fs::write(
            tasks.join("broken.md"),
            "---\ntitle: Broken\nstatus: someday\n---\n",
        )
        .unwrap();

        let (error, report) = tool_json("gitpulse_import_tasks", json!({ "repo_path": repo }));
        assert!(!error, "{report}");
        assert_eq!(
            report["ok"], false,
            "one brief is invalid, so not everything was placed"
        );
        assert_eq!(
            (
                report["found"].as_u64(),
                report["created"].as_u64(),
                report["invalid"].as_u64()
            ),
            (Some(3), Some(2), Some(1))
        );
        let broken = report["entries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["file"] == "tasks/broken.md")
            .unwrap();
        assert!(broken["reason"]
            .as_str()
            .unwrap()
            .contains("unknown status"));

        let (_, again) = tool_json("gitpulse_import_tasks", json!({ "repo_path": repo }));
        assert_eq!(
            (again["created"].as_u64(), again["already_present"].as_u64()),
            (Some(0), Some(2)),
            "{again}"
        );

        let board =
            crate::workbench::WorkbenchState::for_profile(&profile.path().join("workbench.sqlite"));
        let repository_id = board
            .board_register(&repo, "board-id", "board-req")
            .unwrap()["repository"]["id"]
            .as_str()
            .unwrap()
            .to_string();
        let page = board
            .board_request(
                "items.list",
                &json!({"repository_id": repository_id, "limit": 50}).to_string(),
            )
            .unwrap();
        assert_eq!(page["total"], 2);
    }
}
